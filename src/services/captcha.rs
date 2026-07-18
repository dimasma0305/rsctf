//! services/captcha.rs — ported from RSCTF `CaptchaService.cs`.
//!
//! Selects a captcha provider from the `RSCTF_CAPTCHA_PROVIDER` env var:
//!   * `none`      -> verification always succeeds (default).
//!   * `turnstile` -> Cloudflare Turnstile siteverify.
//!   * `hashpow`   -> local proof-of-work (sha256 leading-zero-bits challenge).
//!
//! Exposes [`CaptchaService::from_env`] to build the configured provider and
//! [`CaptchaService::verify`] to check a client-supplied token.
//!
//! [`CaptchaSettings::load`] resolves the LIVE captcha policy from the `Configs`
//! key/value table (the `CaptchaConfig:*` keys `/admin/settings` persists, plus
//! the `AccountPolicy:UseCaptcha` enforcement toggle) so the admin toggle takes
//! effect without a restart, mirroring RSCTF's `IOptionsSnapshot<CaptchaConfig>`.
//! It is the single source the verify path (login/register/recovery) and the
//! client-facing endpoints (`GET /api/captcha`, `/api/captcha/powchallenge`)
//! share, so provider/difficulty/site-key can never drift between them.

use std::sync::LazyLock;
use std::time::Duration;

use sea_orm::{DatabaseConnection, EntityTrait};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::services::cache::Cache;

use crate::models::data::config;
use crate::utils::error::{AppError, AppResult};

/// Cloudflare Turnstile siteverify endpoint.
const TURNSTILE_API: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const TURNSTILE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TURNSTILE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

static TURNSTILE_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    turnstile_client_builder(TURNSTILE_CONNECT_TIMEOUT, TURNSTILE_REQUEST_TIMEOUT)
        .build()
        .expect("failed to build Turnstile HTTP client")
});

fn turnstile_client_builder(
    connect_timeout: Duration,
    request_timeout: Duration,
) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
}

pub(crate) fn turnstile_client() -> reqwest::Client {
    TURNSTILE_CLIENT.clone()
}

/// Default proof-of-work difficulty (leading zero bits) when
/// `RSCTF_HASHPOW_DIFFICULTY` is unset or unparseable. Matches RSCTF's
/// `HashPowConfig.Difficulty` default of 18 (≈262K hashes ≈ a couple seconds in
/// a browser; each extra bit *doubles* the work, so this must stay modest).
const DEFAULT_HASHPOW_DIFFICULTY: u32 = 18;

/// RSCTF `HashPowConfig.Difficulty` clamp (`Math.Clamp(_difficulty, 8, 48)`).
const HASHPOW_DIFFICULTY_MIN: u32 = 8;
const HASHPOW_DIFFICULTY_MAX: u32 = 48;

/// The configured captcha provider.
#[derive(Debug, Clone)]
pub enum CaptchaService {
    /// No captcha: verification always succeeds.
    None,
    /// Cloudflare Turnstile, verified against the siteverify API.
    Turnstile {
        secret: String,
        client: reqwest::Client,
    },
    /// Local hash proof-of-work: `sha256(challenge || nonce)` must have at
    /// least `difficulty` leading zero bits.
    HashPow { difficulty: u32 },
}

/// Shape of the Turnstile siteverify response we care about.
#[derive(Debug, Deserialize)]
struct TurnstileResponse {
    #[serde(default)]
    success: bool,
}

impl CaptchaService {
    /// Build the captcha service from the process environment.
    ///
    /// Reads `RSCTF_CAPTCHA_PROVIDER` (`none` | `turnstile` | `hashpow`);
    /// any unrecognized/absent value falls back to `none`.
    pub fn from_env() -> Self {
        let provider = std::env::var("RSCTF_CAPTCHA_PROVIDER")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();

        match provider.as_str() {
            "turnstile" => {
                let secret = std::env::var("RSCTF_TURNSTILE_SECRET").unwrap_or_default();
                CaptchaService::Turnstile {
                    secret,
                    client: turnstile_client(),
                }
            }
            "hashpow" => {
                let difficulty = std::env::var("RSCTF_HASHPOW_DIFFICULTY")
                    .ok()
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(DEFAULT_HASHPOW_DIFFICULTY);
                CaptchaService::HashPow { difficulty }
            }
            // "none" or anything unrecognized -> disabled.
            _ => CaptchaService::None,
        }
    }

