//! Bounded lifecycle for locally-built, reproducible challenge images.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use bollard::container::ListContainersOptions;
use bollard::errors::Error as DockerError;
use bollard::image::{PruneImagesOptions, RemoveImageOptions};
use bollard::models::BuildPruneResponse;
use bollard::Docker;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::services::container_policy::ContainerPolicy;
use crate::utils::enums::{ChallengeBuildStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};

const CLEANUP_INTERVAL_SECONDS: i64 = 15 * 60;
const DOCKER_API_VERSION: &str = "v1.45";
const GIB: u64 = 1024 * 1024 * 1024;
static NEXT_SCHEDULED_CLEANUP: AtomicI64 = AtomicI64::new(0);

const OWNERSHIPS_SQL: &str = r#"SELECT canonical_ref, image_id, updated_at_utc,
       last_used_at_utc
  FROM "BuildImageOwnerships"
 WHERE installation_scope = $1
 ORDER BY COALESCE(last_used_at_utc, updated_at_utc), canonical_ref"#;

const OWNERSHIP_SQL: &str = r#"SELECT canonical_ref, image_id, updated_at_utc,
       last_used_at_utc
  FROM "BuildImageOwnerships"
 WHERE installation_scope = $1 AND canonical_ref = $2"#;

const CHALLENGE_REFERENCES_SQL: &str = r#"SELECT id, title, "Type" AS challenge_type,
       container_image, ad_checker_image, original_archive_blob_path,
       build_context_subdir, build_status, build_image_digest, workload_spec
  FROM "GameChallenges"
 WHERE (container_image IS NOT NULL AND BTRIM(container_image) <> '')
    OR (ad_checker_image IS NOT NULL AND BTRIM(ad_checker_image) <> '')"#;

