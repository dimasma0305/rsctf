//! services/git_sync.rs — git challenge-repo sync.
//!
//! Ported from RSCTF `Services/Transfer/GitRepoSyncService.cs` and its
//! background driver `RepoBindingScanService.cs`, but implemented WITHOUT a git
//! library — we shell out to the system `git` binary via
//! [`tokio::process::Command`]. This keeps the dependency surface small (git is
//! already present in the runtime image) and matches upstream, which also drives
//! git as a subprocess rather than linking libgit2.
//!
//! # RSCTF repo-binding scan flow
//!
//! An operator registers a *repo binding*: a GitHub repository (owner/repo, plus
//! an optional branch/tag/commit ref) whose tree contains one or more challenge
//! manifests. A background poller — RSCTF's `RepoBindingScanService`, a 30-second
//! ticking [`BackgroundService`] — walks every binding whose `NextScanUtc` has
//! elapsed and, for each, performs:
//!
//! 1. **Sync** — [`sync_repo`] shallow-clones the repo into a persistent
//!    per-binding checkout under a repo-cache root (`/app/repos/{kind}/{id}` in
//!    the original), or, when the checkout already exists, does a
//!    `fetch --depth 1` + `reset --hard FETCH_HEAD` so the steady-state poll is a
//!    small delta instead of a full re-download. Stale `*.lock` files left by a
//!    git process that was killed mid-operation (timeout, container restart) are
//!    swept first, since nothing legitimately holds them once the caller owns the
//!    per-binding lock.
//! 2. **Discover** — [`discover_challenges`] walks the checked-out tree for
//!    `challenge.yml` / `challenge.yaml` manifests. Each manifest is one
//!    challenge definition.
//! 3. **Import** — each manifest is deserialized and upserted into a
//!    `GameChallenge`, keyed by `(BindingId, manifest path)` so re-scanning an
//!    unchanged repo is idempotent and preserves its challenge identity.
//!
//! Faults are isolated per binding and `NextScanUtc` is always advanced (even on
//! failure) so a broken target can't hot-loop the poller. That scheduling policy
//! belongs to the background service; this module provides the pure sync +
//! discovery primitives it calls.
//!
//! # Auth
//!
//! Private repos need a GitHub PAT. [`GitCredentials`] rewrites an `https://` URL
//! to embed `x-access-token:<pat>` as HTTP Basic userinfo — GitHub's smart-HTTP
//! transport expects Basic auth with the documented user `x-access-token` and the
//! PAT as the password (a Bearer token works for the REST API but makes git fall
//! through to a credential prompt). Any embedded credential is scrubbed from
//! error messages before they reach a caller or log.

use std::path::Path;
#[cfg(test)]
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseBackend, EntityTrait, Set, Statement,
    TransactionTrait,
};
use serde::Deserialize;

use crate::app_state::SharedState;
use crate::models::data::{flag_context, game, game_challenge};
use crate::utils::enums::{
    ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType, NetworkMode, ScoreCurve,
};
use crate::utils::error::{AppError, AppResult};

mod checker;
use checker::{
    checker_dest_dir, checker_source_dir, cleanup_unpublished_checker, prepare_checker_venv,
    validate_checker_source,
};
mod checker_gc;
pub(crate) use checker_gc::acquire_checker_execution_lease;
pub use checker_gc::collect_stale_checker_revisions;

/// Keep immutable checker publication and the conservative revision collector
/// mutually exclusive across replicas. Callers hold this through the database
/// write that makes the published path reachable.
pub(crate) async fn acquire_checker_artifact_guard(
    st: &SharedState,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    checker_gc::acquire_checker_artifact_guard(st.pg()).await
}

mod reviewed;
pub(crate) use reviewed::prepare_checker as prepare_reviewed_checker;

