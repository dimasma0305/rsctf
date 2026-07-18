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
//!    unchanged repo is idempotent. See the TODO on [`import_manifest`].
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

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde::Deserialize;

use crate::app_state::SharedState;
use crate::models::data::{flag_context, game, game_challenge};
use crate::utils::enums::{ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType, ScoreCurve};
use crate::utils::error::{AppError, AppResult};

mod checker;
use checker::{checker_dest_dir, checker_source_dir, prepare_checker_venv};
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

/// Whether an import may run executable preparation while ingesting its manifest.
/// User submissions must remain inert until a separate, isolated approval worker
/// exists; trusted manager/repository imports preserve the existing inline flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPolicy {
    PendingReview,
    Trusted,
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

async fn cleanup_unpublished_archive(st: &SharedState, hash: Option<&str>) {
    let Some(hash) = hash else {
        return;
    };
    if let Err(error) =
        crate::services::blob_refs::release_and_purge(st.pg(), st.storage.as_ref(), hash).await
    {
        tracing::warn!(%error, %hash, "git_sync: unpublished source archive cleanup failed");
    }
}

/// Persist a source path only when it resolves inside this game's shared,
/// binding-owned checkout. Temporary ZIP/GitHub imports deliberately store no
/// path because their request-scoped directories are removed after import.
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
        .then(|| manifest.to_string_lossy().into_owned())
}

const MAX_REPO_ENTRIES: usize = 4_096;
const MAX_REPO_FILES: usize = 2_048;
const MAX_REPO_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_REPO_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPO_DEPTH: usize = 32;

/// Walk the tree rooted at `dir` and return every `challenge.yml` /
/// `challenge.yaml` manifest path, sorted for deterministic output.
///
/// The `.git` directory is skipped. Traversal is iterative (an explicit stack)
/// rather than recursive to avoid boxing an async recursion.
pub async fn discover_challenges(dir: &Path) -> AppResult<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|e| {
            AppError::internal(format!("git_sync: read_dir {}: {e}", current.display()))
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            AppError::internal(format!(
                "git_sync: read dir entry in {}: {e}",
                current.display()
            ))
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|e| {
                AppError::internal(format!("git_sync: stat {}: {e}", path.display()))
            })?;

            if file_type.is_dir() {
                // Never descend into the git metadata dir.
                if path.file_name() == Some(OsStr::new(".git")) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                if let Some(name) = path.file_name().and_then(OsStr::to_str) {
                    if name == "challenge.yml" || name == "challenge.yaml" {
                        manifests.push(path);
                    }
                }
            }
        }
    }

    manifests.sort();
    Ok(manifests)
}

/// Walk `dir` for every `.gzevent` event manifest (exact filename, one per event
/// directory), mirroring RSCTF `RepoBindingDiscoveryService`'s
/// `EnumerateFiles(scanRoot, ".gzevent", AllDirectories)`. Each `.gzevent`
/// defines one game (event); challenges are discovered under its directory.
/// The `.git` dir is skipped; results are sorted for deterministic output.
pub async fn discover_events(dir: &Path) -> AppResult<Vec<PathBuf>> {
    let mut events = Vec::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|e| {
            AppError::internal(format!("git_sync: read_dir {}: {e}", current.display()))
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            AppError::internal(format!(
                "git_sync: read dir entry in {}: {e}",
                current.display()
            ))
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|e| {
                AppError::internal(format!("git_sync: stat {}: {e}", path.display()))
            })?;
            if file_type.is_dir() {
                if path.file_name() == Some(OsStr::new(".git")) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file()
                && path.file_name().and_then(OsStr::to_str) == Some(".gzevent")
            {
                events.push(path);
            }
        }
    }

    events.sort();
    Ok(events)
}

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
    // networkMode is parsed-but-dropped: the rsctf `GameChallenge` has no
    // network_mode column (unlike RSCTF), so there is nowhere to persist it.
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

