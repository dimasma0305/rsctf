//! Bounded lifecycle for locally-built, reproducible challenge images.

use std::path::Path;
use std::time::Duration;

use bollard::Docker;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::services::container_policy::ContainerPolicy;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};

const GIB: u64 = 1024 * 1024 * 1024;

const RESERVE_RUNTIME_IMAGE_SQL: &str = r#"WITH reserved AS (
    UPDATE "BuildImageOwnerships"
       SET last_used_at_utc = clock_timestamp(),
           cleanup_claim_token = NULL,
           cleanup_claim_until = NULL,
           cleanup_removal_started = FALSE
     WHERE installation_scope = $1
       AND canonical_ref = $2
       AND image_id = $3
       AND (cleanup_removal_started = FALSE
            OR cleanup_claim_until <= clock_timestamp())
     RETURNING 1
)
SELECT EXISTS (SELECT 1 FROM reserved),
       EXISTS (
           SELECT 1 FROM "BuildImageOwnerships"
            WHERE installation_scope = $1
              AND canonical_ref = $2
              AND image_id = $3
       )"#;

#[path = "image_storage/cleanup.rs"]
mod cleanup;
pub use cleanup::cleanup;
pub(crate) use cleanup::{cleanup_with_deadline, ImageCleanupPass};

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
    connect_local_docker_until(tokio::time::Instant::now() + Duration::from_secs(2)).await
}

pub(super) async fn connect_local_docker_until(
    deadline: tokio::time::Instant,
) -> Result<Docker, String> {
    let docker = Docker::connect_with_local_defaults()
        .map_err(|error| format!("Docker connection failed: {error}"))?;
    let ping_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(2));
    match tokio::time::timeout_at(ping_deadline, docker.ping()).await {
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

pub(super) async fn storage_status_with(
    docker: &Docker,
    policy: &ContainerPolicy,
    deadline: tokio::time::Instant,
) -> AppResult<ImageStorageStatus> {
    let (filesystem_total_bytes, filesystem_available_bytes) = filesystem_space(Path::new("/"))?;
    let usage = tokio::time::timeout_at(deadline, docker.df())
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
    storage_status_with(
        &docker,
        &policy,
        tokio::time::Instant::now() + Duration::from_secs(30),
    )
    .await
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
    // A cheap preclaim can be superseded while this image lock is held. Once
    // cleanup marks daemon removal started, runtime startup must retry; an
    // expired finalizing fence is safe to recover.
    let (reserved, owned) = sqlx::query_as::<_, (bool, bool)>(RESERVE_RUNTIME_IMAGE_SQL)
        .bind(&scope)
        .bind(&canonical_ref)
        .bind(immutable_image)
        .fetch_one(lock.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !reserved {
        lock.release().await?;
        if owned {
            return Err(AppError::overloaded(
                "Image cleanup is finalizing this runtime image; retry shortly",
                1,
            ));
        }
        let present = st.containers.image_exists(immutable_image).await;
        return Ok(if present {
            RuntimeImageReservation::Unmanaged
        } else {
            RuntimeImageReservation::Missing
        });
    }
    lock.release().await?;
    Ok(if st.containers.image_exists(immutable_image).await {
        RuntimeImageReservation::Ready
    } else {
        RuntimeImageReservation::Missing
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn ownership() -> ImageOwnership {
        ImageOwnership {
            canonical_ref: "docker.io/rsctf/game/app:latest".to_string(),
            image_id: format!("sha256:{}", "a".repeat(64)),
            updated_at_utc: Utc::now(),
            last_used_at_utc: None,
        }
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

    #[test]
    fn runtime_reservation_supersedes_only_a_preclaim_or_expired_fence() {
        assert!(RESERVE_RUNTIME_IMAGE_SQL.contains("WITH reserved AS"));
        assert!(RESERVE_RUNTIME_IMAGE_SQL.contains("cleanup_removal_started = FALSE"));
        assert!(RESERVE_RUNTIME_IMAGE_SQL.contains("cleanup_claim_until <= clock_timestamp()"));
        assert!(RESERVE_RUNTIME_IMAGE_SQL.contains("cleanup_claim_until = NULL"));
        assert!(RESERVE_RUNTIME_IMAGE_SQL.contains("RETURNING 1"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn runtime_reservation_preempts_preclaim_but_not_live_finalization() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("runtime_image_claim_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "BuildImageOwnerships" (
                 installation_scope TEXT NOT NULL,
                 canonical_ref TEXT NOT NULL,
                 image_id TEXT NOT NULL,
                 last_used_at_utc TIMESTAMPTZ NULL,
                 cleanup_claim_token UUID NULL,
                 cleanup_claim_until TIMESTAMPTZ NULL,
                 cleanup_removal_started BOOLEAN NOT NULL DEFAULT FALSE,
                 PRIMARY KEY (installation_scope, canonical_ref)
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let scope = "0123456789abcdef0123456789abcdef";
        let canonical = "docker.io/rsctf/game/runtime:latest";
        let image_id = format!("sha256:{}", "a".repeat(64));
        let preclaim = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "BuildImageOwnerships"
                 (installation_scope, canonical_ref, image_id,
                  cleanup_claim_token, cleanup_claim_until)
               VALUES ($1, $2, $3, $4, clock_timestamp() + interval '2 minutes')"#,
        )
        .bind(scope)
        .bind(canonical)
        .bind(&image_id)
        .bind(preclaim)
        .execute(&pool)
        .await
        .unwrap();

        let preempted: (bool, bool) = sqlx::query_as(RESERVE_RUNTIME_IMAGE_SQL)
            .bind(scope)
            .bind(canonical)
            .bind(&image_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(preempted, (true, true));
        let cleared: (Option<uuid::Uuid>, Option<chrono::DateTime<Utc>>, bool) = sqlx::query_as(
            r#"SELECT cleanup_claim_token, cleanup_claim_until, cleanup_removal_started
                 FROM "BuildImageOwnerships"
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cleared, (None, None, false));

        let finalizing = uuid::Uuid::new_v4();
        sqlx::query(
            r#"UPDATE "BuildImageOwnerships"
                  SET cleanup_claim_token = $3,
                      cleanup_claim_until = clock_timestamp() + interval '2 minutes',
                      cleanup_removal_started = TRUE
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .bind(finalizing)
        .execute(&pool)
        .await
        .unwrap();
        let blocked: (bool, bool) = sqlx::query_as(RESERVE_RUNTIME_IMAGE_SQL)
            .bind(scope)
            .bind(canonical)
            .bind(&image_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(blocked, (false, true));
        let retained: (Option<uuid::Uuid>, bool) = sqlx::query_as(
            r#"SELECT cleanup_claim_token, cleanup_removal_started
                 FROM "BuildImageOwnerships"
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(retained, (Some(finalizing), true));

        sqlx::query(
            r#"UPDATE "BuildImageOwnerships"
                  SET cleanup_claim_until = clock_timestamp() - interval '1 second'
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .execute(&pool)
        .await
        .unwrap();
        let recovered: (bool, bool) = sqlx::query_as(RESERVE_RUNTIME_IMAGE_SQL)
            .bind(scope)
            .bind(canonical)
            .bind(&image_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(recovered, (true, true));
        let recovered_claim: (Option<uuid::Uuid>, bool) = sqlx::query_as(
            r#"SELECT cleanup_claim_token, cleanup_removal_started
                 FROM "BuildImageOwnerships"
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(scope)
        .bind(canonical)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(recovered_claim, (None, false));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
