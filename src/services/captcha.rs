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

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
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
const HASHPOW_CHALLENGE_TTL_SECS: i64 = 5 * 60;
const HASHPOW_CLOCK_SKEW_SECS: i64 = 5;
const HASHPOW_SIGNING_DOMAIN: &[u8] = b"rsctf-hashpow-challenge-v1\0";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct HashPowClaims {
    #[serde(rename = "v")]
    version: u8,
    #[serde(rename = "n")]
    nonce: String,
    #[serde(rename = "c")]
    challenge: String,
    #[serde(rename = "iat")]
    issued_at: i64,
    #[serde(rename = "exp")]
    expires_at: i64,
    #[serde(rename = "d")]
    difficulty: u32,
    #[serde(rename = "r")]
    policy_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedHashPowChallenge {
    pub id: String,
    pub challenge: String,
    pub difficulty: u32,
    pub expires_at: i64,
}

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

            // Live HashPoW verification is policy- and deployment-key-bound in
            // `CaptchaSettings::verify_local`. A standalone env service has no
            // signing key and therefore cannot authorize a self-contained token.
            CaptchaService::HashPow { .. } => Ok(false),
        }
    }
}

fn hashpow_signature(signing_key: &[u8], encoded_claims: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_key)
        .expect("HMAC-SHA256 accepts deployment secrets of every length");
    mac.update(HASHPOW_SIGNING_DOMAIN);
    mac.update(encoded_claims.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn encode_hashpow_claims(signing_key: &[u8], claims: &HashPowClaims) -> AppResult<String> {
    let payload = serde_json::to_vec(claims)
        .map_err(|error| AppError::internal(format!("HashPoW claims encode failed: {error}")))?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(hashpow_signature(signing_key, &encoded));
    Ok(format!("{encoded}.{signature}"))
}

fn decode_hashpow_claims(signing_key: &[u8], token: &str) -> Option<HashPowClaims> {
    let (encoded, signature) = token.split_once('.')?;
    if encoded.is_empty() || encoded.len() > 1_024 || signature.len() != 43 {
        return None;
    }
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_key).ok()?;
    mac.update(HASHPOW_SIGNING_DOMAIN);
    mac.update(encoded.as_bytes());
    mac.verify_slice(&signature).ok()?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()?;
    serde_json::from_slice(&payload).ok()
}

pub fn issue_hashpow_challenge(
    settings: &CaptchaSettings,
    signing_key: &[u8],
    now: i64,
) -> AppResult<IssuedHashPowChallenge> {
    if !settings.use_captcha || settings.provider != "HashPow" {
        return Err(AppError::not_found("PoW challenge is not available"));
    }
    let claims = HashPowClaims {
        version: 1,
        nonce: crate::utils::codec::random_hex(6),
        challenge: crate::utils::codec::random_hex(8),
        issued_at: now,
        expires_at: now + HASHPOW_CHALLENGE_TTL_SECS,
        difficulty: settings.difficulty,
        policy_revision: settings.revision_hex(),
    };
    Ok(IssuedHashPowChallenge {
        id: encode_hashpow_claims(signing_key, &claims)?,
        challenge: claims.challenge,
        difficulty: claims.difficulty,
        expires_at: claims.expires_at,
    })
}

