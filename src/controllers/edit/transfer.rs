//! edit: game export/import (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const MAX_GAME_IMPORT_ENTRIES: usize = 2_048;
const MAX_GAME_IMPORT_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_GAME_IMPORT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GAME_IMPORT_COMPRESSION_RATIO: u64 = 200;
const MAX_GAME_IMPORT_PATH_COMPONENTS: usize = 32;
static GAME_IMPORT_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[derive(Clone, Copy)]
struct GameImportLimits {
    entries: usize,
    file_bytes: u64,
    total_bytes: u64,
    compression_ratio: u64,
    path_components: usize,
}

const GAME_IMPORT_LIMITS: GameImportLimits = GameImportLimits {
    entries: MAX_GAME_IMPORT_ENTRIES,
    file_bytes: MAX_GAME_IMPORT_FILE_BYTES,
    total_bytes: MAX_GAME_IMPORT_TOTAL_BYTES,
    compression_ratio: MAX_GAME_IMPORT_COMPRESSION_RATIO,
    path_components: MAX_GAME_IMPORT_PATH_COMPONENTS,
};

// --- Game export/import transfer models -------------------------------------
//
// RSCTF's `GameExportService`/`GameImportService` marshal through a large
// `TransferManifest` + `TransferGame` + `TransferChallenge` graph (manifest.json
// + game.json + challenges/challenge-{id}.json + a files/ blob tree). This port
// keeps the package shape the task calls for — `game.json` plus a `challenges/`
// folder of per-challenge JSON (flags inlined) — with a self-consistent schema:
// the same structs serialize on export and deserialize on import, so a package
// round-trips without depending on the C# wire format.
//
// Attachments round-trip in full, mirroring RSCTF: every challenge carries its
// challenge-level attachment (the StaticAttachment blob) AND each static flag's
// per-flag attachment. Both the metadata (type / hash / remote url / filename)
// and — for `Local` files — the blob bytes are bundled under a `files/{hash}`
// tree (deduped by content hash, exactly as `GameExportService.CopyAttachments`
// does). On import valid bundled blobs are re-stored and their `Files` rows are
// recreated in the same transaction as the owning rows. A local attachment
// whose blob is absent or hash-invalid is cleared instead of trusting an
// unrelated deployment's metadata.

/// `game.json` payload — the game settings needed to recreate the game.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportGameModel {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub practice_mode: bool,
    #[serde(default)]
    pub accept_without_review: bool,
    #[serde(default)]
    pub allow_user_submissions: bool,
    #[serde(default)]
    pub writeup_required: bool,
    #[serde(default)]
    pub invite_code: Option<String>,
    #[serde(default)]
    pub team_member_count_limit: i32,
    #[serde(default = "default_container_limit")]
    pub container_count_limit: i32,
    // Backward-compatible on read, but never export this outbound credential.
    #[serde(default, skip_serializing)]
    pub discord_webhook: Option<String>,
    #[serde(default = "epoch")]
    pub start_time_utc: DateTime<Utc>,
    #[serde(default = "epoch")]
    pub end_time_utc: DateTime<Utc>,
    #[serde(default)]
    pub freeze_time_utc: Option<DateTime<Utc>>,
    #[serde(default = "epoch")]
    pub writeup_deadline: DateTime<Utc>,
    #[serde(default)]
    pub writeup_note: String,
    #[serde(default = "default_blood_bonus")]
    pub blood_bonus_value: i64,
    #[serde(default)]
    pub poster_hash: Option<String>,
    // A&D / KotH tunables (optional; only applied when present).
    #[serde(default)]
    pub ad_warmup_seconds: Option<i32>,
    #[serde(default)]
    pub ad_tick_seconds: Option<i32>,
    #[serde(default)]
    pub ad_flag_lifetime_ticks: Option<i32>,
    #[serde(default)]
    pub ad_reset_cooldown_minutes: Option<i32>,
    #[serde(default)]
    pub ad_getflag_window_fraction: Option<f64>,
    #[serde(default)]
    pub ad_min_grace_period_seconds: Option<i32>,
    #[serde(default)]
    pub ad_allow_snapshot_download: Option<bool>,
    #[serde(default)]
    pub ad_snapshot_retention_days: Option<i32>,
    #[serde(default = "default_ad_epoch_ticks")]
    pub ad_epoch_ticks: i32,
    #[serde(default = "default_koth_epoch_ticks")]
    pub koth_epoch_ticks: i32,
    #[serde(default = "default_koth_cycle_ticks")]
    pub koth_cycle_ticks: i32,
    #[serde(default = "default_koth_champion_cooldown_ticks")]
    pub koth_champion_cooldown_ticks: i32,
    #[serde(default = "default_koth_claim_confirmation_ticks")]
    pub koth_claim_confirmation_ticks: i32,
    #[serde(default)]
    pub vpn_access_required: bool,
    #[serde(default)]
    pub vpn_behavior_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_flag_scan_enabled: bool,
    #[serde(default)]
    pub vpn_provider_dns_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_source_asn_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_device_sharing_telemetry_enabled: bool,
    // Divisions + their per-challenge permission configs. Empty for a
    // single-division game; a legacy package without this field deserializes to
    // an empty vec. Mirrors GameExportService loading Divisions with ChallengeConfigs.
    #[serde(default)]
    pub divisions: Vec<ExportDivisionModel>,
}

