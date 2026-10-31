//! Conservative garbage collection for immutable checker revisions.
//!
//! Revisions may still be executing after their owning challenge is replaced or
//! deleted. The collector starts a durable grace marker when it first observes
//! an exact path as unreachable, then takes that revision's cross-process
//! execution lock before deleting a bounded batch under the publication lock.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::{Duration, SystemTime};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use crate::utils::single_flight::PgAdvisoryLock;

#[path = "checker_gc_scan.rs"]
mod discovery;

const CHECKER_GC_LOCK_KEY: &str = "checker-artifacts:publication-gc";
const CHECKER_GC_GRACE: Duration = Duration::from_secs(15 * 60);
const CHECKER_GC_BATCH: usize = 128;
pub(super) const EXECUTION_LOCK_FILE: &str = ".execution.lock";
const UNREACHABLE_MARKER_FILE: &str = ".unreachable-since";
const REACHABLE_SQL: &str = r#"
    SELECT EXISTS(
        SELECT 1 FROM "GameChallenges"
         WHERE ad_checker_image = $1
        UNION ALL
        SELECT 1 FROM "CompetitionMaintenanceWindows"
         WHERE before_snapshot @> jsonb_build_array(jsonb_build_object('ad_checker_image', $1::text))
            OR after_snapshot @> jsonb_build_array(jsonb_build_object('ad_checker_image', $1::text))
    )
"#;

/// Publication and activation paths must hold this guard from before the
/// checker directory is published until its exact path is committed in
/// `GameChallenges`. The collector takes the same guard, closing the otherwise
/// possible check-then-delete race across replicas.
pub(crate) async fn acquire_checker_artifact_guard(
    pool: &sqlx::PgPool,
) -> AppResult<PgAdvisoryLock> {
    PgAdvisoryLock::acquire(pool, CHECKER_GC_LOCK_KEY)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Cross-process shared lease held for the complete checker execution. The
/// collector takes the same revision's lock exclusively before deletion.
pub(crate) struct CheckerExecutionLease {
    _lock: Flock<File>,
}

pub(crate) fn acquire_checker_execution_lease(
    checker_revision: &Path,
) -> std::io::Result<CheckerExecutionLease> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(checker_revision.join(EXECUTION_LOCK_FILE))?;
    Flock::lock(file, FlockArg::LockSharedNonblock)
        .map(|lock| CheckerExecutionLease { _lock: lock })
        .map_err(|(_, error)| std::io::Error::from_raw_os_error(error as i32))
}

fn try_acquire_checker_gc_lease(checker_revision: &Path) -> std::io::Result<Option<Flock<File>>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(checker_revision.join(EXECUTION_LOCK_FILE))?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(Some(lock)),
        Err((_, Errno::EAGAIN)) => Ok(None),
        Err((_, error)) => Err(std::io::Error::from_raw_os_error(error as i32)),
    }
}

fn revision_name_is_managed(name: &str) -> bool {
    let uuid =
        |value: &str| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if uuid(name) {
        return true;
    }
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((revision, staging)) = rest.split_once(".staging-") else {
        return false;
    };
    uuid(revision) && uuid(staging)
}

fn old_enough(modified: SystemTime, now: SystemTime, grace: Duration) -> bool {
    now.duration_since(modified).is_ok_and(|age| age >= grace)
}

