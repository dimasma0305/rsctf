//! Time-budgeted Docker storage cleanup.
//!
//! PostgreSQL is used only for short candidate claims and identity commits.
//! Every daemon future is dropped at the earlier of its per-operation timeout
//! or the pass-wide absolute deadline.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bollard::container::ListContainersOptions;
use bollard::errors::Error as DockerError;
use bollard::image::{PruneImagesOptions, RemoveImageOptions};
use bollard::models::BuildPruneResponse;
use bollard::Docker;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::StreamExt;
use uuid::Uuid;

use super::{
    connect_local_docker_until, filesystem_space, storage_status_with, ImageCleanupReport,
    ImageOwnership,
};
use crate::app_state::SharedState;
use crate::services::container_policy::ContainerPolicy;
use crate::utils::enums::{ChallengeBuildStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};

const DOCKER_API_VERSION: &str = "v1.45";
const CLEANUP_BATCH_SIZE: i64 = 32;
const CLEANUP_CONCURRENCY: usize = 4;
const CLAIM_LEASE_SECONDS: i32 = 3 * 60;
const ADMIN_CLEANUP_BUDGET: Duration = Duration::from_secs(120);
const DOCKER_OPERATION_BUDGET: Duration = Duration::from_secs(20);
const MAX_REFERENCE_SNAPSHOT: i64 = 50_000;
const MAX_DOCKER_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

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
        AND variant_generator_image = variant_generator_digest)
 ORDER BY id
 LIMIT $1"#;

const CANDIDATE_COUNT_SQL: &str = r#"SELECT COUNT(*)
  FROM "BuildImageOwnerships"
 WHERE installation_scope = $1
   AND ($2 OR COALESCE(last_used_at_utc, updated_at_utc) <= $3)
   AND (cleanup_claim_until IS NULL OR cleanup_claim_until <= clock_timestamp())"#;

const CLAIM_CANDIDATES_SQL: &str = r#"WITH candidates AS (
    SELECT installation_scope, canonical_ref
      FROM "BuildImageOwnerships"
     WHERE installation_scope = $1
       AND ($2 OR COALESCE(last_used_at_utc, updated_at_utc) <= $3)
       AND (cleanup_claim_until IS NULL OR cleanup_claim_until <= clock_timestamp())
     ORDER BY cleanup_checked_at_utc NULLS FIRST,
              COALESCE(last_used_at_utc, updated_at_utc), canonical_ref
     FOR UPDATE SKIP LOCKED
     LIMIT $6
)
UPDATE "BuildImageOwnerships" owned
   SET cleanup_claim_token = $4,
       cleanup_claim_until = clock_timestamp() + make_interval(secs => $5),
       cleanup_removal_started = FALSE,
       cleanup_checked_at_utc = clock_timestamp()
  FROM candidates
 WHERE owned.installation_scope = candidates.installation_scope
   AND owned.canonical_ref = candidates.canonical_ref
RETURNING owned.canonical_ref, owned.image_id, owned.updated_at_utc,
          owned.last_used_at_utc, owned.cleanup_claim_token"#;

const RENEW_CLAIM_SQL: &str = r#"UPDATE "BuildImageOwnerships"
   SET cleanup_claim_until = clock_timestamp() + make_interval(secs => $5),
       cleanup_removal_started = TRUE
 WHERE installation_scope = $1 AND canonical_ref = $2
   AND image_id = $3 AND cleanup_claim_token = $4
   AND cleanup_claim_until > clock_timestamp()
   AND NOT EXISTS (
       SELECT 1 FROM "ControlPlaneResourceLeases"
        WHERE resource_key = $6
          AND lease_expires_at_utc > clock_timestamp()
   )"#;

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

#[derive(Clone, Debug, sqlx::FromRow)]
struct CleanupCandidate {
    canonical_ref: String,
    image_id: String,
    updated_at_utc: DateTime<Utc>,
    last_used_at_utc: Option<DateTime<Utc>>,
    cleanup_claim_token: Option<Uuid>,
}