fn default_ad_epoch_ticks() -> i32 {
    8
}

fn default_koth_epoch_ticks() -> i32 {
    12
}

fn default_koth_cycle_ticks() -> i32 {
    3
}

fn default_koth_champion_cooldown_ticks() -> i32 {
    1
}

fn default_koth_claim_confirmation_ticks() -> i32 {
    2
}

fn default_ad_scoring_weight() -> f64 {
    1.0
}

/// One division inside `game.json`, with its per-challenge permission overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDivisionModel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub invite_code: Option<String>,
    /// `GamePermission` bit-flags (numeric; the package is rsctf-internal so the
    /// raw int round-trips without RSCTF's string-flag form).
    #[serde(default)]
    pub default_permissions: i32,
    #[serde(default)]
    pub challenge_configs: Vec<ExportDivisionConfigModel>,
}

/// A per-challenge permission override for a division. `challenge_id` is the
/// SOURCE challenge id (matching `ExportChallengeModel.id`); import remaps it to
/// the freshly-allocated challenge id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDivisionConfigModel {
    #[serde(default)]
    pub challenge_id: i32,
    #[serde(default)]
    pub permissions: i32,
}

impl ExportGameModel {
    fn from_game(g: &game::Model) -> Self {
        Self {
            title: g.title.clone(),
            summary: g.summary.clone(),
            content: g.content.clone(),
            hidden: g.hidden,
            practice_mode: g.practice_mode,
            accept_without_review: g.accept_without_review,
            allow_user_submissions: g.allow_user_submissions,
            writeup_required: g.writeup_required,
            invite_code: g.invite_code.clone(),
            team_member_count_limit: g.team_member_count_limit,
            container_count_limit: g.container_count_limit,
            discord_webhook: None,
            start_time_utc: g.start_time_utc,
            end_time_utc: g.end_time_utc,
            freeze_time_utc: g.freeze_time_utc,
            writeup_deadline: g.writeup_deadline,
            writeup_note: g.writeup_note.clone(),
            blood_bonus_value: g.blood_bonus_value,
            poster_hash: g.poster_hash.clone(),
            ad_warmup_seconds: g.ad_warmup_seconds,
            ad_tick_seconds: g.ad_tick_seconds,
            ad_flag_lifetime_ticks: g.ad_flag_lifetime_ticks,
            ad_reset_cooldown_minutes: g.ad_reset_cooldown_minutes,
            ad_getflag_window_fraction: g.ad_getflag_window_fraction,
            ad_min_grace_period_seconds: g.ad_min_grace_period_seconds,
            ad_allow_snapshot_download: Some(g.ad_allow_snapshot_download),
            ad_snapshot_retention_days: g.ad_snapshot_retention_days,
            ad_epoch_ticks: g.ad_epoch_ticks,
            koth_epoch_ticks: g.koth_epoch_ticks,
            koth_cycle_ticks: g.koth_cycle_ticks,
            koth_champion_cooldown_ticks: g.koth_champion_cooldown_ticks,
            koth_claim_confirmation_ticks: g.koth_claim_confirmation_ticks,
            vpn_access_required: g.vpn_access_required,
            vpn_behavior_telemetry_enabled: g.vpn_behavior_telemetry_enabled,
            vpn_flag_scan_enabled: g.vpn_flag_scan_enabled,
            vpn_provider_dns_telemetry_enabled: g.vpn_provider_dns_telemetry_enabled,
            vpn_source_asn_telemetry_enabled: g.vpn_source_asn_telemetry_enabled,
            vpn_device_sharing_telemetry_enabled: g.vpn_device_sharing_telemetry_enabled,
            // Populated by the export handler (needs DB access).
            divisions: Vec::new(),
        }
    }