    /// Verify a client-supplied captcha token.
    ///
    /// * `None`      -> always `Ok(true)`.
    /// * `Turnstile` -> POSTs the token to Cloudflare and returns `success`.
    ///   An empty secret disables verification (mirrors RSCTF), returning
    ///   `Ok(true)`.
    /// * `HashPow`   -> the token is `"<challenge>:<nonce>"`; returns whether
    ///   `sha256(challenge || nonce)` has at least `difficulty` leading zero
    ///   bits.
    pub async fn verify(&self, token: &str, cache: &dyn Cache) -> AppResult<bool> {
        match self {
            CaptchaService::None => Ok(true),

            CaptchaService::Turnstile { secret, client } => {
                // No secret configured -> treat as disabled (RSCTF behavior).
                if secret.trim().is_empty() {
                    return Ok(true);
                }
                if token.trim().is_empty() {
                    return Ok(false);
                }

                let params = [("secret", secret.as_str()), ("response", token)];
                let resp = client
                    .post(TURNSTILE_API)
                    .form(&params)
                    .send()
                    .await
                    .map_err(|e| AppError::internal(format!("turnstile request failed: {e}")))?;

                let body: TurnstileResponse = resp.json().await.map_err(|e| {
                    AppError::internal(format!("turnstile response decode failed: {e}"))
                })?;

                Ok(body.success)
            }

            CaptchaService::HashPow { difficulty } => {
                Ok(verify_hashpow(token, *difficulty, cache).await)
            }
        }
    }
}

/// Verify a proof-of-work token of the form `"<id>:<answer>"` against the
/// challenge value the server minted in [`get_pow_challenge`] and cached under
/// `_HP_{id}` (single-use).
///
/// This matches the client worker (`web/src/utils/PowWorker.ts`) exactly:
/// the browser hashes `SHA-256(hex_decode(challenge_value) ‖ salt ‖ nonce)` and
/// returns `answer = hex(salt) ‖ hex(nonce)` (16 hex chars). So server-side the
/// pre-image is `hex_decode(challenge_value) ‖ hex_decode(answer)`, and the token
/// passes iff its SHA-256 has ≥ `difficulty` leading zero bits. The `_HP_{id}`
/// key is consumed on every attempt so a solved nonce can't be replayed.
async fn verify_hashpow(token: &str, difficulty: u32, cache: &dyn Cache) -> bool {
    let mut parts = token.splitn(2, ':');
    let (id, answer) = match (parts.next(), parts.next()) {
        (Some(i), Some(a)) if !i.is_empty() && !a.is_empty() => (i, a),
        _ => return false,
    };

    let key = format!("_HP_{id}");
    let Some(value) = cache.get_and_remove(&key).await else {
        return false; // expired, unknown, or already consumed
    };

    let (Some(value_bytes), Some(answer_bytes)) = (
        hex_bytes(std::str::from_utf8(&value).unwrap_or_default()),
        hex_bytes(answer),
    ) else {
        return false;
    };

    let mut hasher = Sha256::new();
    hasher.update(&value_bytes);
    hasher.update(&answer_bytes);
    leading_zero_bits(&hasher.finalize()) >= difficulty
}

/// Decode an even-length lowercase/uppercase hex string to bytes; `None` on any
/// malformed input.
fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Count the number of leading zero bits in a byte slice (most-significant
/// bit of the first byte first).
fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    for &b in bytes {
        if b == 0 {
            count += 8;
        } else {
            count += b.leading_zeros(); // u8::leading_zeros: within 8-bit width
            break;
        }
    }
    count
}