#[derive(Clone, Debug, sqlx::FromRow)]
pub(crate) struct ImageOwnership {
    pub canonical_ref: String,
    pub image_id: String,
    pub updated_at_utc: DateTime<Utc>,
    pub last_used_at_utc: Option<DateTime<Utc>>,
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
    pub messages: Vec<String>,
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
    let usage = docker
        .df()
        .await
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
    let owned = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
             SELECT 1 FROM "BuildImageOwnerships"
              WHERE installation_scope = $1
                AND canonical_ref = $2
                AND image_id = $3
           )"#,
    )
    .bind(&scope)
    .bind(&canonical_ref)
    .bind(immutable_image)
    .fetch_one(lock.connection_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !owned {
        let present = st.containers.image_exists(immutable_image).await;
        lock.release().await?;
        return Ok(if present {
            RuntimeImageReservation::Unmanaged
        } else {
            RuntimeImageReservation::Missing
        });
    }
    if !st.containers.image_exists(immutable_image).await {
        lock.release().await?;
        return Ok(RuntimeImageReservation::Missing);
    }
    sqlx::query(
        r#"UPDATE "BuildImageOwnerships"
              SET last_used_at_utc = clock_timestamp()
            WHERE installation_scope = $1
              AND canonical_ref = $2
              AND image_id = $3"#,
    )
    .bind(&scope)
    .bind(&canonical_ref)
    .bind(immutable_image)
    .execute(lock.connection_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    lock.release().await?;
    Ok(RuntimeImageReservation::Ready)
}

fn reference_matches(reference: Option<&str>, canonical_ref: &str) -> bool {
    reference.is_some_and(|reference| {
        crate::controllers::edit::canonical_image_reference(Some(reference)) == canonical_ref
    })
}

fn reference_is_rebuildable(reference: &ChallengeReference, ownership: &ImageOwnership) -> bool {
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

fn references_for<'a>(
    references: &'a [ChallengeReference],
    canonical_ref: &str,
) -> Vec<&'a ChallengeReference> {
    references
        .iter()
        .filter(|reference| {
            reference_matches(reference.container_image.as_deref(), canonical_ref)
                || reference_matches(reference.ad_checker_image.as_deref(), canonical_ref)
        })
        .collect()
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
        .timeout(Duration::from_secs(120))
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
    cutoff: DateTime<Utc>,
    pressure: bool,
) -> AppResult<Option<(u64, String)>> {
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&candidate.canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let scope = crate::services::container::docker_installation_scope();
    let Some(current) = sqlx::query_as::<_, ImageOwnership>(OWNERSHIP_SQL)
        .bind(&scope)
        .bind(&candidate.canonical_ref)
        .fetch_optional(lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    else {
        lock.release().await?;
        return Ok(None);
    };
    if current.image_id != candidate.image_id {
        lock.release().await?;
        return Ok(None);
    }

    let references = sqlx::query_as::<_, ChallengeReference>(CHALLENGE_REFERENCES_SQL)
        .fetch_all(lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let matching = references_for(&references, &current.canonical_ref);
    let orphan = matching.is_empty();
    let expired = current.retention_anchor() <= cutoff;
    if !(expired || (pressure && orphan))
        || (!orphan
            && !matching
                .iter()
                .all(|reference| reference_is_rebuildable(reference, &current)))
    {
        lock.release().await?;
        return Ok(None);
    }

    let containers = docker
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        }))
        .await
        .map_err(|error| {
            AppError::unavailable(format!("Docker container inventory failed: {error}"))
        })?;
    if containers.iter().any(|container| {
        container
            .image_id
            .as_deref()
            .is_some_and(|image_id| image_id.eq_ignore_ascii_case(&current.image_id))
    }) {
        lock.release().await?;
        return Ok(None);
    }

    let inspected = match docker.inspect_image(&current.canonical_ref).await {
        Ok(inspected) => inspected,
        Err(error) if docker_not_found(&error) => {
            sqlx::query(
                r#"DELETE FROM "BuildImageOwnerships"
                    WHERE installation_scope = $1 AND canonical_ref = $2 AND image_id = $3"#,
            )
            .bind(&scope)
            .bind(&current.canonical_ref)
            .bind(&current.image_id)
            .execute(lock.connection_mut())
            .await
            .map_err(|db_error| AppError::internal(db_error.to_string()))?;
            lock.release().await?;
            return Ok(None);
        }
        Err(error) => {
            lock.release().await?;
            return Err(AppError::unavailable(format!(
                "Docker image inspection failed for {}: {error}",
                current.canonical_ref
            )));
        }
    };
    let current_id = crate::services::challenge_images::inspected_local_image_id(&inspected)
        .ok_or_else(|| AppError::conflict("Docker returned an invalid image identity"))?;
    if !current_id.eq_ignore_ascii_case(&current.image_id) {
        lock.release().await?;
        return Err(AppError::conflict(format!(
            "image ownership changed for {}",
            current.canonical_ref
        )));
    }
    crate::services::challenge_images::validate_image_ownership_labels(
        &inspected,
        &scope,
        &current.canonical_ref,
        false,
    )
    .map_err(AppError::conflict)?;
    let size = inspected
        .size
        .and_then(|size| u64::try_from(size).ok())
        .unwrap_or_default();
    docker
        .remove_image(
            &current.canonical_ref,
            Some(RemoveImageOptions {
                force: false,
                noprune: false,
            }),
            None,
        )
        .await
        .map_err(|error| {
            AppError::conflict(format!(
                "Docker refused to evict {}: {error}",
                current.canonical_ref
            ))
        })?;
    if docker.inspect_image(&current.canonical_ref).await.is_ok() {
        lock.release().await?;
        return Err(AppError::conflict(format!(
            "Docker still resolves {} after eviction",
            current.canonical_ref
        )));
    }
    sqlx::query(
        r#"DELETE FROM "BuildImageOwnerships"
            WHERE installation_scope = $1 AND canonical_ref = $2 AND image_id = $3"#,
    )
    .bind(&scope)
    .bind(&current.canonical_ref)
    .bind(&current.image_id)
    .execute(lock.connection_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    lock.release().await?;
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
        format!("evicted {} ({detail})", current.canonical_ref),
    )))
}