    fn configuration(&self) -> crate::services::game_config::GameConfiguration {
        crate::services::game_config::GameConfiguration {
            start_time_utc: self.start_time_utc,
            end_time_utc: self.end_time_utc,
            freeze_time_utc: self.freeze_time_utc,
            team_member_count_limit: self.team_member_count_limit,
            container_count_limit: self.container_count_limit,
            ad_warmup_seconds: self.ad_warmup_seconds,
            ad_snapshot_retention_days: self.ad_snapshot_retention_days,
            ad_tick_seconds: self.ad_tick_seconds,
            ad_flag_lifetime_ticks: self.ad_flag_lifetime_ticks,
            ad_reset_cooldown_minutes: self.ad_reset_cooldown_minutes,
            ad_getflag_window_fraction: self.ad_getflag_window_fraction,
            ad_min_grace_period_seconds: self.ad_min_grace_period_seconds,
            ad_epoch_ticks: self.ad_epoch_ticks,
            koth_epoch_ticks: self.koth_epoch_ticks,
            koth_cycle_ticks: self.koth_cycle_ticks,
            koth_champion_cooldown_ticks: self.koth_champion_cooldown_ticks,
            koth_claim_confirmation_ticks: self.koth_claim_confirmation_ticks,
        }
    }
}

/// One flag inside a `challenges/challenge-{id}.json` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFlagModel {
    #[serde(default)]
    pub flag: String,
    #[serde(default)]
    pub attachment_type: Option<FileType>,
    #[serde(default)]
    pub file_hash: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
    /// Original filename of the bundled blob (for the recreated `Files` row).
    #[serde(default)]
    pub file_name: Option<String>,
}

/// `challenges/challenge-{id}.json` payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportChallengeModel {
    /// Source id — used only to name the file; import allocates a fresh id.
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
    #[serde(default = "default_category")]
    pub category: ChallengeCategory,
    #[serde(default = "default_type", rename = "type")]
    pub challenge_type: ChallengeType,
    #[serde(default)]
    pub hints: Option<JsonValue>,
    #[serde(default)]
    pub flag_template: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub container_image: Option<String>,
    #[serde(default)]
    pub network_mode: Option<NetworkMode>,
    #[serde(default)]
    pub memory_limit: Option<i32>,
    #[serde(default)]
    pub storage_limit: Option<i32>,
    #[serde(default, rename = "cpuCount")]
    pub cpu_count: Option<i32>,
    #[serde(default)]
    pub expose_port: Option<i32>,
    #[serde(default)]
    pub workload_spec: Option<JsonValue>,
    #[serde(default)]
    pub deadline_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub submission_limit: i32,
    #[serde(default)]
    pub original_score: i32,
    #[serde(default)]
    pub min_score_rate: f64,
    #[serde(default)]
    pub difficulty: f64,
    #[serde(default = "default_score_curve")]
    pub score_curve: ScoreCurve,
    #[serde(default)]
    pub enable_traffic_capture: bool,
    #[serde(default)]
    pub enable_shared_container: bool,
    #[serde(default = "default_true")]
    pub disable_blood_bonus: bool,
    #[serde(default)]
    pub ad_checker_image: Option<String>,
    #[serde(default)]
    pub ad_allow_egress: bool,
    #[serde(default)]
    pub ad_allow_self_reset: bool,
    #[serde(default)]
    pub ad_ssh_requires_flag: bool,
    #[serde(default)]
    pub ad_self_hosted: bool,
    #[serde(default = "default_ad_scoring_weight")]
    pub ad_scoring_weight: f64,
    #[serde(default = "default_variant_mode")]
    pub variant_mode: ChallengeVariantMode,
    #[serde(default)]
    pub variant_generator_image: Option<String>,
    #[serde(default)]
    pub variant_generator_digest: Option<String>,
    #[serde(default)]
    pub variant_generator_build_context_subdir: Option<String>,
    #[serde(default = "default_challenge_build_status")]
    pub variant_generator_build_status: ChallengeBuildStatus,
    #[serde(default)]
    pub variant_generator_last_build_log: Option<String>,
    #[serde(default = "default_solve_receipt_mode")]
    pub solve_receipt_mode: SolveReceiptMode,
    #[serde(default)]
    pub receipt_verifier_identity: Option<String>,
    // Challenge-level attachment (RSCTF `GameChallenge.Attachment` — the single
    // StaticAttachment blob). Present for any challenge type that carries one.
    #[serde(default)]
    pub attachment_type: Option<FileType>,
    #[serde(default)]
    pub attachment_file_hash: Option<String>,
    #[serde(default)]
    pub attachment_remote_url: Option<String>,
    #[serde(default)]
    pub attachment_file_name: Option<String>,
    #[serde(default)]
    pub flags: Vec<ExportFlagModel>,
}