/// Parse a `challenge.yml` / `challenge.yaml` manifest and INSERT the resulting
/// `GameChallenge` (plus its static `FlagContext` rows) under `game_id`.
///
/// Ports RSCTF `ChallengeImportService.ImportOneAsync` +
/// `ChallengeRepository.CreateChallenge`: deserialize the yaml
/// ([`ChallengeYaml`], mirroring `ChallengeYamlSerializer`'s model), map it onto
/// the flattened challenge/base fields, then persist. Returns the created
/// challenge id.
///
/// This is the create half only (the poller's idempotent upsert-on-re-scan keyed
/// by `(binding id, manifest path)` layers on top of this). [`ImportPolicy`]
/// establishes the row's review state at insert time and decides whether build /
/// checker preparation may run.
///
/// Errors: a missing/empty `name`, an unknown `type`, `ignore: true`, or a
/// nonexistent `game_id` all map to [`AppError::bad_request`]; a yaml parse
/// failure or a DB error maps through [`AppError`].
pub async fn import_manifest(
    st: &SharedState,
    game_id: i32,
    manifest: &Path,
    policy: ImportPolicy,
) -> AppResult<i32> {
    // Fail early with a friendly message rather than surfacing an FK violation
    // from the INSERT below.
    let game = game::Entity::find_by_id(game_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found(format!("game {game_id} not found")))?;

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
            "'{name}' has ignore: true — not synced"
        )));
    }

    let raw_type = model.challenge_type.as_deref().unwrap_or("");
    let challenge_type = parse_enum::<ChallengeType>(raw_type)
        .ok_or_else(|| AppError::bad_request(format!("unknown challenge type '{raw_type}'")))?;
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

    // A&D-engine knobs. Egress is deny-by-default; self-reset retains the
    // upstream default. A sparse manifest must opt into outbound access.
    let ad = model.ad.as_ref();
    let uses_ad = challenge_type.uses_ad_engine();
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
    let mut archive_blob_path: Option<String> = if policy == ImportPolicy::PendingReview {
        let package = zip_context_dir(package_dir).await?;
        let (blob, _) = crate::services::blob_refs::store_and_acquire(
            st.pg(),
            st.storage.as_ref(),
            "challenge-source.zip",
            &package,
        )
        .await?;
        Some(blob.hash)
    } else {
        None
    };
    let mut queue_challenge_build = false;
    let mut build_context_subdir = None;
    // Build locally when the operator named no registry image, or used a gzcli-style
    // "{{.slug}}:latest" template placeholder ("build whatever Dockerfile is here").
    // A concrete registry ref (nginx:alpine, ghcr.io/foo:tag) is resolved by the
    // pull seam below rather than built from local source.
    let wants_local_build = is_container
        && declared_container_image
            .as_deref()
            .map(|s| s.contains("{{"))
            .unwrap_or(true);
    if wants_local_build {
        if let Some(ctx_dir) = find_dockerfile_context(package_dir) {
            if policy.may_execute() {
                // Preserve the complete reviewed package, not only the Docker
                // subtree. `build_context_subdir` selects the exact bytes sent
                // to Docker while audit/checker provenance remains available.
                let zip = zip_context_dir(package_dir).await?;
                let (blob, _) = crate::services::blob_refs::store_and_acquire(
                    st.pg(),
                    st.storage.as_ref(),
                    "challenge-source.zip",
                    &zip,
                )
                .await?;
                archive_blob_path = Some(blob.hash);
            }
            if ctx_dir == package_dir.join("src") {
                // Pending review retains the complete immutable package for
                // checker preparation and audit. The builder deterministically
                // selects this subtree without replacing the source blob.
                build_context_subdir = Some("src".to_string());
            } else {
                build_context_subdir = Some(".".to_string());
            }
            container_image = Some(image_tag(game_id, &name));
            build_status = ChallengeBuildStatus::Queued;
            queue_challenge_build = true;
        }
    }
    // Concrete registry tags are mutable too. Pull them through the same build
    // seam so Docker resolves and persists the exact repository digest before a
    // reviewed runtime may execute the challenge.
    if is_container
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
        cleanup_unpublished_archive(st, archive_blob_path.as_deref()).await;
        return Err(AppError::bad_request(
            "ad.checkerImage requests a local checker but checker/run.py is missing",
        ));
    }
    let checker_prep = if policy.may_execute() {
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
                cleanup_unpublished_archive(st, archive_blob_path.as_deref()).await;
                return Err(error);
            }
        };
        if let Err(error) = prepare_checker_venv(dest, src_dir).await {
            cleanup_unpublished_archive(st, archive_blob_path.as_deref()).await;
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
    let am = game_challenge::ActiveModel {
        game_id: Set(game_id),
        title: Set(name),
        content: Set(content),
        category: Set(category),
        challenge_type: Set(challenge_type),
        hints: Set(hints),
        is_enabled: Set(false),
        submission_limit: Set(submission_limit),
        accepted_count: Set(0),
        submission_count: Set(0),
        container_image: Set(container_image),
        memory_limit: Set(memory_limit),
        storage_limit: Set(storage_limit),
        cpu_count: Set(cpu_count),
        expose_port: Set(expose_port),
        flag_template: Set(flag_template),
        // Establish review state at INSERT time. A user submission must never be
        // transiently Active while its untrusted side effects are still running.
        review_status: Set(policy.review_status()),
        reviewed_at_utc: Set(policy.reviewed_at(now)),
        submitted_at_utc: Set(Some(now)),
        build_status: Set(build_status),
        // Record the manifest's on-disk path so the edit-time push-back
        // (EditController.TryPushBackAsync) can find the yaml to regenerate even
        // after a title/category rename. Absolute (rooted at the per-binding
        // checkout dir) — the push-back re-derives the git-relative path via
        // strip_prefix(checkout). Previously left NULL.
        source_yaml_path: Set(durable_repo_manifest_path(
            &st.config.storage_root,
            game.repo_binding_id,
            manifest,
        )),
        original_archive_blob_path: Set(archive_blob_path.clone()),
        build_context_subdir: Set(build_context_subdir),
        enable_traffic_capture: Set(enable_traffic_capture),
        enable_shared_container: Set(enable_shared_container),
        disable_blood_bonus: Set(model.disable_blood_bonus.unwrap_or(false)),
        original_score: Set(1000),
        min_score_rate: Set(min_score_rate),
        difficulty: Set(difficulty),
        score_curve: Set(ScoreCurve::Standard),
        ad_checker_image: Set(ad_checker_image),
        ad_allow_egress: Set(ad_allow_egress),
        ad_allow_self_reset: Set(ad_allow_self_reset),
        ad_ssh_requires_flag: Set(ad_ssh_requires_flag),
        ad_self_hosted: Set(ad_self_hosted),
        ..Default::default()
    };
    let created = match am.insert(&st.db).await {
        Ok(created) => created,
        Err(error) => {
            // The archive reference was acquired before INSERT so the build
            // row could point at an already-published object. If publication
            // of the owning challenge fails, release that otherwise-orphaned
            // reference under the same distributed hash fence as every other
            // blob producer.
            cleanup_unpublished_archive(st, archive_blob_path.as_deref()).await;
            // This unique revision has not been published through the database,
            // so it is safe to discard. Once INSERT succeeds revisions are never
            // mutated or removed by the importer.
            if checker_prepared {
                if let Some((dest, _)) = checker_prep.as_ref() {
                    if let Err(cleanup_error) = tokio::fs::remove_dir_all(dest).await {
                        tracing::warn!(
                            error = %cleanup_error,
                            path = %dest,
                            "git_sync: unpublished checker cleanup failed"
                        );
                    }
                }
            }
            if let Some(guard) = checker_artifact_guard.take() {
                if let Err(release_error) = guard.release().await {
                    tracing::warn!(%release_error, "checker publication guard release failed");
                }
            }
            return Err(error.into());
        }
    };
    if let Some(guard) = checker_artifact_guard.take() {
        if let Err(error) = guard.release().await {
            tracing::warn!(%error, "checker publication guard release failed");
        }
    }

    // Attach the challenge's provided artifact — the RSCTF `provide:` path, or the
    // TCP1P `dist/` convention when it's absent. Best-effort (logs on failure).
    let _ = sync_attachment(st, created.id, package_dir, model.provide.as_deref()).await;

    // Static flags → FlagContext rows. A&D/KotH plant per-team flags at runtime
    // and carry none here; dedup so a manifest listing the same flag twice
    // doesn't double-insert.
    if !uses_ad {
        if let Some(flags) = model.flags {
            let mut seen = std::collections::HashSet::new();
            for flag in flags {
                let flag = flag.trim().to_string();
                if flag.is_empty() || !seen.insert(flag.clone()) {
                    continue;
                }
                let fam = flag_context::ActiveModel {
                    flag: Set(flag),
                    is_occupied: Set(false),
                    challenge_id: Set(Some(created.id)),
                    ..Default::default()
                };
                fam.insert(&st.db).await?;
            }
        }
    }

    // Fire the deferred image builds now the row (and its Model) exists — the build
    // seam needs the persisted challenge. Mirrors RSCTF enqueuing after SaveChanges.
    if policy.may_execute() && queue_challenge_build {
        let (_outcome, _record) =
            crate::controllers::edit::run_challenge_build(st, &created, "Import", 1).await;
        // `run_challenge_build` publishes status/log while holding its
        // cross-replica per-challenge lock.
    }
    Ok(created.id)
}