mod git;
use git::run_git;
pub use git::{
    head_sha, lock_checkout, lock_checkout_distributed, sync_repo, validate_binding_repo_url,
    validate_git_ref, validate_github_repo_url, CheckoutLockGuard, GitCredentials,
};
#[cfg(test)]
use git::{url_without_credentials, validate_checkout_tree, validate_sync_repo_url};
mod package;
use package::{find_dockerfile_context, image_tag, parse_enum, resolve_category, zip_context_dir};
mod discovery;
pub use discovery::{discover_challenges, discover_events};
mod repository;
use repository::find_repository_challenge;
pub(crate) use repository::{manifest_candidate_in_checkout, tombstone_missing_challenges};
mod runtime;
use runtime::{live_runtime_update_deferred, LiveRuntimeIntent};
mod grading;
use grading::{grading_fence_locked, GradingIntent};

/// Whether an import may run executable preparation while ingesting its manifest.
/// User submissions must remain inert until a separate, isolated approval worker
/// exists; trusted manager/repository imports preserve the existing inline flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPolicy {
    PendingReview,
    Trusted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestImportResult {
    pub challenge_id: i32,
    pub created: bool,
    pub build_queued: bool,
    pub runtime_update_deferred: bool,
    pub grading_update_deferred: bool,
    pub attachment_synced: bool,
}

impl ImportPolicy {
    fn review_status(self) -> ChallengeReviewStatus {
        match self {
            Self::PendingReview => ChallengeReviewStatus::Pending,
            Self::Trusted => ChallengeReviewStatus::Active,
        }
    }

    fn reviewed_at(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        matches!(self, Self::Trusted).then_some(now)
    }

    fn may_execute(self) -> bool {
        matches!(self, Self::Trusted)
    }
}

/// Persist a replica-independent source identity only when the manifest resolves
/// inside this game's binding-owned checkout. Temporary ZIP/GitHub imports store
/// no path because their request-scoped directories are removed after import.
fn durable_repo_manifest_path(
    storage_root: &str,
    binding_id: Option<i32>,
    manifest: &Path,
) -> Option<String> {
    let binding_id = binding_id?;
    let checkout = std::fs::canonicalize(
        Path::new(storage_root)
            .join("repos")
            .join(binding_id.to_string()),
    )
    .ok()?;
    let manifest = std::fs::canonicalize(manifest).ok()?;
    (manifest.is_file() && manifest.starts_with(&checkout))
        .then(|| manifest.strip_prefix(&checkout).ok())
        .flatten()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| {
            repository::scoped_manifest_identity(
                binding_id,
                &relative.to_string_lossy().replace('\\', "/"),
            )
        })
}

const MAX_REPO_ENTRIES: usize = 4_096;
const MAX_REPO_FILES: usize = 2_048;
const MAX_REPO_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_REPO_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPO_DEPTH: usize = 32;

/// In-memory shape of one `.gzevent` event manifest, mirroring RSCTF
/// `Models/Request/Edit/GzEventModel`. Every field is optional (a sparse
/// manifest only seeds what it names); nested keys are camelCase. Used at
/// game-CREATE time only — a re-scan never re-applies these over operator edits.
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

/// The `ad:` section of a `.gzevent` — event-wide Attack & Defense knobs, each
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
    let raw = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| AppError::internal(format!("git_sync: read {}: {e}", path.display())))?;
    serde_norway::from_str(&raw)
        .map_err(|e| AppError::bad_request(format!("invalid .gzevent: {e}")))
}

/// In-memory shape of one `challenge.yml` / `challenge.yaml` file, mirroring
/// RSCTF `Models/Request/Edit/ChallengeYamlModel` — the subset of the gzcli
/// template schema that maps onto a `GameChallenge`.
///
/// Aliases match the upstream (camelCase for nested fields). Unrecognized keys
/// are ignored (serde's default) and every field is optional (`Option` missing
/// ⇒ `None`), so a sparse manifest only sets what it names.
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
    /// When true the challenge opts out of sync entirely — never created.
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
    pub container: Option<ContainerSection>,
    /// Attack-&-Defense / King-of-the-Hill block — only consulted when the
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