impl ExportChallengeModel {
    #[allow(clippy::too_many_arguments)]
    fn from_challenge(
        c: &game_challenge::Model,
        flags: Vec<ExportFlagModel>,
        attachment_type: Option<FileType>,
        attachment_file_hash: Option<String>,
        attachment_remote_url: Option<String>,
        attachment_file_name: Option<String>,
    ) -> Self {
        Self {
            id: c.id,
            title: c.title.clone(),
            content: c.content.clone(),
            category: c.category,
            challenge_type: c.challenge_type,
            hints: c.hints.clone(),
            flag_template: c.flag_template.clone(),
            file_name: c.file_name.clone(),
            container_image: c.container_image.clone(),
            network_mode: c.network_mode,
            memory_limit: c.memory_limit,
            storage_limit: c.storage_limit,
            cpu_count: c.cpu_count,
            expose_port: c.expose_port,
            workload_spec: c.workload_spec.clone(),
            deadline_utc: c.deadline_utc,
            submission_limit: c.submission_limit,
            original_score: c.original_score,
            min_score_rate: c.min_score_rate,
            difficulty: c.difficulty,
            score_curve: c.score_curve,
            enable_traffic_capture: c.enable_traffic_capture,
            enable_shared_container: c.enable_shared_container,
            disable_blood_bonus: c.disable_blood_bonus,
            ad_checker_image: c.ad_checker_image.clone(),
            ad_allow_egress: c.ad_allow_egress,
            ad_allow_self_reset: c.ad_allow_self_reset,
            ad_ssh_requires_flag: c.ad_ssh_requires_flag,
            ad_self_hosted: c.ad_self_hosted,
            ad_scoring_weight: c.ad_scoring_weight,
            variant_mode: c.variant_mode,
            variant_generator_image: c.variant_generator_image.clone(),
            variant_generator_digest: c.variant_generator_digest.clone(),
            variant_generator_build_context_subdir: c
                .variant_generator_build_context_subdir
                .clone(),
            variant_generator_build_status: c.variant_generator_build_status,
            variant_generator_last_build_log: c.variant_generator_last_build_log.clone(),
            solve_receipt_mode: c.solve_receipt_mode,
            receipt_verifier_identity: c.receipt_verifier_identity.clone(),
            attachment_type,
            attachment_file_hash,
            attachment_remote_url,
            attachment_file_name,
            flags,
        }
    }
}

fn default_variant_mode() -> ChallengeVariantMode {
    ChallengeVariantMode::Disabled
}

fn default_challenge_build_status() -> ChallengeBuildStatus {
    ChallengeBuildStatus::None
}

