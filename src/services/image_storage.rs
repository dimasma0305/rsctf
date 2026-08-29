//! Bounded lifecycle for locally-built, reproducible challenge images.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::container::ListContainersOptions;
use bollard::errors::Error as DockerError;
use bollard::image::{PruneImagesOptions, RemoveImageOptions};
use bollard::models::BuildPruneResponse;
use bollard::Docker;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::StreamExt;
use serde::Serialize;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::services::container_policy::ContainerPolicy;
use crate::utils::enums::{ChallengeBuildStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};

const DOCKER_CALL_BUDGET: Duration = Duration::from_secs(15);
const CLEANUP_CLAIM_SECONDS: i64 = 180;
const CLEANUP_BATCH_SIZE: i64 = 32;
const CLEANUP_CONCURRENCY: usize = 4;
const DOCKER_API_VERSION: &str = "v1.45";
const GIB: u64 = 1024 * 1024 * 1024;

mod scheduler;
pub use scheduler::scheduled_cleanup;

const OWNERSHIPS_AFTER_SQL: &str = r#"SELECT canonical_ref, image_id, updated_at_utc,
       last_used_at_utc, cleanup_claim_id, cleanup_claim_expires_at_utc
  FROM "BuildImageOwnerships"
 WHERE installation_scope = $1
   AND ($2::text IS NULL OR canonical_ref > $2)
 ORDER BY canonical_ref
 LIMIT $3"#;

const OWNERSHIP_SQL: &str = r#"SELECT canonical_ref, image_id, updated_at_utc,
       last_used_at_utc, cleanup_claim_id, cleanup_claim_expires_at_utc
  FROM "BuildImageOwnerships"
 WHERE installation_scope = $1 AND canonical_ref = $2"#;

const CHALLENGE_REFERENCES_SQL: &str = r#"SELECT id, title, "Type" AS challenge_type,
       container_image, ad_checker_image, original_archive_blob_path,
       build_context_subdir, build_status, build_image_digest, workload_spec,
       variant_generator_image, variant_generator_digest,
       variant_generator_build_context_subdir, variant_generator_build_status
  FROM "GameChallenges"
 WHERE (container_image IS NOT NULL AND BTRIM(container_image) <> '')
    OR (ad_checker_image IS NOT NULL AND BTRIM(ad_checker_image) <> '')
    OR (variant_generator_build_context_subdir = 'generator'
        AND variant_generator_build_status = 1
        AND variant_generator_image IS NOT NULL
        AND variant_generator_image = variant_generator_digest)"#;

const CANDIDATE_REFERENCES_SQL: &str = r#"SELECT id, title, "Type" AS challenge_type,
       container_image, ad_checker_image, original_archive_blob_path,
       build_context_subdir, build_status, build_image_digest, workload_spec,
       variant_generator_image, variant_generator_digest,
       variant_generator_build_context_subdir, variant_generator_build_status
  FROM "GameChallenges"
 WHERE BTRIM(container_image) = ANY($1)
    OR BTRIM(ad_checker_image) = ANY($1)
    OR variant_generator_image = $2"#;

#[derive(Clone, Debug, sqlx::FromRow)]
pub(crate) struct ImageOwnership {
    pub canonical_ref: String,
    pub image_id: String,
    pub updated_at_utc: DateTime<Utc>,
    pub last_used_at_utc: Option<DateTime<Utc>>,
    pub cleanup_claim_id: Option<uuid::Uuid>,
    pub cleanup_claim_expires_at_utc: Option<DateTime<Utc>>,
}