impl CleanupCandidate {
    fn ownership(&self) -> ImageOwnership {
        ImageOwnership {
            canonical_ref: self.canonical_ref.clone(),
            image_id: self.image_id.clone(),
            updated_at_utc: self.updated_at_utc,
            last_used_at_utc: self.last_used_at_utc,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ImageCleanupPass {
    pub report: ImageCleanupReport,
    pub scanned: i64,
    pub claimed: i64,
    pub backlog: i64,
    pub duration_millis: i64,
    pub deadline_expired: bool,
}

#[derive(Default)]
struct ReferenceSnapshot {
    rows: Vec<ChallengeReference>,
    by_canonical_ref: HashMap<String, Vec<usize>>,
    by_image_id: HashMap<String, Vec<usize>>,
    truncated: bool,
}

impl ReferenceSnapshot {
    fn new(mut rows: Vec<ChallengeReference>) -> Self {
        let truncated = rows.len() > MAX_REFERENCE_SNAPSHOT as usize;
        rows.truncate(MAX_REFERENCE_SNAPSHOT as usize);
        let mut snapshot = Self {
            rows,
            truncated,
            ..Self::default()
        };
        for (index, reference) in snapshot.rows.iter().enumerate() {
            for image in [
                reference.container_image.as_deref(),
                reference.ad_checker_image.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                let canonical = crate::controllers::edit::canonical_image_reference(Some(image));
                snapshot
                    .by_canonical_ref
                    .entry(canonical)
                    .or_default()
                    .push(index);
            }
            if reference.variant_generator_build_context_subdir.as_deref()
                == Some(crate::services::git_sync::GENERATOR_CONTEXT_SUBDIR)
                && reference.variant_generator_build_status == ChallengeBuildStatus::Success as i16
                && reference.variant_generator_image == reference.variant_generator_digest
            {
                if let Some(image) = reference.variant_generator_image.as_deref() {
                    snapshot
                        .by_image_id
                        .entry(image.to_ascii_lowercase())
                        .or_default()
                        .push(index);
                }
            }
        }
        snapshot
    }

    fn matching<'a>(&'a self, ownership: &ImageOwnership) -> Vec<&'a ChallengeReference> {
        let mut indexes = HashSet::new();
        if let Some(found) = self.by_canonical_ref.get(&ownership.canonical_ref) {
            indexes.extend(found.iter().copied());
        }
        if let Some(found) = self
            .by_image_id
            .get(&ownership.image_id.to_ascii_lowercase())
        {
            indexes.extend(found.iter().copied());
        }
        let mut indexes = indexes.into_iter().collect::<Vec<_>>();
        indexes.sort_unstable();
        indexes
            .into_iter()
            .filter_map(|index| self.rows.get(index))
            .collect()
    }
}

async fn docker_call<T, E, F>(
    deadline: tokio::time::Instant,
    label: &'static str,
    future: F,
) -> AppResult<T>
where
    E: std::fmt::Display,
    F: Future<Output = Result<T, E>>,
{
    let operation_deadline = deadline.min(tokio::time::Instant::now() + DOCKER_OPERATION_BUDGET);
    tokio::time::timeout_at(operation_deadline, future)
        .await
        .map_err(|_| AppError::unavailable(format!("Docker {label} timed out")))?
        .map_err(|error| AppError::unavailable(format!("Docker {label} failed: {error}")))
}

fn reference_matches(reference: Option<&str>, canonical_ref: &str) -> bool {
    reference.is_some_and(|reference| {
        crate::controllers::edit::canonical_image_reference(Some(reference)) == canonical_ref
    })
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
    ) && matches!(
        reference.challenge_type,
        value if value == ChallengeType::StaticContainer as i16
            || value == ChallengeType::DynamicContainer as i16
    ) && reference.workload_spec.is_none()
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

async fn prune_build_cache(
    retention_hours: i32,
    pressure: bool,
    deadline: tokio::time::Instant,
) -> AppResult<u64> {
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
        .build()
        .map_err(|error| AppError::internal(error.to_string()))?;
    let response = docker_call(
        deadline,
        "build-cache prune",
        client
            .post(format!("http://localhost/{DOCKER_API_VERSION}/build/prune"))
            .query(&[("filters", filters)])
            .send(),
    )
    .await?;
    let status = response.status();
    let response_deadline = deadline.min(tokio::time::Instant::now() + DOCKER_OPERATION_BUDGET);
    let bytes = tokio::time::timeout_at(response_deadline, async move {
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                AppError::unavailable(format!("Docker build-cache prune response failed: {error}"))
            })?;
            let next_len = body
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| AppError::unavailable("Docker prune response was too large"))?;
            if next_len > MAX_DOCKER_RESPONSE_BYTES {
                return Err(AppError::unavailable(format!(
                    "Docker prune response exceeded {MAX_DOCKER_RESPONSE_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })
    .await
    .map_err(|_| AppError::unavailable("Docker build-cache prune response timed out"))??;
    if !status.is_success() {
        let detail = String::from_utf8_lossy(&bytes);
        let detail = detail.chars().take(1_024).collect::<String>();
        return Err(AppError::unavailable(format!(
            "Docker build-cache prune returned {status}: {detail}"
        )));
    }
    let result = serde_json::from_slice::<BuildPruneResponse>(&bytes)
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

fn normalized_image_hex(image_id: &str) -> Option<&str> {
    let hex = image_id
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(image_id.trim());
    (hex.len() >= 12 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(hex)
}

fn image_id_may_match(live_id: &str, owned_id: &str) -> bool {
    let (Some(live), Some(owned)) = (
        normalized_image_hex(live_id),
        normalized_image_hex(owned_id),
    ) else {
        return live_id.eq_ignore_ascii_case(owned_id);
    };
    live.eq_ignore_ascii_case(owned)
        || (live.len() < owned.len()
            && owned
                .get(..live.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(live)))
        || (owned.len() < live.len()
            && live
                .get(..owned.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(owned)))
}

async fn load_references(st: &SharedState) -> AppResult<ReferenceSnapshot> {
    let rows = sqlx::query_as::<_, ChallengeReference>(CHALLENGE_REFERENCES_SQL)
        .bind(MAX_REFERENCE_SNAPSHOT + 1)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ReferenceSnapshot::new(rows))
}

async fn claim_candidates(
    st: &SharedState,
    scope: &str,
    pressure: bool,
    cutoff: DateTime<Utc>,
    token: Uuid,
) -> AppResult<(i64, Vec<CleanupCandidate>)> {
    let total = sqlx::query_scalar::<_, i64>(CANDIDATE_COUNT_SQL)
        .bind(scope)
        .bind(pressure)
        .bind(cutoff)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let candidates = sqlx::query_as::<_, CleanupCandidate>(CLAIM_CANDIDATES_SQL)
        .bind(scope)
        .bind(pressure)
        .bind(cutoff)
        .bind(token)
        .bind(CLAIM_LEASE_SECONDS)
        .bind(CLEANUP_BATCH_SIZE)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((total, candidates))
}

async fn release_claim(st: &SharedState, scope: &str, candidate: &CleanupCandidate) {
    let Some(token) = candidate.cleanup_claim_token else {
        return;
    };
    if let Err(error) = sqlx::query(
        r#"UPDATE "BuildImageOwnerships"
              SET cleanup_claim_token = NULL, cleanup_claim_until = NULL,
                  cleanup_removal_started = FALSE
            WHERE installation_scope = $1 AND canonical_ref = $2
              AND image_id = $3 AND cleanup_claim_token = $4"#,
    )
    .bind(scope)
    .bind(&candidate.canonical_ref)
    .bind(&candidate.image_id)
    .bind(token)
    .execute(st.pg())
    .await
    {
        tracing::warn!(tag=%candidate.canonical_ref, %error, "image cleanup claim release failed");
    }
}

async fn renew_claim_for_removal(
    st: &SharedState,
    scope: &str,
    candidate: &CleanupCandidate,
) -> AppResult<bool> {
    let Some(token) = candidate.cleanup_claim_token else {
        return Ok(false);
    };
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&candidate.canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // Renew while ordered with runtime reservations. The database connection
    // is released before Docker I/O, while the durable live claim makes every
    // cooperating publisher/reservation fail and retry until removal commits.
    let current = sqlx::query(RENEW_CLAIM_SQL)
        .bind(scope)
        .bind(&candidate.canonical_ref)
        .bind(&candidate.image_id)
        .bind(token)
        .bind(CLAIM_LEASE_SECONDS)
        .bind(&lock_key)
        .execute(lock.connection_mut())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|error| AppError::internal(error.to_string()));
    let released = lock.release().await;
    let current = current?;
    released?;
    Ok(current)
}

async fn commit_removed_identity(
    st: &SharedState,
    scope: &str,
    candidate: &CleanupCandidate,
) -> AppResult<bool> {
    let Some(token) = candidate.cleanup_claim_token else {
        return Ok(false);
    };
    let lock_key = crate::controllers::edit::image_build_lock_key(Some(&candidate.canonical_ref));
    let mut lock = crate::utils::single_flight::PgAdvisoryLock::acquire_build(st.pg(), &lock_key)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let deleted = sqlx::query(
        r#"DELETE FROM "BuildImageOwnerships"
            WHERE installation_scope = $1 AND canonical_ref = $2
              AND image_id = $3 AND cleanup_claim_token = $4"#,
    )
    .bind(scope)
    .bind(&candidate.canonical_ref)
    .bind(&candidate.image_id)
    .bind(token)
    .execute(lock.connection_mut())
    .await
    .map(|result| result.rows_affected() == 1)
    .map_err(|error| AppError::internal(error.to_string()));
    let released = lock.release().await;
    let deleted = deleted?;
    released?;
    Ok(deleted)
}

#[derive(Default)]
struct CandidateOutcome {
    removed: bool,
    ledger_removed: bool,
    bytes: u64,
    message: Option<String>,
}

impl CandidateOutcome {
    fn retained(message: impl Into<String>) -> Self {
        Self {
            message: Some(message.into()),
            ..Self::default()
        }
    }
}

fn remaining_backlog(scanned: i64, ledger_removed: i64) -> i64 {
    scanned.saturating_sub(ledger_removed).max(0)
}

#[allow(clippy::too_many_arguments)]
async fn evict_candidate(
    st: SharedState,
    docker: Docker,
    scope: Arc<str>,
    references: Arc<ReferenceSnapshot>,
    live_image_ids: Arc<HashSet<String>>,
    candidate: CleanupCandidate,
    cutoff: DateTime<Utc>,
    pressure: bool,
    deadline: tokio::time::Instant,
) -> CandidateOutcome {
    let ownership = candidate.ownership();
    let matching = references.matching(&ownership);
    let orphan = matching.is_empty();
    let expired = ownership.retention_anchor() <= cutoff;
    if !(expired || (pressure && orphan))
        || (!orphan
            && !matching
                .iter()
                .all(|reference| reference_is_rebuildable(reference, &ownership)))
    {
        release_claim(&st, &scope, &candidate).await;
        return CandidateOutcome::default();
    }
    if live_image_ids
        .iter()
        .any(|live| image_id_may_match(live, &candidate.image_id))
    {
        release_claim(&st, &scope, &candidate).await;
        return CandidateOutcome::default();
    }

    let inspect_deadline = deadline.min(tokio::time::Instant::now() + DOCKER_OPERATION_BUDGET);
    let inspected = match tokio::time::timeout_at(
        inspect_deadline,
        docker.inspect_image(&candidate.canonical_ref),
    )
    .await
    {
        Ok(Ok(inspected)) => inspected,
        Ok(Err(error)) if docker_not_found(&error) => {
            let removed = commit_removed_identity(&st, &scope, &candidate)
                .await
                .unwrap_or(false);
            if !removed {
                release_claim(&st, &scope, &candidate).await;
            }
            return CandidateOutcome {
                ledger_removed: removed,
                message: removed.then(|| {
                    format!(
                        "removed stale ownership for absent image {}",
                        candidate.canonical_ref
                    )
                }),
                ..CandidateOutcome::default()
            };
        }
        Ok(Err(error)) => {
            release_claim(&st, &scope, &candidate).await;
            return CandidateOutcome::retained(format!(
                "{} was retained: Docker image inspection failed: {error}",
                candidate.canonical_ref
            ));
        }
        Err(_) => {
            release_claim(&st, &scope, &candidate).await;
            return CandidateOutcome::retained(format!(
                "{} was retained: Docker image inspection timed out",
                candidate.canonical_ref
            ));
        }
    };
    let inspected_id = crate::services::challenge_images::inspected_local_image_id(&inspected);
    if !inspected_id.is_some_and(|image_id| image_id.eq_ignore_ascii_case(&candidate.image_id)) {
        release_claim(&st, &scope, &candidate).await;
        return CandidateOutcome::retained(format!(
            "{} was retained: Docker tag identity changed",
            candidate.canonical_ref
        ));
    }
    if let Err(error) = crate::services::challenge_images::validate_image_ownership_labels(
        &inspected,
        &scope,
        &candidate.canonical_ref,
        false,
    ) {
        release_claim(&st, &scope, &candidate).await;
        return CandidateOutcome::retained(format!(
            "{} was retained: {error}",
            candidate.canonical_ref
        ));
    }
    let owns_tag = inspected.repo_tags.as_ref().is_some_and(|tags| {
        tags.iter().any(|tag| {
            crate::controllers::edit::canonical_image_reference(Some(tag))
                == candidate.canonical_ref
        })
    });
    if !owns_tag {
        release_claim(&st, &scope, &candidate).await;
        return CandidateOutcome::retained(format!(
            "{} was retained: Docker no longer assigns its canonical tag",
            candidate.canonical_ref
        ));
    }
    if !renew_claim_for_removal(&st, &scope, &candidate)
        .await
        .unwrap_or(false)
    {
        release_claim(&st, &scope, &candidate).await;
        return CandidateOutcome::default();
    }

    let size = inspected
        .size
        .and_then(|size| u64::try_from(size).ok())
        .unwrap_or_default();
    if let Err(error) = docker_call(
        deadline,
        "image removal",
        docker.remove_image(
            &candidate.image_id,
            Some(RemoveImageOptions {
                force: false,
                noprune: false,
            }),
            None,
        ),
    )
    .await
    {
        return CandidateOutcome {
            ..CandidateOutcome::retained(format!(
                "{} removal was not confirmed: {error}; cleanup claim retained until expiry",
                candidate.canonical_ref
            ))
        };
    }
    match tokio::time::timeout_at(
        deadline.min(tokio::time::Instant::now() + DOCKER_OPERATION_BUDGET),
        docker.inspect_image(&candidate.image_id),
    )
    .await
    {
        Ok(Err(error)) if docker_not_found(&error) => {}
        Ok(Err(error)) => {
            return CandidateOutcome {
                ..CandidateOutcome::retained(format!(
                    "{} removal could not be verified: {error}; cleanup claim retained until expiry",
                    candidate.canonical_ref
                ))
            };
        }
        Ok(Ok(_)) => {
            return CandidateOutcome {
                ..CandidateOutcome::retained(format!(
                    "{} still exists after Docker removal; cleanup claim retained until expiry",
                    candidate.canonical_ref
                ))
            };
        }
        Err(_) => {
            return CandidateOutcome {
                ..CandidateOutcome::retained(format!(
                    "{} removal verification timed out; cleanup claim retained until expiry",
                    candidate.canonical_ref
                ))
            };
        }
    }

    let (committed, ledger_note) = match commit_removed_identity(&st, &scope, &candidate).await {
        Ok(true) => (true, String::new()),
        Ok(false) => (
            false,
            "; ownership changed concurrently and was preserved; cleanup claim retained until expiry"
                .to_string(),
        ),
        Err(error) => (
            false,
            format!(
                "; ownership ledger cleanup failed ({error}); cleanup claim retained until expiry"
            ),
        ),
    };
    let titles = matching
        .iter()
        .map(|reference| format!("#{} {}", reference.id, reference.title))
        .collect::<Vec<_>>();
    let detail = if titles.is_empty() {
        "orphaned image".to_string()
    } else {
        format!("rebuildable for {}", titles.join(", "))
    };
    CandidateOutcome {
        removed: true,
        ledger_removed: committed,
        bytes: size,
        message: Some(format!(
            "evicted {} ({detail}){ledger_note}",
            candidate.canonical_ref
        )),
    }
}

pub(crate) async fn cleanup_with_deadline(
    st: &SharedState,
    policy: &ContainerPolicy,
    deadline: tokio::time::Instant,
) -> AppResult<ImageCleanupPass> {
    let started = tokio::time::Instant::now();
    policy.validate()?;
    let docker = connect_local_docker_until(deadline)
        .await
        .map_err(AppError::unavailable)?;
    let before = storage_status_with(&docker, policy, deadline).await?;
    let mut report = ImageCleanupReport {
        available_bytes_before: before.filesystem_available_bytes,
        available_bytes_after: before.filesystem_available_bytes,
        minimum_free_bytes: before.minimum_free_bytes,
        pressure_mode: before.low_storage,
        ..Default::default()
    };

    match prune_build_cache(
        policy.build_cache_retention_hours,
        before.low_storage,
        deadline,
    )
    .await
    {
        Ok(bytes) => report.cache_bytes_reclaimed = bytes,
        Err(error) => report
            .messages
            .push(format!("build-cache cleanup skipped: {error}")),
    }
    if before.low_storage {
        let filters = HashMap::from([("dangling".to_string(), vec!["true".to_string()])]);
        match docker_call(
            deadline,
            "dangling-image prune",
            docker.prune_images(Some(PruneImagesOptions { filters })),
        )
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

    let references = Arc::new(load_references(st).await?);
    let containers = docker_call(
        deadline,
        "container inventory",
        docker.list_containers(Some(ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        })),
    )
    .await?;
    let live_image_ids = Arc::new(
        containers
            .into_iter()
            .filter_map(|container| container.image_id)
            .collect::<HashSet<_>>(),
    );
    let cutoff = Utc::now() - ChronoDuration::hours(i64::from(policy.image_idle_retention_hours));
    let scope: Arc<str> = crate::services::container::docker_installation_scope().into();
    let token = Uuid::new_v4();
    let (scanned, candidates) =
        claim_candidates(st, &scope, before.low_storage, cutoff, token).await?;
    let claimed = i64::try_from(candidates.len()).unwrap_or(i64::MAX);

    if references.truncated {
        for candidate in &candidates {
            release_claim(st, &scope, candidate).await;
        }
        report.messages.push(format!(
            "image eviction skipped because the challenge reference snapshot exceeded {MAX_REFERENCE_SNAPSHOT} rows"
        ));
        return Ok(ImageCleanupPass {
            report,
            scanned,
            claimed,
            backlog: scanned,
            duration_millis: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
            deadline_expired: tokio::time::Instant::now() >= deadline,
        });
    }

    let outcomes = futures::stream::iter(candidates.into_iter().map(|candidate| {
        evict_candidate(
            st.clone(),
            docker.clone(),
            scope.clone(),
            references.clone(),
            live_image_ids.clone(),
            candidate,
            cutoff,
            before.low_storage,
            deadline,
        )
    }))
    .buffer_unordered(CLEANUP_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut ledger_removed = 0_i64;
    for outcome in outcomes {
        ledger_removed += i64::from(outcome.ledger_removed);
        if outcome.removed {
            report.images_removed += 1;
            report.image_bytes_evicted = report.image_bytes_evicted.saturating_add(outcome.bytes);
        }
        if let Some(message) = outcome.message {
            report.messages.push(message);
        }
    }
    // A Docker removal and its ledger commit describe the same ownership row.
    // Only a successful identity commit resolves that row from the backlog;
    // counting the daemon removal as well would subtract normal evictions twice.
    let backlog = remaining_backlog(scanned, ledger_removed);
    let (_, available_after) = filesystem_space(std::path::Path::new("/"))?;
    report.available_bytes_after = available_after;
    if report.minimum_free_bytes > 0 && available_after < report.minimum_free_bytes {
        report.messages.push(format!(
            "free storage remains below the configured floor ({} < {} bytes); recent, active, or non-rebuildable images were retained",
            available_after, report.minimum_free_bytes
        ));
    }
    if scanned > claimed {
        report.messages.push(format!(
            "{} cleanup candidate(s) remain for a later bounded pass",
            scanned.saturating_sub(claimed)
        ));
    }
    Ok(ImageCleanupPass {
        report,
        scanned,
        claimed,
        backlog,
        duration_millis: i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
        deadline_expired: tokio::time::Instant::now() >= deadline,
    })
}

pub async fn cleanup(st: &SharedState, policy: &ContainerPolicy) -> AppResult<ImageCleanupReport> {
    let deadline = tokio::time::Instant::now() + ADMIN_CLEANUP_BUDGET;
    cleanup_with_deadline(st, policy, deadline)
        .await
        .map(|pass| pass.report)
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod tests;