fn default_solve_receipt_mode() -> SolveReceiptMode {
    SolveReceiptMode::Disabled
}

/// `POST /api/edit/games/import` — import a game ZIP package (multipart `file`);
/// parses `game.json` + `challenges/*.json` and atomically INSERTs a new hidden
/// game with its challenges and flags. Returns the new game id (raw `number`).
/// Mirrors `GameImportService.ImportGameAsync`.
pub async fn import_game(
    State(st): State<SharedState>,
    _admin: AdminUser,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<i32>> {
    let _upload_reservation =
        crate::utils::upload::reserve_buffered(crate::utils::upload::ARCHIVE_BODY_BYTES)?;
    // Read the uploaded `file` field into memory.
    let mut data: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        if field.name() == Some("file") {
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?;
            data = Some(bytes.to_vec());
            break;
        }
    }
    let bytes = data.ok_or_else(|| AppError::bad_request("No file provided"))?;
    if bytes.is_empty() {
        return Err(AppError::bad_request("File size is zero"));
    }
    if bytes.len() > crate::utils::upload::ARCHIVE_FILE_BYTES {
        return Err(AppError::bad_request("Game archive is too large"));
    }
    // Expand every entry once, under a shared actual-byte budget, before any
    // untrusted JSON can cause database writes.
    let _permit = GAME_IMPORT_SLOTS
        .try_acquire()
        .map_err(|_| AppError::unavailable("Game import capacity is busy; retry shortly"))?;
    let entries = tokio::task::spawn_blocking(move || read_game_import_archive(&bytes))
        .await
        .map_err(|error| AppError::internal(format!("game import task failed: {error}")))??;
    let game_json = read_import_text(&entries, "game.json")?
        .ok_or_else(|| AppError::bad_request("Missing game.json in import package"))?;
    let export_game: ExportGameModel = serde_json::from_str(game_json)
        .map_err(|e| AppError::bad_request(format!("Invalid game.json: {e}")))?;
    export_game.configuration().validate()?;
    let challenge_names: Vec<String> = entries
        .keys()
        .filter(|name| {
            name.starts_with("challenges/") && name.ends_with(".json") && !name.ends_with('/')
        })
        .cloned()
        .collect();
    let mut export_challenges: Vec<ExportChallengeModel> = Vec::new();
    for name in challenge_names {
        let body = read_import_text(&entries, &name)?
            .ok_or_else(|| AppError::bad_request(format!("Missing challenge file: {name}")))?;
        let challenge: ExportChallengeModel = serde_json::from_str(body)
            .map_err(|e| AppError::bad_request(format!("Invalid challenge file {name}: {e}")))?;
        export_challenges.push(challenge);
    }
    // Deterministic order so the imported challenge ids follow the source ids.
    export_challenges.sort_by_key(|c| c.id);
    validate_import_challenges(&export_challenges)?;
    let game_id =
        import_persistence::persist_game_import(&st, &entries, &export_game, &export_challenges)
            .await?;
    Ok(RequestResponse::ok(game_id))
}

fn validate_import_challenges(challenges: &[ExportChallengeModel]) -> AppResult<()> {
    let mut source_challenge_ids = BTreeSet::new();
    for challenge in challenges {
        if !source_challenge_ids.insert(challenge.id) {
            return Err(AppError::bad_request(
                "Game import contains duplicate challenge ids",
            ));
        }
        crate::utils::scoring::validate_challenge_scoring(
            challenge.original_score,
            challenge.min_score_rate,
            challenge.difficulty,
            challenge.submission_limit,
        )?;
        crate::services::container::validate_network_mode_value(
            challenge.challenge_type,
            challenge.network_mode.unwrap_or(NetworkMode::Open),
        )?;
        if !challenge.ad_scoring_weight.is_finite()
            || !(0.8..=1.2).contains(&challenge.ad_scoring_weight)
        {
            return Err(AppError::bad_request(
                "Engine challenge scoring weight must be between 0.8 and 1.2.",
            ));
        }
        if let Some(spec) = challenge.workload_spec.clone() {
            crate::services::challenge_workloads::validate_json_for_challenge(
                challenge.challenge_type,
                spec,
            )?;
        }
    }
    Ok(())
}