/// Verify a paid proof before creating the one bounded consumed marker. Invalid
/// or unsigned issuance traffic creates no cache state.
async fn verify_hashpow(
    token: &str,
    settings: &CaptchaSettings,
    signing_key: &[u8],
    cache: &dyn Cache,
    now: i64,
) -> bool {
    if token.len() > MAX_CAPTCHA_TOKEN_BYTES {
        return false;
    }
    let Some((id, answer)) = token.split_once(':') else {
        return false;
    };
    if answer.len() != 16 || !answer.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let Some(claims) = decode_hashpow_claims(signing_key, id) else {
        return false;
    };
    if claims.version != 1
        || claims.nonce.len() != 12
        || !claims.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        || claims.challenge.len() != 16
        || !claims
            .challenge
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || claims.issued_at > now + HASHPOW_CLOCK_SKEW_SECS
        || claims.expires_at < now
        || claims.expires_at - claims.issued_at != HASHPOW_CHALLENGE_TTL_SECS
        || claims.difficulty != settings.difficulty
        || claims.policy_revision != settings.revision_hex()
    {
        return false;
    }

    let (Some(value_bytes), Some(answer_bytes)) = (hex_bytes(&claims.challenge), hex_bytes(answer))
    else {
        return false;
    };

    let mut hasher = Sha256::new();
    hasher.update(&value_bytes);
    hasher.update(&answer_bytes);
    if leading_zero_bits(&hasher.finalize()) < claims.difficulty {
        return false;
    }

    let ttl = u64::try_from(claims.expires_at.saturating_sub(now).max(1)).unwrap_or(1);
    cache
        .set_if_absent(
            &format!("_HP_USED_{}", claims.nonce),
            b"1",
            Some(Duration::from_secs(ttl)),
        )
        .await
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
        hashpow_signing_key: &[u8],
    ) -> AppResult<CaptchaAdmission> {
        if !self.use_captcha {
            return Ok(CaptchaAdmission::Local(None));
        }
        let verified = if self.provider == "HashPow" {
            verify_hashpow(
                token,
                self,
                hashpow_signing_key,
                cache,
                chrono::Utc::now().timestamp(),
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

    fn revision_hex(&self) -> String {
        hex::encode(self.revision().0)
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
    async fn signed_hashpow_is_stateless_until_a_valid_proof_is_consumed() {
        const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
        let values = BTreeMap::from([
            (
                "AccountPolicy:UseCaptcha".to_string(),
                Some("true".to_string()),
            ),
            (
                "CaptchaConfig:Provider".to_string(),
                Some("HashPow".to_string()),
            ),
            (
                "CaptchaConfig:HashPow:Difficulty".to_string(),
                Some("8".to_string()),
            ),
        ]);
        let settings = CaptchaSettings::from_values(&values, false).unwrap();
        let now = 1_800_000_000;
        let issued = issue_hashpow_challenge(&settings, KEY, now).unwrap();
        let cache = InMemoryCache::default();

        assert!(cache
            .get(&format!(
                "_HP_USED_{}",
                decode_hashpow_claims(KEY, &issued.id).unwrap().nonce
            ))
            .await
            .is_none());
        assert!(!verify_hashpow("no-colon", &settings, KEY, &cache, now).await);
        assert!(
            !verify_hashpow(
                &format!("{}:0000000000000000", issued.id),
                &settings,
                b"different-signing-key-000000000000",
                &cache,
                now,
            )
            .await
        );

        let value_bytes = hex_bytes(&issued.challenge).unwrap();
        let answer = (0..5_000_000u32)
            .find_map(|nonce| {
                let mut hasher = Sha256::new();
                hasher.update(&value_bytes);
                hasher.update(0u32.to_be_bytes());
                hasher.update(nonce.to_be_bytes());
                (leading_zero_bits(&hasher.finalize()) >= issued.difficulty)
                    .then(|| format!("{nonce:016x}"))
            })
            .expect("an 8-bit answer exists well within range");
        let token = format!("{}:{answer}", issued.id);
        assert!(verify_hashpow(&token, &settings, KEY, &cache, now).await);
        assert!(!verify_hashpow(&token, &settings, KEY, &cache, now).await);
        assert!(
            !verify_hashpow(
                &format!(
                    "{}:{answer}",
                    issue_hashpow_challenge(&settings, KEY, now).unwrap().id
                ),
                &settings,
                KEY,
                &cache,
                now + HASHPOW_CHALLENGE_TTL_SECS + 1,
            )
            .await
        );
    }

    #[tokio::test]
    async fn concurrent_hashpow_replays_have_one_winner() {
        const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
        let values = BTreeMap::from([
            (
                "AccountPolicy:UseCaptcha".to_string(),
                Some("true".to_string()),
            ),
            (
                "CaptchaConfig:Provider".to_string(),
                Some("HashPow".to_string()),
            ),
            (
                "CaptchaConfig:HashPow:Difficulty".to_string(),
                Some("8".to_string()),
            ),
        ]);
        let settings = std::sync::Arc::new(CaptchaSettings::from_values(&values, false).unwrap());
        let now = 1_800_000_000;
        let issued = issue_hashpow_challenge(&settings, KEY, now).unwrap();
        let value = hex_bytes(&issued.challenge).unwrap();
        let answer = (0..5_000_000u32)
            .find_map(|nonce| {
                let mut hasher = Sha256::new();
                hasher.update(&value);
                hasher.update(0u32.to_be_bytes());
                hasher.update(nonce.to_be_bytes());
                (leading_zero_bits(&hasher.finalize()) >= issued.difficulty)
                    .then(|| format!("{nonce:016x}"))
            })
            .unwrap();
        let token = format!("{}:{answer}", issued.id);
        let cache = std::sync::Arc::new(InMemoryCache::default());
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(17));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let cache = cache.clone();
            let settings = settings.clone();
            let barrier = barrier.clone();
            let token = token.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                verify_hashpow(&token, &settings, KEY, cache.as_ref(), now).await
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