impl ImageOwnership {
    pub fn retention_anchor(&self) -> DateTime<Utc> {
        self.last_used_at_utc.unwrap_or(self.updated_at_utc)
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct ChallengeReference {
    id: i32,
    title: String,
    challenge_type: i16,
    container_image: Option<String>,
    ad_checker_image: Option<String>,
    original_archive_blob_path: Option<String>,
    build_context_subdir: Option<String>,
    build_status: i16,
    build_image_digest: Option<String>,
    workload_spec: Option<serde_json::Value>,
    variant_generator_image: Option<String>,
    variant_generator_digest: Option<String>,
    variant_generator_build_context_subdir: Option<String>,
    variant_generator_build_status: i16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeImageReservation {
    Unmanaged,
    Ready,
    Missing,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageStorageStatus {
    pub filesystem_total_bytes: u64,
    pub filesystem_available_bytes: u64,
    pub build_cache_bytes: u64,
    pub reclaimable_build_cache_bytes: u64,
    pub minimum_free_bytes: u64,
    pub low_storage: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCleanupReport {
    pub images_removed: i32,
    pub image_bytes_evicted: u64,
    pub cache_bytes_reclaimed: u64,
    pub dangling_bytes_reclaimed: u64,
    pub available_bytes_before: u64,
    pub available_bytes_after: u64,
    pub minimum_free_bytes: u64,
    pub pressure_mode: bool,
    /// Ownership rows deferred to a later bounded pass. Candidates inspected
    /// during this pass but retained are not counted as backlog.
    pub candidate_backlog: u64,
    pub messages: Vec<String>,
    #[serde(skip)]
    pub(crate) next_candidate_cursor: Option<String>,
}

pub(crate) fn lazy_build_eligible(
    policy: &ContainerPolicy,
    challenge: &game_challenge::Model,
) -> bool {
    policy.build_images_on_demand
        && matches!(
            challenge.challenge_type,
            ChallengeType::StaticContainer | ChallengeType::DynamicContainer
        )
        && challenge.workload_spec.is_none()
        && challenge
            .original_archive_blob_path
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && challenge
            .build_context_subdir
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && challenge
            .container_image
            .as_deref()
            .and_then(crate::controllers::edit::canonical_managed_image_tag)
            .is_some()
}

pub(crate) async fn connect_local_docker() -> Result<Docker, String> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| format!("Docker connection failed: {error}"))?;
    match tokio::time::timeout(Duration::from_secs(2), docker.ping()).await {
        Ok(Ok(_)) => Ok(docker),
        Ok(Err(error)) => Err(format!("Docker daemon is unavailable: {error}")),
        Err(_) => Err("Docker daemon ping timed out".to_string()),
    }
}

fn filesystem_space(path: &Path) -> AppResult<(u64, u64)> {
    let stats = nix::sys::statvfs::statvfs(path)
        .map_err(|error| AppError::internal(format!("filesystem usage probe failed: {error}")))?;
    let block_size = stats.fragment_size();
    Ok((
        stats.blocks().saturating_mul(block_size),
        stats.blocks_available().saturating_mul(block_size),
    ))
}

fn minimum_free_bytes(policy: &ContainerPolicy) -> u64 {
    u64::try_from(policy.minimum_free_storage_gib)
        .unwrap_or_default()
        .saturating_mul(GIB)
}

async fn storage_status_with(
    docker: &Docker,
    policy: &ContainerPolicy,
) -> AppResult<ImageStorageStatus> {
    let (filesystem_total_bytes, filesystem_available_bytes) = filesystem_space(Path::new("/"))?;
    let usage = tokio::time::timeout(DOCKER_CALL_BUDGET, docker.df())
        .await
        .map_err(|_| AppError::unavailable("Docker disk usage timed out"))?
        .map_err(|error| AppError::unavailable(format!("Docker disk usage failed: {error}")))?;
    let caches = usage.build_cache.unwrap_or_default();
    let (build_cache_bytes, reclaimable_build_cache_bytes) = build_cache_space(
        caches
            .iter()
            .map(|cache| (cache.size, cache.shared, cache.in_use)),
    );
    let minimum_free_bytes = minimum_free_bytes(policy);
    Ok(ImageStorageStatus {
        filesystem_total_bytes,
        filesystem_available_bytes,
        build_cache_bytes,
        reclaimable_build_cache_bytes,
        minimum_free_bytes,
        low_storage: minimum_free_bytes > 0 && filesystem_available_bytes < minimum_free_bytes,
    })
}

/// Docker reports cache records shared with image layers as logical cache
/// bytes, but pruning those records cannot release the shared layer. Match
/// `docker system df` by reporting only private cache bytes.
fn build_cache_space(
    caches: impl IntoIterator<Item = (Option<i64>, Option<bool>, Option<bool>)>,
) -> (u64, u64) {
    caches
        .into_iter()
        .filter(|(_, shared, _)| !shared.unwrap_or(false))
        .filter_map(|(size, _, in_use)| {
            size.and_then(|size| u64::try_from(size).ok())
                .map(|size| (size, !in_use.unwrap_or(false)))
        })
        .fold(
            (0_u64, 0_u64),
            |(total, reclaimable), (size, can_reclaim)| {
                (
                    total.saturating_add(size),
                    reclaimable.saturating_add(if can_reclaim { size } else { 0 }),
                )
            },
        )
}

pub async fn storage_status(st: &SharedState) -> AppResult<ImageStorageStatus> {
    let policy = ContainerPolicy::load(st.pg()).await?;
    let docker = connect_local_docker()
        .await
        .map_err(AppError::unavailable)?;
    storage_status_with(&docker, &policy).await
}

pub(crate) async fn reserve_runtime_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
    immutable_image: &str,
) -> AppResult<RuntimeImageReservation> {
    if !crate::services::challenge_images::is_local_image_id(immutable_image) {
        return Ok(RuntimeImageReservation::Unmanaged);
    }
    let Some(canonical_ref) = challenge
        .container_image
        .as_deref()
        .and_then(crate::controllers::edit::canonical_managed_image_tag)
    else {
        return Ok(RuntimeImageReservation::Unmanaged);
    };
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let scope = crate::services::container::docker_installation_scope();
    let owned = sqlx::query_as::<_, ImageOwnership>(OWNERSHIP_SQL)
        .bind(&scope)
        .bind(&canonical_ref)
        .fetch_optional(lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let exact_owner = owned
        .as_ref()
        .is_some_and(|ownership| ownership.image_id.eq_ignore_ascii_case(immutable_image));
    if !exact_owner {
        lock.release().await?;
        let present = st.containers.image_exists(immutable_image).await;
        return Ok(if present {
            RuntimeImageReservation::Unmanaged
        } else {
            RuntimeImageReservation::Missing
        });
    }
    if owned.as_ref().is_some_and(|ownership| {
        ownership.cleanup_claim_id.is_some()
            && ownership
                .cleanup_claim_expires_at_utc
                .is_some_and(|expires| expires > Utc::now())
    }) {
        // A scheduled removal claimed this exact immutable identity before the
        // start reached the shared build fence. Make the caller retry rather
        // than launching while Docker deletion is in flight.
        lock.release().await?;
        return Ok(RuntimeImageReservation::Missing);
    }
    let reserved = sqlx::query(
        r#"UPDATE "BuildImageOwnerships"
              SET last_used_at_utc = clock_timestamp()
                , cleanup_claim_id = NULL
                , cleanup_claim_expires_at_utc = NULL
            WHERE installation_scope = $1
              AND canonical_ref = $2
              AND image_id = $3
              AND (cleanup_claim_expires_at_utc IS NULL
                   OR cleanup_claim_expires_at_utc <= clock_timestamp())"#,
    )
    .bind(&scope)
    .bind(&canonical_ref)
    .bind(immutable_image)
    .execute(lock.connection_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    lock.release().await?;
    if reserved != 1 || !st.containers.image_exists(immutable_image).await {
        return Ok(RuntimeImageReservation::Missing);
    }
    Ok(RuntimeImageReservation::Ready)
}

fn reference_matches(reference: Option<&str>, canonical_ref: &str) -> bool {
    reference.is_some_and(|reference| {
        crate::controllers::edit::canonical_image_reference(Some(reference)) == canonical_ref
    })
}

fn reference_is_rebuildable(reference: &ChallengeReference, ownership: &ImageOwnership) -> bool {
    if managed_generator_matches(reference, ownership) {
        return false;
    }
    reference_matches(
        reference.container_image.as_deref(),
        &ownership.canonical_ref,
    ) && !reference_matches(
        reference.ad_checker_image.as_deref(),
        &ownership.canonical_ref,
    ) && matches!(reference.challenge_type, 1 | 3)
        && reference.workload_spec.is_none()
        && reference
            .original_archive_blob_path
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && reference
            .build_context_subdir
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && reference.build_status == ChallengeBuildStatus::Success as i16
        && reference
            .build_image_digest
            .as_deref()
            .is_some_and(|digest| digest.eq_ignore_ascii_case(&ownership.image_id))
}

fn managed_generator_matches(reference: &ChallengeReference, ownership: &ImageOwnership) -> bool {
    reference.variant_generator_build_context_subdir.as_deref()
        == Some(crate::services::git_sync::GENERATOR_CONTEXT_SUBDIR)
        && reference.variant_generator_build_status == ChallengeBuildStatus::Success as i16
        && reference.variant_generator_image == reference.variant_generator_digest
        && reference
            .variant_generator_image
            .as_deref()
            .is_some_and(|image| image.eq_ignore_ascii_case(&ownership.image_id))
}

fn references_for<'a>(
    references: &'a [ChallengeReference],
    ownership: &ImageOwnership,
) -> Vec<&'a ChallengeReference> {
    references
        .iter()
        .filter(|reference| {
            reference_matches(
                reference.container_image.as_deref(),
                &ownership.canonical_ref,
            ) || reference_matches(
                reference.ad_checker_image.as_deref(),
                &ownership.canonical_ref,
            ) || managed_generator_matches(reference, ownership)
        })
        .collect()
}

fn canonical_reference_aliases(canonical_ref: &str) -> Vec<String> {
    let mut aliases = vec![canonical_ref.to_string()];
    if let Some(short) = canonical_ref.strip_prefix("docker.io/") {
        aliases.push(short.to_string());
        aliases.push(format!("index.docker.io/{short}"));
    }
    if canonical_ref.ends_with(":latest") {
        let with_latest = aliases.clone();
        aliases.extend(
            with_latest
                .into_iter()
                .filter_map(|alias| alias.strip_suffix(":latest").map(str::to_string)),
        );
    }
    aliases.sort_unstable();
    aliases.dedup();
    aliases
}

async fn load_candidate_references(
    connection: &mut sqlx::PgConnection,
    candidate: &ImageOwnership,
) -> AppResult<Vec<ChallengeReference>> {
    sqlx::query_as::<_, ChallengeReference>(CANDIDATE_REFERENCES_SQL)
        .bind(canonical_reference_aliases(&candidate.canonical_ref))
        .bind(&candidate.image_id)
        .fetch_all(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

fn candidate_is_evictable(
    candidate: &ImageOwnership,
    matching: &[&ChallengeReference],
    live_image_ids: &HashSet<String>,
    cutoff: DateTime<Utc>,
    pressure: bool,
) -> bool {
    let orphan = matching.is_empty();
    let expired = candidate.retention_anchor() <= cutoff;
    (expired || (pressure && orphan))
        && (orphan
            || matching
                .iter()
                .all(|reference| reference_is_rebuildable(reference, candidate)))
        && !live_image_ids.contains(&candidate.image_id.to_ascii_lowercase())
}

async fn claim_cleanup_candidate(
    st: &SharedState,
    candidate: &ImageOwnership,
    live_image_ids: &HashSet<String>,
    cutoff: DateTime<Utc>,
    pressure: bool,
) -> AppResult<Option<uuid::Uuid>> {
    let scope = crate::services::container::docker_installation_scope();
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&candidate.canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let current = sqlx::query_as::<_, ImageOwnership>(OWNERSHIP_SQL)
        .bind(&scope)
        .bind(&candidate.canonical_ref)
        .fetch_optional(lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(current) = current.filter(|current| {
        current.image_id.eq_ignore_ascii_case(&candidate.image_id)
            && current
                .cleanup_claim_expires_at_utc
                .is_none_or(|expires| expires <= Utc::now())
    }) else {
        lock.release().await?;
        return Ok(None);
    };
    let references = load_candidate_references(lock.connection_mut(), &current).await?;
    let matching = references_for(&references, &current);
    if !candidate_is_evictable(&current, &matching, live_image_ids, cutoff, pressure) {
        lock.release().await?;
        return Ok(None);
    }

    let claim_id = uuid::Uuid::new_v4();
    let claimed = sqlx::query(
        r#"UPDATE "BuildImageOwnerships"
              SET cleanup_claim_id = $4,
                  cleanup_claim_expires_at_utc = clock_timestamp()
                    + ($5::bigint * INTERVAL '1 second')
            WHERE installation_scope = $1
              AND canonical_ref = $2
              AND image_id = $3
              AND (cleanup_claim_expires_at_utc IS NULL
                   OR cleanup_claim_expires_at_utc <= clock_timestamp())"#,
    )
    .bind(&scope)
    .bind(&candidate.canonical_ref)
    .bind(&candidate.image_id)
    .bind(claim_id)
    .bind(CLEANUP_CLAIM_SECONDS)
    .execute(lock.connection_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    lock.release().await?;
    Ok((claimed == 1).then_some(claim_id))
}

async fn release_cleanup_claim(
    st: &SharedState,
    candidate: &ImageOwnership,
    claim_id: uuid::Uuid,
) -> AppResult<()> {
    let scope = crate::services::container::docker_installation_scope();
    sqlx::query(
        r#"UPDATE "BuildImageOwnerships"
              SET cleanup_claim_id = NULL,
                  cleanup_claim_expires_at_utc = NULL
            WHERE installation_scope = $1
              AND canonical_ref = $2
              AND image_id = $3
              AND cleanup_claim_id = $4"#,
    )
    .bind(scope)
    .bind(&candidate.canonical_ref)
    .bind(&candidate.image_id)
    .bind(claim_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn commit_removed_ownership(
    st: &SharedState,
    candidate: &ImageOwnership,
    claim_id: uuid::Uuid,
    cutoff: DateTime<Utc>,
    pressure: bool,
) -> AppResult<bool> {
    let scope = crate::services::container::docker_installation_scope();
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&candidate.canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let current = sqlx::query_as::<_, ImageOwnership>(OWNERSHIP_SQL)
        .bind(&scope)
        .bind(&candidate.canonical_ref)
        .fetch_optional(lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(current) = current.filter(|current| {
        current.image_id.eq_ignore_ascii_case(&candidate.image_id)
            && current.cleanup_claim_id == Some(claim_id)
    }) else {
        lock.release().await?;
        return Ok(false);
    };
    let references = load_candidate_references(lock.connection_mut(), &current).await?;
    let matching = references_for(&references, &current);
    let no_live_images = HashSet::new();
    if !candidate_is_evictable(&current, &matching, &no_live_images, cutoff, pressure) {
        sqlx::query(
            r#"UPDATE "BuildImageOwnerships"
                  SET cleanup_claim_id = NULL,
                      cleanup_claim_expires_at_utc = NULL
                WHERE installation_scope = $1
                  AND canonical_ref = $2
                  AND image_id = $3
                  AND cleanup_claim_id = $4"#,
        )
        .bind(&scope)
        .bind(&candidate.canonical_ref)
        .bind(&candidate.image_id)
        .bind(claim_id)
        .execute(lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        lock.release().await?;
        return Ok(false);
    }
    let deleted = sqlx::query(
        r#"DELETE FROM "BuildImageOwnerships"
            WHERE installation_scope = $1
              AND canonical_ref = $2
              AND image_id = $3
              AND cleanup_claim_id = $4"#,
    )
    .bind(&scope)
    .bind(&candidate.canonical_ref)
    .bind(&candidate.image_id)
    .bind(claim_id)
    .execute(lock.connection_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    lock.release().await?;
    Ok(deleted == 1)
}

fn docker_socket_path_from(host: Option<&str>) -> Option<PathBuf> {
    match host {
        Some(host) => host.strip_prefix("unix://").map(PathBuf::from),
        None => Some(PathBuf::from("/var/run/docker.sock")),
    }
}

fn docker_socket_path() -> Option<PathBuf> {
    let host = std::env::var("DOCKER_HOST").ok();
    docker_socket_path_from(host.as_deref())
}

async fn prune_build_cache(retention_hours: i32, pressure: bool) -> AppResult<u64> {
    let socket = docker_socket_path().ok_or_else(|| {
        AppError::unavailable("Docker build-cache cleanup requires a local Unix socket")
    })?;
    let filters = if pressure {
        HashMap::<String, Vec<String>>::new()
    } else {
        HashMap::from([("until".to_string(), vec![format!("{retention_hours}h")])])
    };
    let filters =
        serde_json::to_string(&filters).map_err(|error| AppError::internal(error.to_string()))?;
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .timeout(DOCKER_CALL_BUDGET)
        .build()
        .map_err(|error| AppError::internal(error.to_string()))?;
    let response = client
        .post(format!("http://localhost/{DOCKER_API_VERSION}/build/prune"))
        .query(&[("filters", filters)])
        .send()
        .await
        .map_err(|error| {
            AppError::unavailable(format!("Docker build-cache prune failed: {error}"))
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AppError::unavailable(format!(
            "Docker build-cache prune returned {status}: {body}"
        )));
    }
    let result = response
        .json::<BuildPruneResponse>()
        .await
        .map_err(|error| AppError::internal(format!("invalid Docker prune response: {error}")))?;
    Ok(result
        .space_reclaimed
        .and_then(|bytes| u64::try_from(bytes).ok())
        .unwrap_or_default())
}

fn docker_not_found(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

async fn evict_one(
    st: &SharedState,
    docker: &Docker,
    candidate: &ImageOwnership,
    references: &[ChallengeReference],
    live_image_ids: &HashSet<String>,
    cutoff: DateTime<Utc>,
    pressure: bool,
) -> AppResult<Option<(u64, String)>> {
    let matching = references_for(references, candidate);
    if !candidate_is_evictable(candidate, &matching, live_image_ids, cutoff, pressure) {
        return Ok(None);
    }
    let Some(claim_id) =
        claim_cleanup_candidate(st, candidate, live_image_ids, cutoff, pressure).await?
    else {
        return Ok(None);
    };
    let scope = crate::services::container::docker_installation_scope();

    let inspected = match tokio::time::timeout(
        DOCKER_CALL_BUDGET,
        docker.inspect_image(&candidate.image_id),
    )
    .await
    {
        Ok(Ok(inspected)) => inspected,
        Ok(Err(error)) if docker_not_found(&error) => {
            match commit_removed_ownership(st, candidate, claim_id, cutoff, pressure).await {
                Ok(true) => {}
                Ok(false) => release_cleanup_claim(st, candidate, claim_id).await?,
                Err(error) => {
                    let _ = release_cleanup_claim(st, candidate, claim_id).await;
                    return Err(error);
                }
            }
            return Ok(None);
        }
        Ok(Err(error)) => {
            release_cleanup_claim(st, candidate, claim_id).await?;
            return Err(AppError::unavailable(format!(
                "Docker image inspection failed for {}: {error}",
                candidate.canonical_ref
            )));
        }
        Err(_) => {
            release_cleanup_claim(st, candidate, claim_id).await?;
            return Err(AppError::unavailable(format!(
                "Docker image inspection timed out for {}",
                candidate.canonical_ref
            )));
        }
    };
    let Some(current_id) = crate::services::challenge_images::inspected_local_image_id(&inspected)
    else {
        release_cleanup_claim(st, candidate, claim_id).await?;
        return Err(AppError::conflict(
            "Docker returned an invalid image identity",
        ));
    };
    if !current_id.eq_ignore_ascii_case(&candidate.image_id) {
        release_cleanup_claim(st, candidate, claim_id).await?;
        return Err(AppError::conflict(format!(
            "image ownership changed for {}",
            candidate.canonical_ref
        )));
    }
    if let Err(error) = crate::services::challenge_images::validate_image_ownership_labels(
        &inspected,
        &scope,
        &candidate.canonical_ref,
        false,
    ) {
        release_cleanup_claim(st, candidate, claim_id).await?;
        return Err(AppError::conflict(error));
    }
    let size = inspected
        .size
        .and_then(|size| u64::try_from(size).ok())
        .unwrap_or_default();
    let removal = tokio::time::timeout(
        DOCKER_CALL_BUDGET,
        docker.remove_image(
            &candidate.image_id,
            Some(RemoveImageOptions {
                force: false,
                noprune: false,
            }),
            None,
        ),
    )
    .await;
    match removal {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            release_cleanup_claim(st, candidate, claim_id).await?;
            return Err(AppError::conflict(format!(
                "Docker refused to evict {}: {error}",
                candidate.canonical_ref
            )));
        }
        Err(_) => {
            release_cleanup_claim(st, candidate, claim_id).await?;
            return Err(AppError::unavailable(format!(
                "Docker image removal timed out for {}",
                candidate.canonical_ref
            )));
        }
    }
    match tokio::time::timeout(
        DOCKER_CALL_BUDGET,
        docker.inspect_image(&candidate.image_id),
    )
    .await
    {
        Ok(Err(error)) if docker_not_found(&error) => {}
        Ok(Err(error)) => {
            release_cleanup_claim(st, candidate, claim_id).await?;
            return Err(AppError::unavailable(format!(
                "Docker eviction verification failed for {}: {error}",
                candidate.canonical_ref
            )));
        }
        Ok(Ok(_)) => {
            release_cleanup_claim(st, candidate, claim_id).await?;
            return Err(AppError::conflict(format!(
                "Docker still resolves {} after eviction",
                candidate.canonical_ref
            )));
        }
        Err(_) => {
            release_cleanup_claim(st, candidate, claim_id).await?;
            return Err(AppError::unavailable(format!(
                "Docker eviction verification timed out for {}",
                candidate.canonical_ref
            )));
        }
    }
    match commit_removed_ownership(st, candidate, claim_id, cutoff, pressure).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(AppError::conflict(format!(
                "{} changed ownership or gained a protected reference during cleanup",
                candidate.canonical_ref
            )));
        }
        Err(error) => {
            let _ = release_cleanup_claim(st, candidate, claim_id).await;
            return Err(error);
        }
    }
    let titles = matching
        .iter()
        .map(|reference| format!("#{} {}", reference.id, reference.title))
        .collect::<Vec<_>>();
    let detail = if titles.is_empty() {
        "orphaned image".to_string()
    } else {
        format!("rebuildable for {}", titles.join(", "))
    };
    Ok(Some((
        size,
        format!("evicted {} ({detail})", candidate.canonical_ref),
    )))
}

async fn load_cleanup_candidates(
    pool: &sqlx::PgPool,
    scope: &str,
    cursor: Option<&str>,
) -> AppResult<Vec<ImageOwnership>> {
    sqlx::query_as::<_, ImageOwnership>(OWNERSHIPS_AFTER_SQL)
        .bind(scope)
        .bind(cursor)
        .bind(CLEANUP_BATCH_SIZE)
        .fetch_all(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn cleanup_from_cursor(
    st: &SharedState,
    policy: &ContainerPolicy,
    cursor: Option<&str>,
) -> AppResult<ImageCleanupReport> {
    policy.validate()?;
    let docker = connect_local_docker()
        .await
        .map_err(AppError::unavailable)?;
    let before = storage_status_with(&docker, policy).await?;
    let mut report = ImageCleanupReport {
        available_bytes_before: before.filesystem_available_bytes,
        available_bytes_after: before.filesystem_available_bytes,
        minimum_free_bytes: before.minimum_free_bytes,
        pressure_mode: before.low_storage,
        ..Default::default()
    };

    match prune_build_cache(policy.build_cache_retention_hours, before.low_storage).await {
        Ok(bytes) => report.cache_bytes_reclaimed = bytes,
        Err(error) => report
            .messages
            .push(format!("build-cache cleanup skipped: {error}")),
    }
    if before.low_storage {
        let mut filters = HashMap::new();
        filters.insert("dangling".to_string(), vec!["true".to_string()]);
        match tokio::time::timeout(
            DOCKER_CALL_BUDGET,
            docker.prune_images(Some(PruneImagesOptions { filters })),
        )
        .await
        {
            Ok(Ok(result)) => {
                report.dangling_bytes_reclaimed = result
                    .space_reclaimed
                    .and_then(|bytes| u64::try_from(bytes).ok())
                    .unwrap_or_default();
            }
            Ok(Err(error)) => report
                .messages
                .push(format!("dangling-image cleanup skipped: {error}")),
            Err(_) => report
                .messages
                .push("dangling-image cleanup timed out".to_string()),
        }
    }

    let cutoff = Utc::now() - ChronoDuration::hours(i64::from(policy.image_idle_retention_hours));
    let scope = crate::services::container::docker_installation_scope();
    let candidates = load_cleanup_candidates(st.pg(), &scope, cursor).await?;
    let references = sqlx::query_as::<_, ChallengeReference>(CHALLENGE_REFERENCES_SQL)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let containers = tokio::time::timeout(
        DOCKER_CALL_BUDGET,
        docker.list_containers(Some(ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        })),
    )
    .await
    .map_err(|_| AppError::unavailable("Docker container inventory timed out"))?
    .map_err(|error| {
        AppError::unavailable(format!("Docker container inventory failed: {error}"))
    })?;
    let live_image_ids = containers
        .into_iter()
        .filter_map(|container| container.image_id)
        .map(|image_id| image_id.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let candidates = candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.canonical_ref.clone()))
        .collect::<Vec<_>>();
    if let Some(last) = candidates.last() {
        let remaining: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM "BuildImageOwnerships"
                WHERE installation_scope = $1 AND canonical_ref > $2"#,
        )
        .bind(&scope)
        .bind(&last.canonical_ref)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        report.candidate_backlog = u64::try_from(remaining).unwrap_or_default();
        report.next_candidate_cursor = (remaining > 0).then(|| last.canonical_ref.clone());
    }
    let mut outcomes = futures::stream::iter(candidates)
        .map(|candidate| {
            let canonical_ref = candidate.canonical_ref.clone();
            let docker = &docker;
            let references = &references;
            let live_image_ids = &live_image_ids;
            async move {
                let outcome = evict_one(
                    st,
                    docker,
                    &candidate,
                    references,
                    live_image_ids,
                    cutoff,
                    before.low_storage,
                )
                .await;
                (canonical_ref, outcome)
            }
        })
        .buffer_unordered(CLEANUP_CONCURRENCY);
    while let Some((canonical_ref, outcome)) = outcomes.next().await {
        match outcome {
            Ok(Some((bytes, message))) => {
                report.images_removed += 1;
                report.image_bytes_evicted = report.image_bytes_evicted.saturating_add(bytes);
                report.messages.push(message);
            }
            Ok(None) => {}
            Err(error) => report
                .messages
                .push(format!("{canonical_ref} was retained: {error}")),
        }
    }

    let (_, available_after) = filesystem_space(Path::new("/"))?;
    report.available_bytes_after = available_after;
    if report.minimum_free_bytes > 0 && available_after < report.minimum_free_bytes {
        report.messages.push(format!(
            "free storage remains below the configured floor ({} < {} bytes); recent, active, or non-rebuildable images were retained",
            available_after, report.minimum_free_bytes
        ));
    }
    Ok(report)
}

pub async fn cleanup(st: &SharedState, policy: &ContainerPolicy) -> AppResult<ImageCleanupReport> {
    cleanup_from_cursor(st, policy, None).await
}

#[cfg(test)]
#[path = "image_storage/tests.rs"]
mod tests;
