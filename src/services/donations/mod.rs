//! Optional donation-provider integration.
//!
//! Provider credentials stay server-side. Public callers receive only a small,
//! cached projection of successful support history; no payment identifiers,
//! email addresses, or API credentials cross the API boundary.

#[cfg(test)]
mod db_tests;
mod trakteer;

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use axum::http::HeaderValue;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::app_state::SharedState;
use crate::services::cache::Cache;
use crate::utils::error::{AppError, AppResult};
use crate::utils::single_flight::SingleFlight;

const ENABLED_KEY: &str = "DonationConfig:Enabled";
const PROVIDER_KEY: &str = "DonationConfig:Provider";
const API_KEY: &str = "DonationConfig:ApiKey";
const SETTINGS_CACHE_KEY: &str = "donations:settings:v1";
const FRESH_CACHE_KEY: &str = "donations:feed:fresh:v2";
const STALE_CACHE_KEY: &str = "donations:feed:stale:v2";
const SETTINGS_TTL: Duration = Duration::from_secs(60);
const FRESH_TTL: Duration = Duration::from_secs(5 * 60);
const STALE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const FILL_LOCK_TTL: Duration = Duration::from_secs(25);
const FINGERPRINT_LEN: usize = 64;

static FEED_FLIGHT: LazyLock<SingleFlight<Option<Bytes>>> = LazyLock::new(SingleFlight::new);

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum DonationProvider {
    #[default]
    Trakteer,
}

impl DonationProvider {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("Trakteer") | None => Self::Trakteer,
            Some(_) => Self::Trakteer,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Trakteer => "Trakteer",
        }
    }
}

impl std::fmt::Display for DonationProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Admin-facing configuration. `api_key` is write-only; reads expose only
/// `has_api_key`, so the secret never enters the SPA or browser storage.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DonationConfig {
    pub enabled: bool,
    pub provider: DonationProvider,
    pub api_key: Option<String>,
    pub has_api_key: bool,
}

#[derive(Clone, Debug)]
struct DonationSettings {
    enabled: bool,
    provider: DonationProvider,
    api_key: Option<String>,
}

impl DonationSettings {
    fn from_map(values: &BTreeMap<String, Option<String>>) -> Self {
        let value = |key: &str| values.get(key).and_then(|value| value.as_deref());
        Self {
            enabled: value(ENABLED_KEY) == Some("true"),
            provider: DonationProvider::parse(value(PROVIDER_KEY)),
            api_key: value(API_KEY)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        }
    }

    fn active(&self) -> bool {
        self.enabled && self.api_key.is_some()
    }

