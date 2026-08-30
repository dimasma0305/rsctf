//! Stable wire contract between the network owner and the bounded sensor.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) struct SensorSnapshotCache {
    value: tokio::sync::Mutex<Option<(tokio::time::Instant, SensorSnapshot)>>,
}

impl SensorSnapshotCache {
    pub(crate) fn new() -> Self {
        Self {
            value: tokio::sync::Mutex::new(None),
        }
    }

    pub(crate) async fn lock(
        &self,
    ) -> tokio::sync::MutexGuard<'_, Option<(tokio::time::Instant, SensorSnapshot)>> {
        self.value.lock().await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorSnapshot {
    #[serde(with = "crate::utils::datetime::millis")]
    pub generated_at_utc: DateTime<Utc>,
    pub games: Vec<SensorGameSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorGameSnapshot {
    pub game_id: i32,
    pub behavior_telemetry_enabled: bool,
    pub flag_scan_enabled: bool,
    pub provider_dns_telemetry_enabled: bool,
    pub source_asn_telemetry_enabled: bool,
    pub device_sharing_telemetry_enabled: bool,
    pub peers: Vec<SensorPeer>,
    pub flag_patterns: Vec<SensorFlagPattern>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorPeer {
    pub peer_id: Uuid,
    pub user_id: Uuid,
    pub participation_id: i32,
    pub public_key: String,
    pub address: String,
    pub generation: i32,
    /// Current authenticated public endpoint, supplied only to the local
    /// sensor and immediately transformed into a keyed hash/category.
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SensorFlagPattern {
    pub challenge_id: i32,
    pub owning_participation_id: i32,
    /// Plaintext exists only in the sensor's bounded in-memory matcher. It is
    /// never accepted by the telemetry ingest API or persisted in its tables.
    pub pattern: String,
    pub value_hash: String,
}

/// Snapshot memory ceiling independent of the number of games in PostgreSQL.
pub const MAX_SENSOR_PEERS: usize = 100_000;

pub const PROVIDER_OPENAI: i16 = 0;
pub const PROVIDER_ANTHROPIC: i16 = 1;
pub const PROVIDER_GOOGLE_AI: i16 = 2;
pub const PROVIDER_MICROSOFT_AI: i16 = 3;
pub const PROVIDER_OTHER_AI: i16 = 4;
pub const PROVIDER_HOSTING: i16 = 16;

/// Classify only well-known provider suffixes. This is context, not evidence
/// that a person used an AI system; false positives (documentation, CDN, shared
/// resolvers) are expected and receive zero score.
pub fn provider_category(name: &str) -> Option<i16> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let suffix = |domain: &str| {
        name == domain
            || name
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    };
    if suffix("openai.com") || suffix("chatgpt.com") || suffix("oaistatic.com") {
        Some(PROVIDER_OPENAI)
    } else if suffix("anthropic.com") || suffix("claude.ai") {
        Some(PROVIDER_ANTHROPIC)
    } else if suffix("generativelanguage.googleapis.com") || suffix("gemini.google.com") {
        Some(PROVIDER_GOOGLE_AI)
    } else if suffix("copilot.microsoft.com") || suffix("githubcopilot.com") {
        Some(PROVIDER_MICROSOFT_AI)
    } else if suffix("huggingface.co") || suffix("perplexity.ai") || suffix("mistral.ai") {
        Some(PROVIDER_OTHER_AI)
    } else if suffix("amazonaws.com")
        || suffix("digitaloceanspaces.com")
        || suffix("cloudfront.net")
        || suffix("azure.com")
    {
        Some(PROVIDER_HOSTING)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_matching_is_suffix_safe_and_context_only() {
        assert_eq!(provider_category("api.openai.com."), Some(PROVIDER_OPENAI));
        assert_eq!(provider_category("claude.ai"), Some(PROVIDER_ANTHROPIC));
        assert_eq!(provider_category("openai.com.attacker.test"), None);
        assert_eq!(provider_category("notopenai.com"), None);
    }
}
