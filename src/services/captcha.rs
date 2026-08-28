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
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
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
const HASHPOW_LIFETIME: Duration = Duration::from_secs(5 * 60);
const HASHPOW_CLOCK_SKEW: u64 = 15;
const CAPTCHA_SETTINGS_SNAPSHOT_TTL: Duration = Duration::from_secs(2);

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

#[derive(Clone, Debug)]
pub struct IssuedHashPowChallenge {
    pub id: String,
    pub challenge: String,
    pub difficulty: i32,
    pub expires_at_millis: i64,
}

#[derive(Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedHashPowPayload {
    version: u8,
    challenge_id: String,
    challenge: String,
    issued_at: u64,
    expires_at: u64,
    difficulty: u32,
    policy_revision: String,
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
    pub async fn verify(&self, token: &str, _cache: &dyn Cache) -> AppResult<bool> {
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

            // HashPoW verification needs the policy revision and deployment
            // signing secret held by `CaptchaSettings::verify_local`.
            CaptchaService::HashPow { .. } => Ok(false),
        }
    }
}

/// Verify a proof-of-work token of the form `"<signed-id>:<answer>"` against
/// the self-contained challenge value authenticated by the signed envelope.
///
/// This matches the client worker (`web/src/utils/PowWorker.ts`) exactly:
/// the browser hashes `SHA-256(hex_decode(challenge_value) ‖ salt ‖ nonce)` and
/// returns `answer = hex(salt) ‖ hex(nonce)` (16 hex chars). Server-side the
/// pre-image is `hex_decode(challenge_value) ‖ hex_decode(answer)`, and the token
/// passes iff its SHA-256 has ≥ `difficulty` leading zero bits. Only a valid
/// proof creates a short `_HPC_*` consumed marker, atomically, so issuance is
/// stateless and a solved nonce cannot be replayed.
async fn verify_hashpow(
    token: &str,
    expected_difficulty: u32,
    expected_revision: CaptchaRevision,
    signing_secret: &str,
    cache: &dyn Cache,
) -> bool {
    let mut parts = token.splitn(2, ':');
    let (id, answer) = match (parts.next(), parts.next()) {
        (Some(i), Some(a)) if !i.is_empty() && !a.is_empty() => (i, a),
        _ => return false,
    };
    if id.len() > 512 || answer.len() != 16 {
        return false;
    }
    let Some(payload) = verify_hashpow_envelope(id, signing_secret) else {
        return false;
    };
    let now = unix_seconds();
    if payload.version != 1
        || payload.difficulty != expected_difficulty
        || payload.policy_revision != hex::encode(expected_revision.0)
        || payload.challenge_id.len() != 24
        || payload.challenge.len() != 16
        || payload.issued_at > now.saturating_add(HASHPOW_CLOCK_SKEW)
        || payload.expires_at <= now
        || payload.expires_at.saturating_sub(payload.issued_at) != HASHPOW_LIFETIME.as_secs()
    {
        return false;
    }

    let (Some(value_bytes), Some(answer_bytes)) =
        (hex_bytes(&payload.challenge), hex_bytes(answer))
    else {
        return false;
    };

    let mut hasher = Sha256::new();
    hasher.update(&value_bytes);
    hasher.update(&answer_bytes);
    if leading_zero_bits(&hasher.finalize()) < payload.difficulty {
        return false;
    }

    // Only paid, valid work creates cache pressure. SET NX is the distributed
    // one-use decision; its short marker naturally expires with the challenge.
    let consumed_key = format!("_HPC_{}", crate::utils::codec::sha256_hex(id.as_bytes()));
    cache
        .set_if_absent(
            &consumed_key,
            b"1",
            Some(Duration::from_secs(
                payload.expires_at.saturating_sub(now).max(1),
            )),
        )
        .await
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hashpow_mac(payload: &[u8], signing_secret: &str) -> Option<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()).ok()?;
    mac.update(b"rsctf-hashpow-v1\0");
    mac.update(payload);
    Some(mac.finalize().into_bytes().to_vec())
}

fn sign_hashpow_payload(payload: &SignedHashPowPayload, signing_secret: &str) -> Option<String> {
    let encoded_payload = serde_json::to_vec(payload).ok()?;
    let signature = hashpow_mac(&encoded_payload, signing_secret)?;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    Some(format!(
        "{}.{}",
        engine.encode(encoded_payload),
        engine.encode(signature)
    ))
}