pub async fn cleanup(st: &SharedState, policy: &ContainerPolicy) -> AppResult<ImageCleanupReport> {
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
        match docker
            .prune_images(Some(PruneImagesOptions { filters }))
            .await
        {
            Ok(result) => {
                report.dangling_bytes_reclaimed = result
                    .space_reclaimed
                    .and_then(|bytes| u64::try_from(bytes).ok())
                    .unwrap_or_default();
            }
            Err(error) => report
                .messages
                .push(format!("dangling-image cleanup skipped: {error}")),
        }
    }

    let cutoff = Utc::now() - ChronoDuration::hours(i64::from(policy.image_idle_retention_hours));
    let scope = crate::services::container::docker_installation_scope();
    let candidates = sqlx::query_as::<_, ImageOwnership>(OWNERSHIPS_SQL)
        .bind(&scope)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut seen = HashSet::new();
    for candidate in candidates {
        if !seen.insert(candidate.canonical_ref.clone()) {
            continue;
        }
        match evict_one(st, &docker, &candidate, cutoff, before.low_storage).await {
            Ok(Some((bytes, message))) => {
                report.images_removed += 1;
                report.image_bytes_evicted = report.image_bytes_evicted.saturating_add(bytes);
                report.messages.push(message);
            }
            Ok(None) => {}
            Err(error) => report
                .messages
                .push(format!("{} was retained: {error}", candidate.canonical_ref)),
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

fn claim_scheduled_cleanup(now: i64) -> bool {
    let next = NEXT_SCHEDULED_CLEANUP.load(Ordering::Relaxed);
    if now < next {
        return false;
    }
    NEXT_SCHEDULED_CLEANUP
        .compare_exchange(
            next,
            now.saturating_add(CLEANUP_INTERVAL_SECONDS),
            Ordering::AcqRel,
            Ordering::Relaxed,
        )
        .is_ok()
}

pub async fn scheduled_cleanup(st: &SharedState) -> AppResult<Option<ImageCleanupReport>> {
    let policy = ContainerPolicy::load(st.pg()).await?;
    if !policy.image_cleanup_enabled
        || st.containers.backend_kind() != crate::services::container::ContainerBackendKind::Docker
        || !claim_scheduled_cleanup(Utc::now().timestamp())
    {
        return Ok(None);
    }
    cleanup(st, &policy).await.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> ChallengeReference {
        ChallengeReference {
            id: 7,
            title: "recoverable".to_string(),
            challenge_type: ChallengeType::DynamicContainer as i16,
            container_image: Some("docker.io/rsctf/game/app:latest".to_string()),
            ad_checker_image: None,
            original_archive_blob_path: Some("build/source.zip".to_string()),
            build_context_subdir: Some("src".to_string()),
            build_status: ChallengeBuildStatus::Success as i16,
            build_image_digest: Some(format!("sha256:{}", "a".repeat(64))),
            workload_spec: None,
        }
    }

    fn ownership() -> ImageOwnership {
        ImageOwnership {
            canonical_ref: "docker.io/rsctf/game/app:latest".to_string(),
            image_id: format!("sha256:{}", "a".repeat(64)),
            updated_at_utc: Utc::now(),
            last_used_at_utc: None,
        }
    }

    #[test]
    fn only_exact_recoverable_jeopardy_sources_are_evictable() {
        let owned = ownership();
        let mut candidate = reference();
        assert!(reference_is_rebuildable(&candidate, &owned));
        candidate.challenge_type = ChallengeType::AttackDefense as i16;
        assert!(!reference_is_rebuildable(&candidate, &owned));
        candidate = reference();
        candidate.original_archive_blob_path = None;
        assert!(!reference_is_rebuildable(&candidate, &owned));
        candidate = reference();
        candidate.ad_checker_image = candidate.container_image.clone();
        assert!(!reference_is_rebuildable(&candidate, &owned));
        candidate = reference();
        candidate.build_image_digest = Some(format!("sha256:{}", "b".repeat(64)));
        assert!(!reference_is_rebuildable(&candidate, &owned));
    }

    #[test]
    fn start_use_replaces_build_time_as_retention_anchor() {
        let built = Utc::now() - ChronoDuration::hours(30);
        let used = Utc::now() - ChronoDuration::hours(2);
        let ownership = ImageOwnership {
            updated_at_utc: built,
            last_used_at_utc: Some(used),
            ..ownership()
        };
        assert_eq!(ownership.retention_anchor(), used);
    }

    #[test]
    fn local_socket_resolution_fails_closed_for_remote_docker() {
        assert!(docker_socket_path_from(Some("tcp://docker.example:2376")).is_none());
        assert_eq!(
            docker_socket_path_from(Some("unix:///run/docker.sock")),
            Some(PathBuf::from("/run/docker.sock"))
        );
    }

    #[test]
    fn storage_status_excludes_cache_bytes_shared_with_images() {
        assert_eq!(
            build_cache_space([
                (Some(100), Some(true), Some(false)),
                (Some(50), Some(false), Some(false)),
                (Some(25), Some(false), Some(true)),
            ]),
            (75, 50),
        );
    }
}