    fn fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(self.provider.as_str().as_bytes());
        hash.update([0]);
        if let Some(api_key) = &self.api_key {
            hash.update(api_key.as_bytes());
        }
        hex::encode(hash.finalize())
    }

    async fn load(pool: &PgPool) -> AppResult<Self> {
        let rows = sqlx::query_as::<_, (String, Option<String>)>(
            r#"SELECT config_key, value
                 FROM "Configs"
                WHERE config_key = ANY($1)"#,
        )
        .bind(vec![ENABLED_KEY, PROVIDER_KEY, API_KEY])
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(Self::from_map(&rows.into_iter().collect()))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicSettings {
    active: bool,
    provider: DonationProvider,
    fingerprint: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DonationFeed {
    pub provider: DonationProvider,
    pub currency: &'static str,
    #[serde(with = "crate::utils::datetime::millis")]
    pub fetched_at: DateTime<Utc>,
    pub total_amount: i64,
    pub total_quantity: i64,
    pub support_count: usize,
    pub supporter_count: usize,
    pub leaderboard: Vec<DonationLeaderboardEntry>,
    pub messages: Vec<DonationMessage>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DonationLeaderboardEntry {
    pub rank: usize,
    pub supporter_name: String,
    pub total_amount: i64,
    pub total_quantity: i64,
    pub support_count: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DonationMessage {
    pub supporter_name: String,
    pub message: String,
    pub amount: i64,
    pub quantity: i64,
    pub unit_name: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub updated_at: DateTime<Utc>,
    pub reply_message: Option<String>,
}

pub fn admin_config(values: &BTreeMap<String, Option<String>>) -> DonationConfig {
    let settings = DonationSettings::from_map(values);
    DonationConfig {
        enabled: settings.enabled,
        provider: settings.provider,
        api_key: None,
        has_api_key: settings.api_key.is_some(),
    }
}

pub fn public_config(values: &BTreeMap<String, Option<String>>) -> (bool, DonationProvider) {
    let settings = DonationSettings::from_map(values);
    (settings.active(), settings.provider)
}

fn supplied_api_key(input: &DonationConfig) -> AppResult<Option<&str>> {
    let supplied = input
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if supplied.is_some_and(|value| value.len() > 512 || HeaderValue::from_str(value).is_err()) {
        return Err(AppError::bad_request("Donation API key is invalid"));
    }
    Ok(supplied)
}

/// Preflight an admin form before independently persisted settings sections
/// can change. `save_config` repeats the effective-key check under a row lock.
pub async fn validate_config(pool: &PgPool, input: &DonationConfig) -> AppResult<()> {
    let supplied = supplied_api_key(input)?;
    if input.enabled && supplied.is_none() {
        let configured = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (
                   SELECT 1 FROM "Configs"
                    WHERE config_key = $1
                      AND NULLIF(BTRIM(value), '') IS NOT NULL
               )"#,
        )
        .bind(API_KEY)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !configured {
            return Err(AppError::bad_request(
                "Configure a donation API key before enabling donations",
            ));
        }
    }
    Ok(())
}

/// Persist an admin update atomically. A blank API key preserves the configured
/// value, while enabling without any effective key fails before the write.
pub async fn save_config(pool: &PgPool, cache: &dyn Cache, input: DonationConfig) -> AppResult<()> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let existing = sqlx::query_scalar::<_, Option<String>>(
        r#"SELECT value FROM "Configs" WHERE config_key = $1 FOR UPDATE"#,
    )
    .bind(API_KEY)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .flatten()
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty());

    let supplied = supplied_api_key(&input)?;
    let effective_key = supplied.map(str::to_owned).or(existing);
    if input.enabled && effective_key.is_none() {
        return Err(AppError::bad_request(
            "Configure a donation API key before enabling donations",
        ));
    }

    let keys = vec![ENABLED_KEY, PROVIDER_KEY];
    let values = vec![
        input.enabled.to_string(),
        input.provider.as_str().to_owned(),
    ];
    sqlx::query(
        r#"INSERT INTO "Configs" (config_key, value, cache_keys)
           SELECT * FROM UNNEST($1::text[], $2::text[], ARRAY_FILL(NULL::jsonb, ARRAY[2]))
           ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#,
    )
    .bind(keys)
    .bind(values)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(api_key) = supplied {
        sqlx::query(
            r#"INSERT INTO "Configs" (config_key, value, cache_keys)
               VALUES ($1, $2, NULL)
               ON CONFLICT (config_key) DO UPDATE SET value = EXCLUDED.value"#,
        )
        .bind(API_KEY)
        .bind(api_key)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    invalidate(cache).await;
    Ok(())
}

pub async fn invalidate(cache: &dyn Cache) {
    for key in [SETTINGS_CACHE_KEY, FRESH_CACHE_KEY, STALE_CACHE_KEY] {
        cache.remove(key).await;
    }
}

async fn public_settings(st: &SharedState) -> AppResult<PublicSettings> {
    if let Some(cached) = st.cache.get(SETTINGS_CACHE_KEY).await {
        if let Ok(settings) = serde_json::from_slice(&cached) {
            return Ok(settings);
        }
        st.cache.remove(SETTINGS_CACHE_KEY).await;
    }
    let settings = DonationSettings::load(st.pg()).await?;
    let public = PublicSettings {
        active: settings.active(),
        provider: settings.provider,
        fingerprint: settings.fingerprint(),
    };
    let bytes = serde_json::to_vec(&public)
        .map_err(|error| AppError::internal(format!("donation settings encode: {error}")))?;
    st.cache
        .set(SETTINGS_CACHE_KEY, &bytes, Some(SETTINGS_TTL))
        .await;
    Ok(public)
}

fn cached_body(value: Bytes, fingerprint: &str) -> Option<Bytes> {
    (value.len() >= FINGERPRINT_LEN && value.get(..FINGERPRINT_LEN) == Some(fingerprint.as_bytes()))
        .then(|| value.slice(FINGERPRINT_LEN..))
}

async fn cached_feed(cache: &dyn Cache, key: &str, fingerprint: &str) -> Option<Bytes> {
    cached_body(cache.get(key).await?, fingerprint)
}

async fn store_feed(cache: &dyn Cache, fingerprint: &str, body: &[u8]) {
    let mut cached = Vec::with_capacity(FINGERPRINT_LEN + body.len());
    cached.extend_from_slice(fingerprint.as_bytes());
    cached.extend_from_slice(body);
    cache.set(FRESH_CACHE_KEY, &cached, Some(FRESH_TTL)).await;
    cache.set(STALE_CACHE_KEY, &cached, Some(STALE_TTL)).await;
}

/// Return the public JSON body. The common path is a zero-copy `Bytes` slice
/// from the two-tier cache; upstream calls are bounded and coalesced locally and
/// across replicas.
pub async fn feed_json(st: SharedState) -> AppResult<Bytes> {
    let public = public_settings(&st).await?;
    if !public.active {
        return Err(AppError::not_found("Donations are disabled"));
    }
    if let Some(body) = cached_feed(st.cache.as_ref(), FRESH_CACHE_KEY, &public.fingerprint).await {
        return Ok(body);
    }

    let flight_key = public.fingerprint.clone();
    let expected_fingerprint = public.fingerprint;
    let state = st.clone();
    let body = FEED_FLIGHT
        .run(&flight_key, move || async move {
            if let Some(body) =
                cached_feed(state.cache.as_ref(), FRESH_CACHE_KEY, &expected_fingerprint).await
            {
                return Some(body);
            }

            let lock_key = format!("donations:fill:{expected_fingerprint}");
            let owns_lock = state
                .cache
                .set_if_absent(&lock_key, b"1", Some(FILL_LOCK_TTL))
                .await;
            if !owns_lock {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Some(body) =
                        cached_feed(state.cache.as_ref(), FRESH_CACHE_KEY, &expected_fingerprint)
                            .await
                    {
                        return Some(body);
                    }
                }
                return cached_feed(state.cache.as_ref(), STALE_CACHE_KEY, &expected_fingerprint)
                    .await;
            }

            let result = async {
                let settings = DonationSettings::load(state.pg()).await.ok()?;
                if !settings.active() || settings.fingerprint() != expected_fingerprint {
                    return None;
                }
                let feed_result = match settings.provider {
                    DonationProvider::Trakteer => {
                        trakteer::fetch(settings.api_key.as_deref()?).await
                    }
                };
                let feed = match feed_result {
                    Ok(feed) => feed,
                    Err(error) => {
                        tracing::warn!(provider = %settings.provider, %error, "donation provider request failed");
                        return None;
                    }
                };
                let body = serde_json::to_vec(&feed).ok()?;
                store_feed(state.cache.as_ref(), &expected_fingerprint, &body).await;
                Some(Bytes::from(body))
            }
            .await;
            state.cache.remove(&lock_key).await;
            result
        })
        .await;

    if let Some(body) = body {
        return Ok(body);
    }
    if let Some(stale) = cached_feed(st.cache.as_ref(), STALE_CACHE_KEY, &flight_key).await {
        tracing::warn!(provider = %public.provider, "donation provider unavailable; serving stale cache");
        return Ok(stale);
    }
    tracing::warn!(provider = %public.provider, "donation provider unavailable and no stale cache exists");
    Err(AppError::unavailable(
        "Donation history is temporarily unavailable",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_projection_never_returns_the_key() {
        let values = BTreeMap::from([
            (ENABLED_KEY.to_owned(), Some("true".to_owned())),
            (PROVIDER_KEY.to_owned(), Some("Trakteer".to_owned())),
            (API_KEY.to_owned(), Some("top-secret".to_owned())),
        ]);
        let config = admin_config(&values);
        assert!(config.enabled);
        assert!(config.has_api_key);
        assert_eq!(config.api_key, None);
        assert!(!serde_json::to_string(&config)
            .unwrap()
            .contains("top-secret"));
    }

    #[test]
    fn cached_body_is_zero_copy_and_bound_to_its_configuration() {
        let bytes = Bytes::from(format!("{}{{\"ok\":true}}", "a".repeat(64)));
        assert_eq!(
            cached_body(bytes.clone(), &"a".repeat(64)).unwrap(),
            Bytes::from_static(br#"{"ok":true}"#)
        );
        assert!(cached_body(bytes, &"b".repeat(64)).is_none());
    }
}