fn verify_hashpow_envelope(id: &str, signing_secret: &str) -> Option<SignedHashPowPayload> {
    let (payload, signature) = id.split_once('.')?;
    if payload.is_empty() || signature.is_empty() || signature.contains('.') {
        return None;
    }
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = engine.decode(payload).ok()?;
    let signature = engine.decode(signature).ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_secret.as_bytes()).ok()?;
    mac.update(b"rsctf-hashpow-v1\0");
    mac.update(&payload);
    mac.verify_slice(&signature).ok()?;
    serde_json::from_slice(&payload).ok()
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

struct CachedCaptchaSettings {
    fallback_use_captcha: bool,
    expires_at: Instant,
    settings: CaptchaSettings,
}

static CAPTCHA_SETTINGS_SNAPSHOT: LazyLock<Mutex<Option<CachedCaptchaSettings>>> =
    LazyLock::new(|| Mutex::new(None));
static CAPTCHA_SETTINGS_FLIGHT: LazyLock<
    crate::utils::single_flight::SingleFlight<Option<CaptchaSettings>>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);

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

    /// Small process-local snapshot for anonymous captcha discovery/issuance.
    /// Admin writes explicitly invalidate it; the TTL is only a missed-writer
    /// backstop, and single-flight prevents a database dogpile at expiry.
    pub async fn load_cached(pool: &PgPool, fallback_use_captcha: bool) -> AppResult<Self> {
        if let Some(settings) = CAPTCHA_SETTINGS_SNAPSHOT
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|cached| {
                cached.fallback_use_captcha == fallback_use_captcha
                    && cached.expires_at > Instant::now()
            })
            .map(|cached| cached.settings.clone())
        {
            return Ok(settings);
        }

        let pool = pool.clone();
        let key = if fallback_use_captcha {
            "enabled"
        } else {
            "disabled"
        };
        let settings = CAPTCHA_SETTINGS_FLIGHT
            .run(key, move || async move {
                Self::load(&pool, fallback_use_captcha).await.ok()
            })
            .await
            .ok_or_else(|| AppError::internal("captcha policy snapshot refresh failed"))?;
        *CAPTCHA_SETTINGS_SNAPSHOT
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(CachedCaptchaSettings {
            fallback_use_captcha,
            expires_at: Instant::now() + CAPTCHA_SETTINGS_SNAPSHOT_TTL,
            settings: settings.clone(),
        });
        Ok(settings)
    }

    pub fn issue_hashpow(&self, signing_secret: &str) -> AppResult<IssuedHashPowChallenge> {
        if !self.use_captcha || self.provider != "HashPow" {
            return Err(AppError::not_found("PoW challenge is not available"));
        }
        let issued_at = unix_seconds();
        let expires_at = issued_at.saturating_add(HASHPOW_LIFETIME.as_secs());
        let payload = SignedHashPowPayload {
            version: 1,
            challenge_id: crate::utils::codec::random_hex(12),
            challenge: crate::utils::codec::random_hex(8),
            issued_at,
            expires_at,
            difficulty: self.difficulty,
            policy_revision: hex::encode(self.revision().0),
        };
        let id = sign_hashpow_payload(&payload, signing_secret)
            .ok_or_else(|| AppError::internal("could not sign PoW challenge"))?;
        Ok(IssuedHashPowChallenge {
            id,
            challenge: payload.challenge,
            difficulty: payload.difficulty as i32,
            expires_at_millis: i64::try_from(expires_at)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
        })
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
        let stored_provider = nonempty_value(values, "CaptchaConfig:Provider");
        let stored_site_key = nonempty_value(values, "CaptchaConfig:SiteKey");
        let stored_secret = nonempty_value(values, "CaptchaConfig:SecretKey");
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
        signing_secret: &str,
    ) -> AppResult<CaptchaAdmission> {
        if !self.use_captcha {
            return Ok(CaptchaAdmission::Local(None));
        }
        let verified = if self.provider == "HashPow" {
            token.len() <= MAX_CAPTCHA_TOKEN_BYTES
                && verify_hashpow(
                    token,
                    self.difficulty,
                    self.revision(),
                    signing_secret,
                    cache,
                )
                .await
        } else {
            self.service().verify(token, cache).await?
        };
        if !verified {
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

pub fn invalidate_settings_snapshot() {
    *CAPTCHA_SETTINGS_SNAPSHOT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
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
    use crate::services::cache::InMemoryCache;

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
        let revision = CaptchaRevision([0; 32]);
        assert!(!verify_hashpow("no-colon", 8, revision, "secret", &cache).await);
        assert!(!verify_hashpow(":answer", 8, revision, "secret", &cache).await);
        assert!(!verify_hashpow("id:", 8, revision, "secret", &cache).await);
        assert!(!verify_hashpow("unknownid:0000000000000000", 8, revision, "secret", &cache).await);
    }

    #[tokio::test]
    async fn hashpow_rejects_oversized_tokens_without_creating_state() {
        let cache = InMemoryCache::default();
        let token = format!("{}:{}", "x".repeat(513), "0".repeat(16));
        assert!(!verify_hashpow(&token, 8, CaptchaRevision([0; 32]), "secret", &cache).await);
    }

    fn signed_test_id(
        challenge: &str,
        difficulty: u32,
        revision: CaptchaRevision,
        secret: &str,
        issued_at: u64,
        expires_at: u64,
    ) -> String {
        sign_hashpow_payload(
            &SignedHashPowPayload {
                version: 1,
                challenge_id: "00112233445566778899aabb".to_string(),
                challenge: challenge.to_string(),
                issued_at,
                expires_at,
                difficulty,
                policy_revision: hex::encode(revision.0),
            },
            secret,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn hashpow_verifies_leading_zero_bits_and_is_single_use() {
        // Mirror the server↔client contract: the id keys a cached hex challenge
        // value, and the answer is hex(bytes) whose sha256(value ‖ answer) has the
        // required leading zero bits.
        let value = "0011223344556677"; // 8-byte hex, like get_pow_challenge mints
        let value_bytes = hex_bytes(value).unwrap();
        let revision = CaptchaRevision([3; 32]);
        let now = unix_seconds();
        let id = signed_test_id(
            value,
            8,
            revision,
            "signing-secret",
            now,
            now + HASHPOW_LIFETIME.as_secs(),
        );

        // Brute-force an answer with >= 8 leading zero bits.
        let mut answer = None;
        for n in 0..5_000_000u32 {
            let candidate = [
                0,
                0,
                0,
                0,
                n.to_be_bytes()[0],
                n.to_be_bytes()[1],
                n.to_be_bytes()[2],
                n.to_be_bytes()[3],
            ];
            let mut h = Sha256::new();
            h.update(&value_bytes);
            h.update(candidate);
            if leading_zero_bits(&h.finalize()) >= 8 {
                answer = Some(format!("00000000{n:08x}"));
                break;
            }
        }
        let answer = answer.expect("a <=8-bit nonce exists well within range");
        let token = format!("{id}:{answer}");
        let cache = InMemoryCache::default();
        assert!(verify_hashpow(&token, 8, revision, "signing-secret", &cache).await);
        assert!(!verify_hashpow(&token, 8, revision, "signing-secret", &cache).await);
        assert!(!verify_hashpow(&token, 8, revision, "different-secret", &cache).await);
        assert!(
            !verify_hashpow(
                &token,
                8,
                CaptchaRevision([4; 32]),
                "signing-secret",
                &cache
            )
            .await
        );
    }

    #[tokio::test]
    async fn concurrent_hashpow_replays_have_one_winner() {
        let revision = CaptchaRevision([5; 32]);
        let now = unix_seconds();
        let id = signed_test_id(
            "0011223344556677",
            0,
            revision,
            "secret",
            now,
            now + HASHPOW_LIFETIME.as_secs(),
        );
        let token = format!("{id}:0000000000000000");
        let cache = std::sync::Arc::new(InMemoryCache::default());
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(17));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            let token = token.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                verify_hashpow(&token, 0, revision, "secret", cache.as_ref()).await
            }));
        }
        barrier.wait().await;

        let mut accepted = 0;
        for task in tasks {
            accepted += usize::from(task.await.unwrap());
        }
        assert_eq!(accepted, 1);
    }

    #[test]
    fn issuance_is_disabled_and_signed_payload_expiry_is_enforced() {
        let disabled = CaptchaSettings {
            use_captcha: false,
            provider: "HashPow".to_string(),
            site_key: None,
            difficulty: 8,
            secret_key: None,
        };
        assert!(disabled.issue_hashpow("secret").is_err());

        let revision = CaptchaRevision([1; 32]);
        let now = unix_seconds();
        let expired = signed_test_id(
            "0011223344556677",
            8,
            revision,
            "secret",
            now.saturating_sub(HASHPOW_LIFETIME.as_secs()),
            now,
        );
        assert_eq!(
            verify_hashpow_envelope(&expired, "secret")
                .unwrap()
                .expires_at,
            now
        );
        assert!(verify_hashpow_envelope(&expired, "wrong-secret").is_none());
    }
}