/// The live captcha policy resolved from the `Configs` table. A single loader so
/// the verify path and the client-facing captcha endpoints read the SAME source
/// (RSCTF resolves both through one `IOptionsSnapshot<CaptchaConfig>`).
#[derive(Debug, Clone)]
pub struct CaptchaSettings {
    /// `AccountPolicy:UseCaptcha` — whether verification is enforced at all.
    pub use_captcha: bool,
    /// Canonical provider name — `"None"` | `"HashPow"` | `"CloudflareTurnstile"`
    /// (the `CaptchaProvider` wire enum the client's `GET /api/captcha` expects).
    pub provider: String,
    /// Turnstile site key surfaced to the client (`None` for other providers).
    pub site_key: Option<String>,
    /// HashPow leading-zero-bit difficulty — used by both the issued PoW challenge
    /// and the verify step so the client solves what the server checks.
    pub difficulty: u32,
    /// Turnstile secret (verify-side only; never surfaced to the client).
    secret_key: Option<String>,
}

impl CaptchaSettings {
    /// Resolve the live captcha policy from the `Configs` key/value table (the
    /// `CaptchaConfig:*` keys `/admin/settings` writes) plus the enforcement
    /// toggle `AccountPolicy:UseCaptcha`. When the provider key was never
    /// persisted, fall back to the process-env provider ([`CaptchaService::from_env`])
    /// so an env-only deployment keeps working. Best-effort: a config read error
    /// leaves everything at "captcha off" rather than failing the request.
    pub async fn load(db: &DatabaseConnection) -> Self {
        let mut use_captcha = false;
        let mut provider: Option<String> = None;
        let mut site_key: Option<String> = None;
        let mut secret_key: Option<String> = None;
        let mut difficulty: Option<u32> = None;

        if let Ok(rows) = config::Entity::find().all(db).await {
            for row in rows {
                let Some(value) = row.value else { continue };
                match row.config_key.as_str() {
                    // Persisted as lowercase `bool::to_string()` (admin settings).
                    "AccountPolicy:UseCaptcha" => use_captcha = value == "true",
                    "CaptchaConfig:Provider" if !value.is_empty() => provider = Some(value),
                    "CaptchaConfig:SiteKey" if !value.is_empty() => site_key = Some(value),
                    "CaptchaConfig:SecretKey" if !value.is_empty() => secret_key = Some(value),
                    "CaptchaConfig:HashPow:Difficulty" => {
                        difficulty = value.trim().parse::<u32>().ok();
                    }
                    _ => {}
                }
            }
        }

        // Provider never persisted -> honor the env-configured provider, mapping
        // it onto the canonical `CaptchaProvider` wire names.
        let (provider, env_difficulty, env_secret) = match provider {
            Some(p) => (p, None, None),
            None => match CaptchaService::from_env() {
                CaptchaService::HashPow { difficulty } => {
                    ("HashPow".to_string(), Some(difficulty), None)
                }
                CaptchaService::Turnstile { secret, .. } => {
                    ("CloudflareTurnstile".to_string(), None, Some(secret))
                }
                CaptchaService::None => ("None".to_string(), None, None),
            },
        };

        Self {
            use_captcha,
            provider,
            site_key,
            difficulty: difficulty
                .or(env_difficulty)
                .unwrap_or(DEFAULT_HASHPOW_DIFFICULTY)
                .clamp(HASHPOW_DIFFICULTY_MIN, HASHPOW_DIFFICULTY_MAX),
            secret_key: secret_key.or(env_secret),
        }
    }

