//! Repository event and challenge manifest wire models.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::utils::enums::{ChallengeVariantMode, NetworkMode, SolveReceiptMode};
use crate::utils::error::{AppError, AppResult};

/// In-memory shape of one `.gzevent` event manifest, mirroring RSCTF
/// `Models/Request/Edit/GzEventModel`. Every field is optional (a sparse
/// manifest only seeds what it names); nested keys are camelCase. Used at
/// game-CREATE time only -- a re-scan never re-applies these over operator edits.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GzEventModel {
    pub title: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub poster: Option<String>,
    pub hidden: Option<bool>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub accept_without_review: Option<bool>,
    pub invite_code: Option<String>,
    pub organizations: Option<Vec<String>>,
    pub team_member_count_limit: Option<i32>,
    pub container_count_limit: Option<i32>,
    pub practice_mode: Option<bool>,
    pub writeup_required: Option<bool>,
    pub writeup_deadline: Option<DateTime<Utc>>,
    pub writeup_note: Option<String>,
    pub blood_bonus: Option<i64>,
    pub ad: Option<GzEventAd>,
}

/// The `ad:` section of a `.gzevent` -- event-wide Attack & Defense knobs, each
/// optional and applied onto the Game only when named (mirrors `AdEventSection`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GzEventAd {
    pub tick_seconds: Option<i32>,
    pub flag_lifetime_ticks: Option<i32>,
    pub warmup_seconds: Option<i32>,
    pub reset_cooldown_minutes: Option<i32>,
    pub allow_snapshot_download: Option<bool>,
    pub snapshot_retention_days: Option<i32>,
    pub getflag_window_fraction: Option<f64>,
    pub min_grace_period_seconds: Option<i32>,
}

/// Parse a `.gzevent` manifest into a [`GzEventModel`]. Unrecognized keys are
/// ignored (serde default), so a manifest with extra fields still loads.
pub async fn parse_event_manifest(path: &Path) -> AppResult<GzEventModel> {
    let raw = tokio::fs::read_to_string(path).await.map_err(|error| {
        AppError::internal(format!("git_sync: read {}: {error}", path.display()))
    })?;
    serde_norway::from_str(&raw)
        .map_err(|error| AppError::bad_request(format!("invalid .gzevent: {error}")))
}

/// In-memory shape of one `challenge.yml` / `challenge.yaml` file, mirroring
/// RSCTF `Models/Request/Edit/ChallengeYamlModel` -- the subset of the gzcli
/// template schema that maps onto a `GameChallenge`.
///
/// Aliases match the upstream (camelCase for nested fields). Unrecognized keys
/// are ignored (serde's default) and every field is optional (`Option` missing
/// means `None`), so a sparse manifest only sets what it names.
#[derive(Debug, Default, Deserialize)]
pub struct ChallengeYaml {
    pub name: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    /// One of `StaticAttachment`, `StaticContainer`, `DynamicAttachment`,
    /// `DynamicContainer`, `AttackDefense`, `KingOfTheHill` (case-insensitive).
    #[serde(rename = "type")]
    pub challenge_type: Option<String>,
    pub category: Option<String>,
    #[serde(rename = "minScoreRate")]
    pub min_score_rate: Option<f64>,
    pub difficulty: Option<f64>,
    /// When true the challenge opts out of sync entirely -- never created.
    pub ignore: Option<bool>,
    pub hints: Option<Vec<String>>,
    pub flags: Option<Vec<String>>,
    #[serde(rename = "flagTemplate")]
    pub flag_template: Option<String>,
    /// Attachment source (RSCTF `provide`): a file OR directory path relative to
    /// the challenge dir. When absent, the TCP1P `dist/` convention is used.
    pub provide: Option<String>,
    #[serde(rename = "disableBloodBonus")]
    pub disable_blood_bonus: Option<bool>,
    #[serde(rename = "submissionLimit")]
    pub submission_limit: Option<i32>,
    #[serde(rename = "variantMode")]
    pub variant_mode: Option<ChallengeVariantMode>,
    #[serde(rename = "variantGeneratorImage")]
    pub variant_generator_image: Option<String>,
    #[serde(rename = "variantGeneratorDigest")]
    pub variant_generator_digest: Option<String>,
    #[serde(rename = "solveReceiptMode")]
    pub solve_receipt_mode: Option<SolveReceiptMode>,
    #[serde(rename = "receiptVerifierIdentity")]
    pub receipt_verifier_identity: Option<String>,
    pub container: Option<ContainerSection>,
    /// Attack-&-Defense / King-of-the-Hill block -- only consulted when the
    /// challenge type uses the A&D engine.
    pub ad: Option<AdSection>,
}

/// Container knobs (`container:` block). Present on any container-typed
/// challenge; the image + ports also feed the A&D service container.
#[derive(Debug, Default, Deserialize)]
pub struct ContainerSection {
    #[serde(rename = "containerImage")]
    pub container_image: Option<String>,
    #[serde(rename = "flagTemplate")]
    pub flag_template: Option<String>,
    #[serde(rename = "memoryLimit")]
    pub memory_limit: Option<i32>,
    #[serde(rename = "cpuCount")]
    pub cpu_count: Option<i32>,
    #[serde(rename = "storageLimit")]
    pub storage_limit: Option<i32>,
    #[serde(rename = "exposePort")]
    pub expose_port: Option<i32>,
    #[serde(rename = "enableTrafficCapture")]
    pub enable_traffic_capture: Option<bool>,
    #[serde(rename = "enableSharedContainer")]
    pub enable_shared_container: Option<bool>,
    #[serde(rename = "networkMode")]
    pub network_mode: Option<NetworkMode>,
}

/// A&D-specific per-challenge knobs (`ad:` block). Only the A&D-specific fields
/// live here; the service image + ports come from the shared `container:` block.
#[derive(Debug, Default, Deserialize)]
pub struct AdSection {
    #[serde(rename = "checkerImage")]
    pub checker_image: Option<String>,
    #[serde(rename = "allowEgress")]
    pub allow_egress: Option<bool>,
    #[serde(rename = "allowSelfReset")]
    pub allow_self_reset: Option<bool>,
    #[serde(rename = "sshRequiresFlag")]
    pub ssh_requires_flag: Option<bool>,
    #[serde(rename = "selfHosted")]
    pub self_hosted: Option<bool>,
}