/// Push-back: regenerate a challenge's `challenge.yml` from its DB row and
/// git-push it upstream (RSCTF `ChallengeYamlSerializer.Serialize` +
/// `GitRepoSyncService.CommitAndPushCoreAsync`, driven by
/// `EditController.TryPushBackAsync`). See [`push_back`].
mod attach;
pub use attach::repair_missing_attachments;
use attach::sync_attachment;
mod push_back;
pub use push_back::{push_file, serialize_challenge};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_imports_are_inert_from_the_initial_insert() {
        let now = Utc::now();
        let policy = ImportPolicy::PendingReview;
        assert_eq!(policy.review_status(), ChallengeReviewStatus::Pending);
        assert_eq!(policy.reviewed_at(now), None);
        assert!(!policy.may_execute());
    }

    #[test]
    fn trusted_imports_preserve_inline_preparation() {
        let now = Utc::now();
        let policy = ImportPolicy::Trusted;
        assert_eq!(policy.review_status(), ChallengeReviewStatus::Active);
        assert_eq!(policy.reviewed_at(now), Some(now));
        assert!(policy.may_execute());
    }

    #[test]
    fn source_paths_are_persisted_only_inside_the_binding_checkout() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-durable-source-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let checkout = root.join("repos/7/challenge");
        std::fs::create_dir_all(&checkout).unwrap();
        let manifest = checkout.join("challenge.yml");
        std::fs::write(&manifest, b"name: example\n").unwrap();
        let outside = root.join("temporary.yml");
        std::fs::write(&outside, b"name: temporary\n").unwrap();

        assert_eq!(
            durable_repo_manifest_path(root.to_str().unwrap(), Some(7), &manifest)
                .map(PathBuf::from),
            Some(std::fs::canonicalize(&manifest).unwrap())
        );
        assert_eq!(
            durable_repo_manifest_path(root.to_str().unwrap(), Some(7), &outside),
            None
        );
        assert_eq!(
            durable_repo_manifest_path(root.to_str().unwrap(), None, &manifest),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn checkout_lock_serializes_one_checkout_only() {
        let root = std::env::temp_dir().join(format!("rsctf-lock-{}", uuid::Uuid::new_v4()));
        let same = root.join("repo");
        let different = root.join("other");
        let first = lock_checkout(&same).await;

        let independent =
            tokio::time::timeout(Duration::from_millis(250), lock_checkout(&different))
                .await
                .expect("different checkouts must not block each other");
        drop(independent);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), lock_checkout(&same))
                .await
                .is_err(),
            "the same checkout must remain locked"
        );

        drop(first);
        tokio::time::timeout(Duration::from_millis(250), lock_checkout(&same))
            .await
            .expect("the checkout lock must be released with its guard");
    }

    #[test]
    fn repository_url_policy_rejects_local_and_option_like_transports() {
        assert!(validate_github_repo_url("https://github.com/rsctf/example.git").is_ok());
        assert!(validate_github_repo_url("http://github.com/rsctf/example.git").is_err());
        assert!(validate_github_repo_url("https://github.com.evil.test/a/b").is_err());
        for invalid in [
            "--upload-pack=/tmp/pwn",
            "/tmp/repo",
            "file:///tmp/repo",
            "ext::sh -c id",
            "ssh://example.com/repo",
            "https://user:pass@example.com/repo",
            "http://127.0.0.1/repo",
            "http://localhost/repo",
        ] {
            assert!(
                validate_binding_repo_url(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(validate_binding_repo_url("https://git.example.com/team/repo.git").is_ok());
    }

    #[test]
    fn git_refs_reject_option_and_ref_syntax_injection() {
        for invalid in [
            "--upload-pack=evil",
            "main..evil",
            "bad ref",
            "x@{y",
            "a\\b",
        ] {
            assert!(
                validate_git_ref(Some(invalid)).is_err(),
                "accepted {invalid}"
            );
        }
        assert_eq!(
            validate_git_ref(Some(" refs/tags/v1 ")).unwrap().as_deref(),
            Some("refs/tags/v1")
        );
        assert_eq!(validate_git_ref(None).unwrap(), None);
    }

    #[test]
    fn credentials_are_encoded_and_removable() {
        let authenticated =
            GitCredentials::new("token:@/value").apply("https://github.com/rsctf/example.git");
        validate_sync_repo_url(&authenticated).unwrap();
        assert_eq!(
            url_without_credentials(&authenticated).unwrap(),
            "https://github.com/rsctf/example.git"
        );
    }

    #[tokio::test]
    async fn checkout_tree_limits_depth_before_packaging() {
        let root = std::env::temp_dir().join(format!("rsctf-tree-{}", uuid::Uuid::new_v4()));
        let mut current = root.clone();
        for _ in 0..=MAX_REPO_DEPTH {
            current.push("d");
        }
        tokio::fs::create_dir_all(&current).await.unwrap();
        tokio::fs::write(current.join("file"), b"x").await.unwrap();
        assert!(validate_checkout_tree(&root).await.is_err());
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