    /// Build the verify-side [`CaptchaService`] for the resolved provider.
    pub fn service(&self) -> CaptchaService {
        match self.provider.as_str() {
            "HashPow" => CaptchaService::HashPow {
                difficulty: self.difficulty,
            },
            "CloudflareTurnstile" => CaptchaService::Turnstile {
                secret: self.secret_key.clone().unwrap_or_default(),
                client: turnstile_client(),
            },
            // "None" or any unrecognized provider -> disabled.
            _ => CaptchaService::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cache::{Cache, InMemoryCache};

    #[test]
    fn leading_zeros_counts_bits() {
        assert_eq!(leading_zero_bits(&[0x00, 0x00]), 16);
        assert_eq!(leading_zero_bits(&[0x0f]), 4);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0x00, 0x01]), 15);
    }

    #[test]
    fn hex_bytes_decodes_and_rejects_malformed() {
        assert_eq!(hex_bytes("00ff10"), Some(vec![0x00, 0xff, 0x10]));
        assert_eq!(hex_bytes("abc"), None); // odd length
        assert_eq!(hex_bytes("zz"), None); // non-hex
    }

    #[tokio::test]
    async fn turnstile_client_enforces_total_request_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let client =
            turnstile_client_builder(Duration::from_millis(100), Duration::from_millis(100))
                .no_proxy()
                .build()
                .unwrap();

        let started = tokio::time::Instant::now();
        let err = client
            .post(format!("http://{addr}/siteverify"))
            .body("response=test")
            .send()
            .await
            .unwrap_err();
        assert!(err.is_timeout());
        assert!(started.elapsed() < Duration::from_secs(1));
        server.abort();
    }

    #[tokio::test]
    async fn hashpow_rejects_bad_shape_or_unknown_challenge() {
        let cache = InMemoryCache::default();
        assert!(!verify_hashpow("no-colon", 0, &cache).await); // no ':'
        assert!(!verify_hashpow(":answer", 0, &cache).await); // empty id
        assert!(!verify_hashpow("id:", 0, &cache).await); // empty answer
                                                          // A well-formed token whose id was never minted (no cached challenge) fails.
        assert!(!verify_hashpow("unknownid:00000000", 0, &cache).await);
    }

    #[tokio::test]
    async fn hashpow_verifies_leading_zero_bits_and_is_single_use() {
        // Mirror the server↔client contract: the id keys a cached hex challenge
        // value, and the answer is hex(bytes) whose sha256(value ‖ answer) has the
        // required leading zero bits.
        let id = "deadbeef";
        let value = "0011223344556677"; // 8-byte hex, like get_pow_challenge mints
        let value_bytes = hex_bytes(value).unwrap();

        // Brute-force an answer with >= 8 leading zero bits.
        let mut answer = None;
        for n in 0..5_000_000u32 {
            let mut h = Sha256::new();
            h.update(&value_bytes);
            h.update(n.to_be_bytes());
            if leading_zero_bits(&h.finalize()) >= 8 {
                answer = Some(format!("{n:08x}"));
                break;
            }
        }
        let answer = answer.expect("a <=8-bit nonce exists well within range");
        let token = format!("{id}:{answer}");
        let key = format!("_HP_{id}");

        let cache = InMemoryCache::default();
        cache.set(&key, value.as_bytes(), None).await;
        assert!(verify_hashpow(&token, 8, &cache).await);
        // Single-use: the key was consumed, so a replay fails.
        assert!(!verify_hashpow(&token, 8, &cache).await);

        // A difficulty higher than the solved nonce provides is rejected.
        cache.set(&key, value.as_bytes(), None).await;
        assert!(!verify_hashpow(&token, 64, &cache).await);
    }

    #[tokio::test]
    async fn concurrent_hashpow_replays_have_one_winner() {
        let id = "concurrent";
        let key = format!("_HP_{id}");
        let token = format!("{id}:00000000");
        let cache = std::sync::Arc::new(InMemoryCache::default());
        cache.set(&key, b"0011223344556677", None).await;
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(17));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            let token = token.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                verify_hashpow(&token, 0, cache.as_ref()).await
            }));
        }
        barrier.wait().await;

        let mut accepted = 0;
        for task in tasks {
            accepted += usize::from(task.await.unwrap());
        }
        assert_eq!(accepted, 1);
    }
}