async fn collect_with_cursor(
    pool: &sqlx::PgPool,
    storage_root: &Path,
    grace: Duration,
    limit: usize,
    cursor: &mut discovery::Discovery,
) -> AppResult<u64> {
    let candidates = cursor
        .scan(limit)
        .await
        .map_err(|error| AppError::internal(format!("checker GC discovery: {error}")))?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let canonical_root = tokio::fs::canonicalize(storage_root.join("checkers"))
        .await
        .map_err(|error| AppError::internal(format!("checker GC root: {error}")))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut removed = 0;
    for candidate in candidates {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let mut guard = acquire_checker_artifact_guard(pool).await?;
        let reachable: bool = sqlx::query_scalar(REACHABLE_SQL)
            .bind(candidate.to_string_lossy().as_ref())
            .fetch_one(&mut **guard.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let marker = candidate.join(UNREACHABLE_MARKER_FILE);
        if reachable {
            let _ = tokio::fs::remove_file(marker).await;
            guard.release().await?;
            continue;
        }
        let metadata = match tokio::fs::symlink_metadata(&candidate).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                guard.release().await?;
                continue;
            }
            Err(error) => return Err(AppError::internal(format!("checker GC restat: {error}"))),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            guard.release().await?;
            continue;
        }
        let canonical_candidate = tokio::fs::canonicalize(&candidate)
            .await
            .map_err(|error| AppError::internal(format!("checker GC revalidate: {error}")))?;
        if !canonical_candidate.starts_with(&canonical_root) {
            guard.release().await?;
            continue;
        }
        let unreachable_since = match tokio::fs::symlink_metadata(&marker).await {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata
                .modified()
                .map_err(|error| AppError::internal(error.to_string()))?,
            Ok(_) => {
                guard.release().await?;
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let created = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&marker)
                    .await;
                if let Err(error) = created {
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(AppError::internal(format!("checker GC mark: {error}")));
                    }
                }
                guard.release().await?;
                continue;
            }
            Err(error) => return Err(AppError::internal(format!("checker GC marker: {error}"))),
        };
        if !old_enough(unreachable_since, SystemTime::now(), grace) {
            guard.release().await?;
            continue;
        }
        let Some(_execution_fence) = try_acquire_checker_gc_lease(&candidate)
            .map_err(|error| AppError::internal(format!("checker GC execution fence: {error}")))?
        else {
            guard.release().await?;
            continue;
        };
        // Atomically withdraw the unreachable name while publication is fenced,
        // then delete outside the database lock. GC also recognizes this owned
        // staging name if the process crashes before deletion completes.
        let tombstone = candidate.with_file_name(format!(
            ".{}.staging-{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::rename(&candidate, &tombstone)
            .await
            .map_err(|error| AppError::internal(format!("checker GC withdraw: {error}")))?;
        guard.release().await?;
        tokio::fs::remove_dir_all(tombstone)
            .await
            .map_err(|error| AppError::internal(format!("checker GC remove: {error}")))?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
pub(super) async fn collect_stale_checker_revisions_at(
    pool: &sqlx::PgPool,
    storage_root: &Path,
    grace: Duration,
    limit: usize,
) -> AppResult<u64> {
    collect_with_cursor(
        pool,
        storage_root,
        grace,
        limit,
        &mut discovery::Discovery::new(storage_root),
    )
    .await
}

/// The process retains only four directory iterators. Every pass bounds both
/// discovery and deletion, including catalogs consisting entirely of live paths.
pub async fn collect_stale_checker_revisions(state: &SharedState) -> AppResult<u64> {
    static CURSOR: std::sync::OnceLock<tokio::sync::Mutex<discovery::Discovery>> =
        std::sync::OnceLock::new();
    let root = Path::new(&state.config.storage_root);
    let mut cursor = CURSOR
        .get_or_init(|| tokio::sync::Mutex::new(discovery::Discovery::new(root)))
        .lock()
        .await;
    let result = collect_with_cursor(
        state.pg(),
        root,
        CHECKER_GC_GRACE,
        CHECKER_GC_BATCH,
        &mut cursor,
    )
    .await;
    if result.is_err() {
        *cursor = discovery::Discovery::new(root);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn recognizes_only_owned_revision_and_staging_names() {
        let uuid = "a".repeat(32);
        assert!(revision_name_is_managed(&uuid));
        assert!(revision_name_is_managed(&format!(
            ".{uuid}.staging-{}",
            "b".repeat(32)
        )));
        assert!(!revision_name_is_managed("latest"));
        assert!(!revision_name_is_managed("../outside"));
        assert!(!revision_name_is_managed(&format!("{uuid}-extra")));
    }

    #[test]
    fn grace_rejects_recent_and_future_directories() {
        let now = SystemTime::now();
        assert!(!old_enough(now, now, CHECKER_GC_GRACE));
        assert!(old_enough(now - CHECKER_GC_GRACE, now, CHECKER_GC_GRACE));
        assert!(!old_enough(
            now + Duration::from_secs(1),
            now,
            CHECKER_GC_GRACE
        ));
    }

    #[test]
    fn reachability_is_an_exact_path_match() {
        assert!(REACHABLE_SQL.contains("ad_checker_image = $1"));
        assert!(!REACHABLE_SQL.contains("LIKE"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn sweep_keeps_exactly_reachable_revision_and_removes_stale_peer() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("checker_gc_{}", uuid::Uuid::new_v4().simple());
        sqlx::raw_sql(&format!(
            r#"CREATE SCHEMA "{schema}";
               CREATE TABLE "{schema}"."GameChallenges" (
                   id INTEGER PRIMARY KEY, ad_checker_image TEXT
               );
               CREATE TABLE "{schema}"."CompetitionMaintenanceWindows" (
                   before_snapshot JSONB, after_snapshot JSONB
               )"#
        ))
        .execute(&admin)
        .await
        .unwrap();
        let search_path = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _| {
                let sql = format!(r#"SET search_path TO "{search_path}""#);
                Box::pin(async move {
                    sqlx::query(&sql).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();

        let root = std::env::temp_dir().join(format!(
            "rsctf-checker-gc-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let revisions = root.join("checkers/7/service/revisions");
        let reachable = revisions.join("a".repeat(32));
        let stale = revisions.join("b".repeat(32));
        tokio::fs::create_dir_all(&reachable).await.unwrap();
        tokio::fs::create_dir_all(&stale).await.unwrap();
        tokio::fs::write(reachable.join("run.py"), b"reachable")
            .await
            .unwrap();
        tokio::fs::write(stale.join("run.py"), b"stale")
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (1, $1)"#)
            .bind(reachable.to_string_lossy().as_ref())
            .execute(&pool)
            .await
            .unwrap();

        let marked = collect_stale_checker_revisions_at(&pool, &root, Duration::ZERO, 16)
            .await
            .unwrap();
        assert_eq!(marked, 0);
        let execution = acquire_checker_execution_lease(&stale).unwrap();
        let held = collect_stale_checker_revisions_at(&pool, &root, Duration::ZERO, 16)
            .await
            .unwrap();
        assert_eq!(held, 0);
        assert!(tokio::fs::try_exists(&stale).await.unwrap());
        drop(execution);
        let removed = collect_stale_checker_revisions_at(&pool, &root, Duration::ZERO, 16)
            .await
            .unwrap();
        assert_eq!(removed, 1);
        assert!(tokio::fs::try_exists(&reachable).await.unwrap());
        assert!(!tokio::fs::try_exists(&stale).await.unwrap());

        // A prior checker remains reachable through the event's audit history
        // after the live definition switches to a different revision.
        sqlx::query(r#"INSERT INTO "CompetitionMaintenanceWindows" VALUES ($1,NULL)"#)
            .bind(serde_json::json!([{"ad_checker_image": reachable.to_string_lossy()}]))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "GameChallenges""#)
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                collect_stale_checker_revisions_at(&pool, &root, Duration::ZERO, 16)
                    .await
                    .unwrap(),
                0
            );
        }
        assert!(tokio::fs::try_exists(&reachable).await.unwrap());

        pool.close().await;
        tokio::fs::remove_dir_all(&root).await.unwrap();
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