/// Parse a `challenge.yml` / `challenge.yaml` manifest and persist the resulting
/// `GameChallenge` (plus its static `FlagContext` rows) under `game_id`.
///
/// Ports RSCTF `ChallengeImportService.ImportOneAsync` +
/// `ChallengeRepository.CreateChallenge`: deserialize the yaml
/// ([`ChallengeYaml`], mirroring `ChallengeYamlSerializer`'s model), map it onto
/// the flattened challenge/base fields, then persist. Repository-backed rows
/// are updated in place by durable manifest path so submissions and solve state
/// retain the same challenge id across scans.
///
/// Repository callers must hold the shared per-game A&D/KotH configuration
/// lock while importing their sorted manifest set. Both repository scans and
/// archive imports do so, serializing legacy identity adoption across replicas.
/// [`ImportPolicy`] establishes a new row's review state and decides whether
/// build/checker preparation may run; an update preserves its established
/// review and enabled state.
///
/// Errors: a missing/empty `name`, an unknown `type`, `ignore: true`, or a
/// nonexistent `game_id` all map to [`AppError::bad_request`]; a yaml parse
/// failure or a DB error maps through [`AppError`].
pub async fn import_manifest(
    st: &SharedState,
    game_id: i32,
    manifest: &Path,
    policy: ImportPolicy,
) -> AppResult<ManifestImportResult> {
    // Fail early with a friendly message rather than surfacing an FK violation
    // from the INSERT below.
    let game = game::Entity::find_by_id(game_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found(format!("game {game_id} not found")))?;
    let game_deletion_pending: bool =
        sqlx::query_scalar(r#"SELECT deletion_pending FROM "Games" WHERE id = $1"#)
            .bind(game_id)
            .fetch_optional(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::not_found(format!("game {game_id} not found")))?;
    if game_deletion_pending {
        return Err(AppError::conflict("Game is being deleted"));
    }

    let raw = tokio::fs::read_to_string(manifest)
        .await
        .map_err(|e| AppError::internal(format!("git_sync: read {}: {e}", manifest.display())))?;
    let model: ChallengeYaml = serde_norway::from_str(&raw)
        .map_err(|e| AppError::bad_request(format!("invalid challenge.yaml: {e}")))?;

    let name = model
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("challenge.yaml missing 'name'"))?
        .to_string();

    // `ignore: true` opts the challenge out of sync entirely — never created.
    if model.ignore == Some(true) {
        return Err(AppError::bad_request(format!(
            "'{name}' has ignore: true — it is not synced; any existing challenge is retained, and repository removal requires deleting the manifest"
        )));
    }

    let raw_type = model.challenge_type.as_deref().unwrap_or("");
    let challenge_type = parse_enum::<ChallengeType>(raw_type)
        .ok_or_else(|| AppError::bad_request(format!("unknown challenge type '{raw_type}'")))?;
    let source_yaml_path =
        durable_repo_manifest_path(&st.config.storage_root, game.repo_binding_id, manifest);
    let existing = find_repository_challenge(
        st,
        game_id,
        game.repo_binding_id,
        source_yaml_path.as_deref(),
    )
    .await?;
    if existing
        .as_ref()
        .is_some_and(|challenge| challenge.challenge_type != challenge_type)
    {
        return Err(AppError::bad_request(
            "repository sync cannot change an existing challenge type; create a new manifest path instead",
        ));
    }
    if challenge_type == ChallengeType::KingOfTheHill {
        crate::services::ad_engine::koth_cycle::validate_crown_shape(
            game.koth_epoch_ticks,
            game.koth_cycle_ticks,
            game.koth_champion_cooldown_ticks,
            game.koth_claim_confirmation_ticks,
        )
        .map_err(|_| AppError::bad_request("Invalid KotH crown-cycle settings."))?;
    }

    // The package directory is the manifest's parent; category is inferred from
    // the enclosing directory names when not stated explicitly.
    let package_dir = manifest.parent().unwrap_or_else(|| Path::new("."));
    let category = resolve_category(model.category.as_deref(), package_dir);

    // Author is folded into the content body ("Author: **X**\n\n...") exactly as
    // RSCTF's ApplyYamlToChallenge does, so a later re-export round-trips.
    let description = model.description.unwrap_or_default();
    let content = match model
        .author
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(author) => format!("Author: **{author}**\n\n{description}"),
        None => description,
    };

    let hints = match &model.hints {
        Some(list) if !list.is_empty() => Some(
            serde_json::to_value(list)
                .map_err(|e| AppError::internal(format!("git_sync: encode hints: {e}")))?,
        ),
        _ => None,
    };

    // Scoring: clamp to the same bounds the API PUT enforces so an out-of-range
    // manifest can't invert the dynamic-score decay curve. Defaults match the
    // GameChallenge entity init (0.25 / 5).
    let requested_min_score_rate = model.min_score_rate.unwrap_or(0.25);
    let min_score_rate = if requested_min_score_rate.is_finite() {
        requested_min_score_rate.clamp(0.0, 1.0)
    } else {
        0.25
    };
    let requested_difficulty = model.difficulty.unwrap_or(5.0);
    let difficulty = if requested_difficulty.is_finite() && requested_difficulty > 0.0 {
        requested_difficulty
    } else {
        5.0
    };
    let submission_limit = model.submission_limit.unwrap_or(0).max(0);

    let container = model.container.as_ref();
    let flag_template = container
        .and_then(|c| c.flag_template.clone())
        .or(model.flag_template.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Container fields only apply to container-typed challenges. `provide:`
    // attachments and the image auto-build pipeline are separate slices.
    let is_container = challenge_type.is_container();
    let preserve_live_runtime = existing
        .as_ref()
        .is_some_and(|challenge| challenge.is_enabled && challenge.challenge_type.is_container());
    let declared_container_image = if is_container {
        container
            .and_then(|c| c.container_image.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let memory_limit = if is_container {
        container.and_then(|c| c.memory_limit)
    } else {
        None
    };
    let cpu_count = if is_container {
        container.and_then(|c| c.cpu_count)
    } else {
        None
    };
    let storage_limit = if is_container {
        container.and_then(|c| c.storage_limit)
    } else {
        None
    };
    let expose_port = if is_container {
        container.and_then(|c| c.expose_port)
    } else {
        None
    };
    let enable_traffic_capture = is_container
        && container
            .and_then(|c| c.enable_traffic_capture)
            .unwrap_or(false);
    // Shared container only makes sense for StaticContainer (single static flag).
    let enable_shared_container = challenge_type == ChallengeType::StaticContainer
        && container
            .and_then(|c| c.enable_shared_container)
            .unwrap_or(false);
    let network_mode = is_container.then_some(
        container
            .and_then(|c| c.network_mode)
            .unwrap_or(NetworkMode::Open),
    );

    // A&D-engine knobs. Egress is deny-by-default; self-reset retains the
    // upstream default. A sparse manifest must opt into outbound access.
    let ad = model.ad.as_ref();
    let uses_ad = challenge_type.uses_ad_engine();
    let mut requested_static_flags = if uses_ad {
        Vec::new()
    } else {
        model
            .flags
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|flag| flag.trim().to_string())
            .filter(|flag| !flag.is_empty())
            .collect::<Vec<_>>()
    };
    requested_static_flags.sort();
    requested_static_flags.dedup();
    let disable_blood_bonus = model.disable_blood_bonus.unwrap_or(false);
    let declared_checker_image = if uses_ad {
        ad.and_then(|a| a.checker_image.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };
    let ad_allow_egress = ad.and_then(|a| a.allow_egress).unwrap_or(false);
    let ad_allow_self_reset = ad.and_then(|a| a.allow_self_reset).unwrap_or(true);
    let ad_ssh_requires_flag = ad.and_then(|a| a.ssh_requires_flag).unwrap_or(false);
    let ad_self_hosted = ad.and_then(|a| a.self_hosted).unwrap_or(false);
    if declared_checker_image
        .as_deref()
        .is_some_and(|value| !value.contains("{{"))
    {
        return Err(AppError::bad_request(
            "container-based ad.checkerImage references are not supported; include checker/run.py for the process sandbox",
        ));
    }

    // --- Auto-build resolution (port of ChallengeImportService.ResolveBuildIntent
    // + the ./checker auto-build). A container/AD challenge that names no usable
    // registry image but ships a Dockerfile in the conventional spot gets its image
    // AUTO-BUILT: generate a deterministic tag, persist the complete package plus
    // its Docker context selector (so source provenance and rebuild bytes stay
    // together), mark it Queued, and fire the build seam after INSERT. Without this,
    // imported container challenges land with container_image=NULL / build_status=None
    // and can never spawn. `is_container()` covers StaticContainer/DynamicContainer AND
    // AttackDefense/KingOfTheHill, so the A&D service / KotH hill image builds here too.
    let mut container_image = declared_container_image.clone();
    let mut build_status = ChallengeBuildStatus::None;
    // Pending uploads disappear when the request ends. Persist their complete
    // package before INSERT so a later reviewer can prepare the exact immutable
    // checker they inspected. Trusted repository imports publish their checker
    // inline, then retain the same complete package when a local image is built.
    let mut archive_package: Option<Vec<u8>> = if policy == ImportPolicy::PendingReview {
        Some(zip_context_dir(package_dir).await?)
    } else {
        None
    };
    let mut queue_challenge_build = false;
    let mut build_context_subdir = None;
    // Build locally when the operator named no registry image, or used a gzcli-style
    // "{{.slug}}:latest" template placeholder ("build whatever Dockerfile is here").
    // A concrete registry ref (nginx:alpine, ghcr.io/foo:tag) is resolved by the
    // pull seam below rather than built from local source.
    let wants_local_source = is_container
        && declared_container_image
            .as_deref()
            .map(|s| s.contains("{{"))
            .unwrap_or(true);
    let local_build_context = wants_local_source
        .then(|| find_dockerfile_context(package_dir))
        .flatten();
    if let Some(ctx_dir) = local_build_context.as_ref() {
        if ctx_dir == &package_dir.join("src") {
            // Pending review retains the complete immutable package for checker
            // preparation and audit. The builder deterministically selects this
            // subtree without replacing the source blob.
            build_context_subdir = Some("src".to_string());
        } else {
            build_context_subdir = Some(".".to_string());
        }
        container_image = Some(image_tag(game_id, &name));
        if !preserve_live_runtime {
            if policy.may_execute() {
                // Preserve the complete reviewed package, not only the Docker
                // subtree. `build_context_subdir` selects the exact bytes sent
                // to Docker while audit/checker provenance remains available.
                archive_package = Some(zip_context_dir(package_dir).await?);
            }
            build_status = ChallengeBuildStatus::Queued;
            queue_challenge_build = true;
        }
    }
    // Concrete registry tags are mutable too. Pull them through the same build
    // seam so Docker resolves and persists the exact repository digest before a
    // reviewed runtime may execute the challenge.
    if is_container
        && !preserve_live_runtime
        && container_image
            .as_deref()
            .is_some_and(|image| !image.trim().is_empty())
    {
        build_status = ChallengeBuildStatus::Queued;
        queue_challenge_build = true;
    }

    // A&D/KotH functional checker: prepare an isolated venv from ./checker/ (no
    // Docker), optionally installing constrained wheel-only requirements.
    // `ad_checker_image` now holds the prepared checker DIRECTORY
    // (`<dir>/venv/bin/python3` + `<dir>/src/run.py`) that the run path
    // sandbox-execs. A newly pending row deliberately stores no executable path:
    // its source is the durable archive above, and approval publishes a reviewed
    // immutable revision before activation. If its image build fails, the valid
    // path may remain on the still-inert row so a retry can reuse it safely.
    let checker_source = uses_ad
        .then(|| checker_source_dir(&package_dir.join("checker")))
        .flatten();
    if declared_checker_image.is_some() && checker_source.is_none() {
        return Err(AppError::bad_request(
            "ad.checkerImage requests a local checker but checker/run.py is missing",
        ));
    }
    if let Some(source) = checker_source.as_deref() {
        validate_checker_source(source).await?;
    }
    let runtime_update_deferred = match existing.as_ref().filter(|_| preserve_live_runtime) {
        Some(challenge) => {
            live_runtime_update_deferred(
                st,
                challenge,
                &LiveRuntimeIntent {
                    container_image: container_image.as_deref(),
                    declared_container_image: declared_container_image.as_deref(),
                    memory_limit,
                    storage_limit,
                    cpu_count,
                    expose_port,
                    flag_template: flag_template.as_deref(),
                    build_context_subdir: build_context_subdir.as_deref(),
                    local_build_context: local_build_context.as_deref(),
                    checker_source: checker_source.as_deref(),
                    static_flags: &requested_static_flags,
                    enable_traffic_capture,
                    enable_shared_container,
                    network_mode,
                    ad_allow_egress,
                    ad_allow_self_reset,
                    ad_ssh_requires_flag,
                    ad_self_hosted,
                },
            )
            .await?
        }
        None => false,
    };
    let checker_prep = if policy.may_execute() && !preserve_live_runtime {
        checker_source.map(|source| {
            (
                checker_dest_dir(Path::new(&st.config.storage_root), game_id, &name),
                source,
            )
        })
    } else {
        None
    };
    let mut ad_checker_image = None;
    let mut checker_artifact_guard = None;

    // A trusted challenge is Active at INSERT time. Publish its immutable checker
    // revision first so no engine replica can observe a path while it is being
    // copied or while its venv is still under construction. Pending-review
    // imports remain inert and deliberately execute no checker preparation.
    let checker_prepared = if let Some((dest, src_dir)) = checker_prep.as_ref() {
        let guard = match acquire_checker_artifact_guard(st).await {
            Ok(guard) => guard,
            Err(error) => {
                return Err(error);
            }
        };
        if let Err(error) = prepare_checker_venv(dest, src_dir).await {
            if let Err(release_error) = guard.release().await {
                tracing::warn!(%release_error, "checker publication guard release failed");
            }
            return Err(error);
        }
        ad_checker_image = Some(dest.clone());
        checker_artifact_guard = Some(guard);
        true
    } else {
        false
    };

    let now = Utc::now();
    let is_update = existing.is_some();
    // The scan already holds the per-game configuration lock, matching runtime
    // edits' game -> definition order. Definition-only build/attachment work
    // can still be in flight, so fail quickly rather than stretching the game
    // fence across an unbounded wait.
    let mut definition_lock = None;
    if let Some(challenge) = existing.as_ref() {
        let acquired = crate::services::challenge_workloads::try_acquire_definition_lock(
            st.pg(),
            game_id,
            challenge.id,
        )
        .await;
        let mut lock = match acquired {
            Ok(Some(lock)) => lock,
            Ok(None) => {
                cleanup_unpublished_checker(
                    checker_prepared,
                    checker_prep.as_ref(),
                    &mut checker_artifact_guard,
                )
                .await;
                return Err(AppError::conflict(
                    "challenge definition is being updated; retry the repository scan",
                ));
            }
            Err(error) => {
                cleanup_unpublished_checker(
                    checker_prepared,
                    checker_prep.as_ref(),
                    &mut checker_artifact_guard,
                )
                .await;
                return Err(error);
            }
        };
        // The retained game + definition advisory locks are the mutation
        // fence. Do not take a row lock here: the enum-rich model update below
        // intentionally uses a separate SeaORM transaction, and a FOR SHARE
        // lock on this transaction would self-deadlock that UPDATE.
        let deletion_pending = sqlx::query_scalar::<_, bool>(
            r#"SELECT deletion_pending
                  FROM "GameChallenges"
                 WHERE id = $1 AND game_id = $2"#,
        )
        .bind(challenge.id)
        .bind(game_id)
        .fetch_optional(&mut **lock.transaction_mut())
        .await;
        let deletion_pending = match deletion_pending {
            Ok(value) => value,
            Err(error) => {
                cleanup_unpublished_checker(
                    checker_prepared,
                    checker_prep.as_ref(),
                    &mut checker_artifact_guard,
                )
                .await;
                let _ = lock.release().await;
                return Err(AppError::internal(error.to_string()));
            }
        };
        if deletion_pending != Some(false) {
            cleanup_unpublished_checker(
                checker_prepared,
                checker_prep.as_ref(),
                &mut checker_artifact_guard,
            )
            .await;
            lock.release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return match deletion_pending {
                Some(true) => Err(AppError::conflict("Challenge is being deleted")),
                _ => Err(AppError::not_found("Challenge not found")),
            };
        }
        definition_lock = Some(lock);
    }
    let mut am = existing
        .clone()
        .map(game_challenge::ActiveModel::from)
        .unwrap_or_default();
    am.game_id = Set(game_id);
    am.title = Set(name);
    am.content = Set(content);
    am.category = Set(category);
    am.challenge_type = Set(challenge_type);
    am.hints = Set(hints);
    // Record the manifest's durable path so later scans update this same row.
    // Existing repository rows keep their primary key, solve aggregates,
    // submissions, enabled/review state, and live runtime ownership fields.
    am.source_yaml_path = Set(source_yaml_path);
    if !preserve_live_runtime {
        am.container_image = Set(container_image);
        am.memory_limit = Set(memory_limit);
        am.storage_limit = Set(storage_limit);
        am.cpu_count = Set(cpu_count);
        am.expose_port = Set(expose_port);
        am.build_status = Set(build_status);
        am.build_image_digest = Set(None);
        am.last_build_log = Set(None);
        am.build_context_subdir = Set(build_context_subdir);
        am.enable_traffic_capture = Set(enable_traffic_capture);
        am.enable_shared_container = Set(enable_shared_container);
        am.network_mode = Set(network_mode);
        am.ad_checker_image = Set(ad_checker_image);
        am.ad_allow_egress = Set(ad_allow_egress);
        am.ad_allow_self_reset = Set(ad_allow_self_reset);
        am.ad_ssh_requires_flag = Set(ad_ssh_requires_flag);
        am.ad_self_hosted = Set(ad_self_hosted);
    }
    if !is_update {
        am.is_enabled = Set(false);
        am.accepted_count = Set(0);
        am.submission_count = Set(0);
        // Establish review state at INSERT time. A user submission must never
        // be transiently Active while its untrusted side effects run.
        am.review_status = Set(policy.review_status());
        am.reviewed_at_utc = Set(policy.reviewed_at(now));
        am.submitted_at_utc = Set(Some(now));
    }

    // Persist the row and its static flags together. On an update the loaded
    // ActiveModel carries the original id and progress fields, so no FK cascade
    // can erase Submissions or FirstSolves during a repository scan.
    // SeaORM is deliberately retained for this write: converting the complete
    // enum-rich challenge row to raw SQL would duplicate its field mapping and
    // lose the safe loaded-model merge that preserves fields not owned by YAML.
    let grading_intent = GradingIntent {
        submission_limit,
        disable_blood_bonus,
        original_score: 1000,
        min_score_rate,
        difficulty,
        score_curve: ScoreCurve::Standard,
        flag_template: flag_template.as_deref(),
        static_flags: &requested_static_flags,
    };
    let persisted: AppResult<(game_challenge::Model, bool)> = async {
        let transaction = st.db.begin().await?;
        if existing
            .as_ref()
            .is_some_and(|challenge| !challenge.challenge_type.uses_ad_engine())
        {
            crate::utils::scoring::lock_jeopardy_flags_exclusive_orm(
                &transaction,
                existing.as_ref().expect("update has an existing row").id,
            )
            .await?;
        }
        // This is the authoritative start/evidence decision. It runs after the
        // submit-side exclusive grading fence and immediately before the write,
        // so slow packaging/checker preparation cannot leave a stale pre-start
        // decision capable of changing live or historical grading.
        let grading_fence =
            grading_fence_locked(&transaction, game_id, existing.as_ref(), &grading_intent).await?;
        if !grading_fence.protected {
            am.submission_limit = Set(submission_limit);
            am.disable_blood_bonus = Set(disable_blood_bonus);
            am.original_score = Set(1000);
            am.min_score_rate = Set(min_score_rate);
            am.difficulty = Set(difficulty);
            am.score_curve = Set(ScoreCurve::Standard);
            if !preserve_live_runtime {
                am.flag_template = Set(flag_template.clone());
            }
        }
        let challenge = if is_update {
            am.update(&transaction).await?
        } else {
            am.insert(&transaction).await?
        };
        if is_update && !preserve_live_runtime && !grading_fence.protected {
            // Dynamic/runtime flags are ownership records, not repository
            // policy. Replace only unoccupied flags that no GameInstance still
            // references, all under the submit-side exclusive grading fence.
            transaction
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    r#"DELETE FROM "FlagContexts" flag
                        WHERE flag.challenge_id = $1
                          AND flag.is_occupied = FALSE
                          AND NOT EXISTS (
                              SELECT 1 FROM "GameInstances" instance
                               WHERE instance.flag_id = flag.id
                          )"#,
                    [challenge.id.into()],
                ))
                .await?;
        }
        if !preserve_live_runtime && !grading_fence.protected {
            for flag in &requested_static_flags {
                flag_context::ActiveModel {
                    flag: Set(flag.clone()),
                    is_occupied: Set(false),
                    challenge_id: Set(Some(challenge.id)),
                    ..Default::default()
                }
                .insert(&transaction)
                .await?;
            }
        }
        transaction.commit().await?;
        Ok((challenge, grading_fence.update_deferred))
    }
    .await;
    let (challenge, grading_update_deferred) = match persisted {
        Ok(result) => result,
        Err(error) => {
            cleanup_unpublished_checker(
                checker_prepared,
                checker_prep.as_ref(),
                &mut checker_artifact_guard,
            )
            .await;
            if let Some(lock) = definition_lock.take() {
                let _ = lock.release().await;
            }
            return Err(error);
        }
    };

    // Keep the content-addressed file reference, owning challenge row, and old
    // reference release in one SQL transaction. Holding the definition fence
    // through this swap prevents a build from observing the short interval in
    // which the metadata update has committed but the archive owner has not.
    if !preserve_live_runtime {
        let archive = archive_package
            .as_deref()
            .map(|bytes| ("challenge-source.zip", bytes));
        if let Err(error) = crate::services::blob_refs::store_and_replace_challenge_archive(
            st.pg(),
            st.storage.as_ref(),
            challenge.id,
            archive,
        )
        .await
        {
            if let Some(lock) = definition_lock.take() {
                let _ = lock.release().await;
            }
            if let Some(guard) = checker_artifact_guard.take() {
                let _ = guard.release().await;
            }
            return Err(error);
        }
    }
    if let Some(guard) = checker_artifact_guard.take() {
        if let Err(error) = guard.release().await {
            tracing::warn!(%error, "checker publication guard release failed");
        }
    }

    // Attach the challenge's provided artifact — the RSCTF `provide:` path, or the
    // TCP1P `dist/` convention when it's absent. Retain the same definition
    // fence through replacement/removal so an interactive edit cannot race it.
    let attachment_synced = sync_attachment(
        st,
        challenge.id,
        package_dir,
        model.provide.as_deref(),
        is_update,
    )
    .await;
    if let Some(lock) = definition_lock.take() {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }

    Ok(ManifestImportResult {
        challenge_id: challenge.id,
        created: !is_update,
        build_queued: policy.may_execute() && queue_challenge_build,
        runtime_update_deferred,
        grading_update_deferred,
        attachment_synced,
    })
}

/// Push-back: regenerate a challenge's `challenge.yml` from its DB row and
/// git-push it upstream (RSCTF `ChallengeYamlSerializer.Serialize` +
/// `GitRepoSyncService.CommitAndPushCoreAsync`, driven by
/// `EditController.TryPushBackAsync`). See [`push_back`].
mod attach;
pub use attach::repair_missing_attachments;
use attach::sync_attachment;
mod push_back;
pub(crate) use push_back::serialize_challenge_preserving_source;
pub use push_back::{push_file, serialize_challenge};

#[cfg(test)]
mod tests;