/// Expand an import package once before deserializing it. Both per-entry and
/// cumulative limits use bytes actually emitted by the decompressor rather than
/// trusting the ZIP central directory's uncompressed sizes.
fn read_game_import_archive(bytes: &[u8]) -> AppResult<BTreeMap<String, Vec<u8>>> {
    read_game_import_archive_with_limits(bytes, GAME_IMPORT_LIMITS)
}

fn read_game_import_archive_with_limits(
    bytes: &[u8],
    limits: GameImportLimits,
) -> AppResult<BTreeMap<String, Vec<u8>>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| AppError::bad_request(format!("Invalid or corrupted ZIP file: {e}")))?;
    if archive.len() > limits.entries {
        return Err(AppError::bad_request(
            "Game import contains too many entries",
        ));
    }

    let mut total = 0u64;
    let mut names = BTreeSet::new();
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| AppError::bad_request(format!("ZIP read error: {e}")))?;
        let raw_name = entry.name().to_string();
        if raw_name.contains('\\') {
            return Err(AppError::bad_request(
                "Game import path contains a backslash",
            ));
        }
        let is_directory = entry.is_dir();
        let path = entry
            .enclosed_name()
            .ok_or_else(|| AppError::bad_request("Game import contains an unsafe path"))?;
        let mut components = Vec::new();
        for component in path.components() {
            let std::path::Component::Normal(component) = component else {
                return Err(AppError::bad_request("Game import path is not canonical"));
            };
            components.push(
                component
                    .to_str()
                    .ok_or_else(|| AppError::bad_request("Game import path is not valid UTF-8"))?,
            );
        }
        if components.is_empty() {
            return Err(AppError::bad_request("Game import path is not canonical"));
        }
        if components.len() > limits.path_components {
            return Err(AppError::bad_request("Game import path is too deep"));
        }
        let name = components.join("/");
        let canonical_name = if is_directory {
            format!("{name}/")
        } else {
            name.clone()
        };
        if raw_name != canonical_name {
            return Err(AppError::bad_request("Game import path is not canonical"));
        }
        if !names.insert(name.clone()) {
            return Err(AppError::bad_request(
                "Game import contains duplicate entries",
            ));
        }
        if entry.size() > limits.file_bytes {
            return Err(AppError::bad_request("Game import entry is too large"));
        }
        let compressed = entry.compressed_size().max(1);
        if entry.size() > compressed.saturating_mul(limits.compression_ratio) {
            return Err(AppError::bad_request(
                "Game import entry compression ratio is too high",
            ));
        }

        let remaining = limits.total_bytes.saturating_sub(total);
        let max_read = remaining.min(limits.file_bytes);
        let mut body = Vec::new();
        std::io::Read::take(&mut entry, max_read + 1)
            .read_to_end(&mut body)
            .map_err(|e| AppError::bad_request(format!("ZIP read error for {name}: {e}")))?;
        if body.len() as u64 > max_read {
            return Err(AppError::bad_request(
                "Game import expands beyond the size limit",
            ));
        }
        let actual_size = body.len() as u64;
        if actual_size > compressed.saturating_mul(limits.compression_ratio) {
            return Err(AppError::bad_request(
                "Game import entry compression ratio is too high",
            ));
        }
        total = total.saturating_add(actual_size);

        if is_directory {
            if !body.is_empty() {
                return Err(AppError::bad_request(
                    "Game import directory entry contains data",
                ));
            }
            continue;
        }
        entries.insert(name, body);
    }
    Ok(entries)
}

fn read_import_text<'a>(
    entries: &'a BTreeMap<String, Vec<u8>>,
    name: &str,
) -> AppResult<Option<&'a str>> {
    entries
        .get(name)
        .map(|body| {
            std::str::from_utf8(body)
                .map_err(|e| AppError::bad_request(format!("Invalid UTF-8 in {name}: {e}")))
        })
        .transpose()
}

#[cfg(test)]
#[path = "transfer_archive_tests.rs"]
mod archive_tests;

#[path = "transfer_export.rs"]
mod export;
pub use export::export_game;

#[path = "transfer_import.rs"]
mod import_persistence;
