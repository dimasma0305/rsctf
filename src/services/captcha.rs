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

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};

use crate::services::cache::Cache;

use crate::utils::error::{AppError, AppResult};

/// Cloudflare Turnstile siteverify endpoint.
const TURNSTILE_API: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const TURNSTILE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TURNSTILE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TURNSTILE_RESPONSE_MAX_BYTES: usize = 16 * 1024;
const MAX_CAPTCHA_TOKEN_BYTES: usize = 4 * 1024;

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

async fn decode_turnstile_response(
    mut response: reqwest::Response,
) -> AppResult<TurnstileResponse> {
    if response
        .content_length()
        .is_some_and(|length| length > TURNSTILE_RESPONSE_MAX_BYTES as u64)
    {
        return Err(AppError::internal("turnstile response is too large"));
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(TURNSTILE_RESPONSE_MAX_BYTES as u64) as usize,
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| AppError::internal(format!("turnstile response read failed: {error}")))?
    {
        if body.len().saturating_add(chunk.len()) > TURNSTILE_RESPONSE_MAX_BYTES {
            return Err(AppError::internal("turnstile response is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|error| AppError::internal(format!("turnstile response decode failed: {error}")))
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
                if token.trim().is_empty() || token.len() > MAX_CAPTCHA_TOKEN_BYTES {
                    return Ok(false);
                }

                let params = [("secret", secret.as_str()), ("response", token)];
                let resp = client
                    .post(TURNSTILE_API)
                    .form(&params)
                    .send()
                    .await
                    .map_err(|e| AppError::internal(format!("turnstile request failed: {e}")))?;

                let body = decode_turnstile_response(resp).await?;

                Ok(body.success)
            }

            CaptchaService::HashPow { difficulty } => Ok(token.len() <= MAX_CAPTCHA_TOKEN_BYTES
                && verify_hashpow(token, *difficulty, cache).await),
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
    hex::decode(s).ok()
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
#[derive(Clone)]
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

/// Opaque digest of every setting that determines whether a locally supplied
/// captcha token is valid. It is request-local and never crosses the wire.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CaptchaRevision([u8; 32]);

/// Provenance of the captcha decision carried into canonical account
/// admission. OAuth is explicit because the provider-authenticated redirect
/// flow has no local captcha field and is intentionally exempt from the local
/// login/registration captcha policy.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CaptchaAdmission {
    Local(Option<CaptchaRevision>),
    OAuthProvider,
}

impl CaptchaSettings {
    /// Resolve the live captcha policy from the `Configs` key/value table (the
    /// `CaptchaConfig:*` keys `/admin/settings` writes) plus the enforcement
    /// toggle `AccountPolicy:UseCaptcha`. When the provider key was never
    /// persisted, fall back to the process-env provider ([`CaptchaService::from_env`])
    /// so an env-only deployment keeps working. Database/configuration errors
    /// fail closed instead of silently turning captcha off.
    pub async fn load(pool: &PgPool, fallback_use_captcha: bool) -> AppResult<Self> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            r#"SELECT config_key, value
                 FROM "Configs"
                WHERE config_key = ANY($1)"#,
        )
        .bind(captcha_config_keys())
        .fetch_all(pool)
        .await
        .map_err(database_error)?;
        Self::from_values(&rows.into_iter().collect(), fallback_use_captcha)
    }

    pub(crate) async fn load_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        fallback_use_captcha: bool,
    ) -> AppResult<Self> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            r#"SELECT config_key, value
                 FROM "Configs"
                WHERE config_key = ANY($1)"#,
        )
        .bind(captcha_config_keys())
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
        Self::from_values(&rows.into_iter().collect(), fallback_use_captcha)
    }

    pub(crate) fn from_values(
        values: &BTreeMap<String, Option<String>>,
        fallback_use_captcha: bool,
    ) -> AppResult<Self> {
        let use_captcha = values
            .get("AccountPolicy:UseCaptcha")
            .and_then(|value| value.as_deref())
            .map(|value| value == "true")
            .unwrap_or(fallback_use_captcha);
        let stored_provider = nonempty_value(&values, "CaptchaConfig:Provider");
        let stored_site_key = nonempty_value(&values, "CaptchaConfig:SiteKey");
        let stored_secret = nonempty_value(&values, "CaptchaConfig:SecretKey");
        let stored_difficulty = values
            .get("CaptchaConfig:HashPow:Difficulty")
            .and_then(|value| value.as_deref())
            .map(|value| {
                value.trim().parse::<u32>().map_err(|_| {
                    AppError::bad_request("HashPow difficulty must be an integer from 8 to 48")
                })
            })
            .transpose()?;

        // Provider never persisted -> honor the env-configured provider, mapping
        // it onto the canonical `CaptchaProvider` wire names.
        let (provider, env_difficulty, env_secret) = match stored_provider {
            Some(provider) => (provider, None, None),
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
        let settings = Self {
            use_captcha,
            provider,
            site_key: stored_site_key,
            difficulty: stored_difficulty
                .or(env_difficulty)
                .unwrap_or(DEFAULT_HASHPOW_DIFFICULTY),
            secret_key: stored_secret.or(env_secret),
        };
        settings.validate()?;
        Ok(settings)
    }

    /// Verify a token under this exact local policy and bind the successful
    /// result to its opaque revision for the later transaction recheck.
    pub async fn verify_local(
        &self,
        token: &str,
        cache: &dyn Cache,
    ) -> AppResult<CaptchaAdmission> {
        if !self.use_captcha {
            return Ok(CaptchaAdmission::Local(None));
        }
        if !self.service().verify(token, cache).await? {
            return Err(AppError::bad_request("Captcha failed"));
        }
        Ok(CaptchaAdmission::Local(Some(self.revision())))
    }

    /// Recheck a preflight decision against the transaction-locked policy.
    /// Weakening to captcha-off is safe; enabling or changing an enabled
    /// provider requires a new token. OAuth remains an explicit provider-auth
    /// exemption rather than an accidental `true` boolean bypass.
    pub fn authorize(&self, admission: CaptchaAdmission) -> AppResult<()> {
        if !self.use_captcha || admission == CaptchaAdmission::OAuthProvider {
            return Ok(());
        }
        if admission == CaptchaAdmission::Local(Some(self.revision())) {
            return Ok(());
        }
        Err(AppError::bad_request(
            "Captcha policy changed; retry with a fresh challenge",
        ))
    }

    pub fn revision(&self) -> CaptchaRevision {
        let mut digest = Sha256::new();
        digest.update(b"rsctf-captcha-policy-v1\0");
        update_revision_field(&mut digest, self.use_captcha.to_string().as_bytes());
        update_revision_field(&mut digest, self.provider.as_bytes());
        update_revision_field(
            &mut digest,
            self.site_key.as_deref().unwrap_or_default().as_bytes(),
        );
        update_revision_field(&mut digest, self.difficulty.to_string().as_bytes());
        update_revision_field(
            &mut digest,
            self.secret_key.as_deref().unwrap_or_default().as_bytes(),
        );
        CaptchaRevision(digest.finalize().into())
    }

    fn validate(&self) -> AppResult<()> {
        match self.provider.as_str() {
            "None" if self.use_captcha => Err(AppError::bad_request(
                "Captcha cannot be enabled with provider None",
            )),
            "HashPow"
                if !(HASHPOW_DIFFICULTY_MIN..=HASHPOW_DIFFICULTY_MAX)
                    .contains(&self.difficulty) =>
            {
                Err(AppError::bad_request(
                    "HashPow difficulty must be an integer from 8 to 48",
                ))
            }
            "CloudflareTurnstile"
                if self.use_captcha
                    && (self
                        .site_key
                        .as_deref()
                        .is_none_or(|site_key| site_key.trim().is_empty())
                        || self
                            .secret_key
                            .as_deref()
                            .is_none_or(|secret| secret.trim().is_empty())) =>
            {
                Err(AppError::bad_request(
                    "Cloudflare Turnstile requires non-empty site and secret keys",
                ))
            }
            "None" | "HashPow" | "CloudflareTurnstile" => Ok(()),
            _ if self.use_captcha => Err(AppError::bad_request("Unknown captcha provider")),
            _ => Ok(()),
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

fn captcha_config_keys() -> &'static [&'static str] {
    &[
        "AccountPolicy:UseCaptcha",
        "CaptchaConfig:Provider",
        "CaptchaConfig:SiteKey",
        "CaptchaConfig:SecretKey",
        "CaptchaConfig:HashPow:Difficulty",
    ]
}

fn nonempty_value(values: &BTreeMap<String, Option<String>>, key: &str) -> Option<String> {
    values
        .get(key)
        .and_then(Clone::clone)
        .filter(|value| !value.trim().is_empty())
}

fn update_revision_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
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
        assert_eq!(hex_bytes("aéa"), None); // never slice malformed UTF-8 boundaries
    }

    #[test]
    fn enabled_policy_rejects_non_verifying_providers_and_binds_revision() {
        let settings = |provider: &str, difficulty: &str, secret: Option<&str>| {
            let mut values = BTreeMap::from([
                (
                    "AccountPolicy:UseCaptcha".to_string(),
                    Some("true".to_string()),
                ),
                (
                    "CaptchaConfig:Provider".to_string(),
                    Some(provider.to_string()),
                ),
                (
                    "CaptchaConfig:HashPow:Difficulty".to_string(),
                    Some(difficulty.to_string()),
                ),
            ]);
            if let Some(secret) = secret {
                values.insert(
                    "CaptchaConfig:SecretKey".to_string(),
                    Some(secret.to_string()),
                );
            }
            CaptchaSettings::from_values(&values, false)
        };

        assert!(settings("None", "18", None).is_err());
        assert!(settings("Unknown", "18", None).is_err());
        assert!(settings("CloudflareTurnstile", "18", None).is_err());

        let old = settings("HashPow", "18", None).unwrap();
        let changed = settings("HashPow", "19", None).unwrap();
        let proof = CaptchaAdmission::Local(Some(old.revision()));
        assert!(old.authorize(proof).is_ok());
        assert!(changed.authorize(proof).is_err());
        assert!(changed.authorize(CaptchaAdmission::OAuthProvider).is_ok());
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
    async fn hashpow_service_rejects_oversized_tokens_before_cache_access() {
        let cache = InMemoryCache::default();
        let key = "_HP_kept";
        cache.set(key, b"0011223344556677", None).await;
        let token = format!("kept:{}", "0".repeat(MAX_CAPTCHA_TOKEN_BYTES));

        let service = CaptchaService::HashPow { difficulty: 0 };
        assert!(!service.verify(&token, &cache).await.unwrap());
        assert!(cache.get(key).await.is_some());
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
