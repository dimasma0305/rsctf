//! Conservative garbage collection for immutable checker revisions.
//!
//! Revisions may still be executing after their owning challenge is replaced or
//! deleted. The collector starts a durable grace marker when it first observes
//! an exact path as unreachable, then takes that revision's cross-process
//! execution lock before deleting a bounded batch under the publication lock.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use crate::utils::single_flight::PgAdvisoryLock;

const CHECKER_GC_LOCK_KEY: &str = "checker-artifacts:publication-gc";
const CHECKER_GC_GRACE: Duration = Duration::from_secs(15 * 60);
const CHECKER_GC_BATCH: usize = 128;
pub(super) const EXECUTION_LOCK_FILE: &str = ".execution.lock";
const UNREACHABLE_MARKER_FILE: &str = ".unreachable-since";
const REACHABLE_SQL: &str = r#"
    SELECT EXISTS(
        SELECT 1 FROM "GameChallenges"
         WHERE ad_checker_image = $1
    )
"#;
const REACHABLE_PATHS_SQL: &str = r#"
    SELECT ad_checker_image
      FROM "GameChallenges"
     WHERE ad_checker_image IS NOT NULL
       AND ad_checker_image <> ''
"#;

/// Publication and activation paths must hold this guard from before the
/// checker directory is prepared until its exact path is committed in
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

async fn directory_children(path: &Path) -> AppResult<Vec<PathBuf>> {
    let mut reader = tokio::fs::read_dir(path).await.map_err(|error| {
        AppError::internal(format!("checker GC read {}: {error}", path.display()))
    })?;
    let mut paths = Vec::new();
    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|error| AppError::internal(format!("checker GC read entry: {error}")))?
    {
        let file_type = entry
            .file_type()
            .await
            .map_err(|error| AppError::internal(format!("checker GC stat: {error}")))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            paths.push(entry.path());
        }
    }
    paths.sort_unstable();
    Ok(paths)
}

async fn collect_candidates(
    storage_root: &Path,
    grace: Duration,
    limit: usize,
    reachable: &HashSet<String>,
) -> AppResult<(PathBuf, Vec<PathBuf>)> {
    let checkers = storage_root.join("checkers");
    let metadata = match tokio::fs::symlink_metadata(&checkers).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((checkers, Vec::new()));
        }
        Err(error) => {
            return Err(AppError::internal(format!(
                "checker GC stat {}: {error}",
                checkers.display()
            )));
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppError::internal(
            "checker GC root must be a real directory",
        ));
    }
    let canonical_root = tokio::fs::canonicalize(&checkers)
        .await
        .map_err(|error| AppError::internal(format!("checker GC canonical root: {error}")))?;
    let now = SystemTime::now();
    let mut candidates = Vec::new();
    'games: for game in directory_children(&checkers).await? {
        for challenge in directory_children(&game).await? {
            let revisions = challenge.join("revisions");
            let revisions_metadata = match tokio::fs::symlink_metadata(&revisions).await {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(AppError::internal(format!(
                        "checker GC stat {}: {error}",
                        revisions.display()
                    )));
                }
            };
            let _ = revisions_metadata;
            let canonical_revisions =
                tokio::fs::canonicalize(&revisions).await.map_err(|error| {
                    AppError::internal(format!("checker GC canonical path: {error}"))
                })?;
            if !canonical_revisions.starts_with(&canonical_root) {
                continue;
            }
            for candidate in directory_children(&revisions).await? {
                let managed = candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(revision_name_is_managed);
                if !managed {
                    continue;
                }
                let canonical_candidate =
                    tokio::fs::canonicalize(&candidate).await.map_err(|error| {
                        AppError::internal(format!("checker GC canonical path: {error}"))
                    })?;
                if canonical_candidate.parent() != Some(canonical_revisions.as_path())
                    || !canonical_candidate.starts_with(&canonical_root)
                {
                    continue;
                }
                let marker = candidate.join(UNREACHABLE_MARKER_FILE);
                if reachable.contains(candidate.to_string_lossy().as_ref()) {
                    if let Err(error) = tokio::fs::remove_file(&marker).await {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            return Err(AppError::internal(format!(
                                "checker GC clear marker {}: {error}",
                                marker.display()
                            )));
                        }
                    }
                    continue;
                }
                let marker_metadata = match tokio::fs::symlink_metadata(&marker).await {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        metadata
                    }
                    Ok(_) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        match tokio::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&marker)
                            .await
                        {
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                            Err(error) => {
                                return Err(AppError::internal(format!(
                                    "checker GC mark {}: {error}",
                                    marker.display()
                                )));
                            }
                        }
                        // The safety grace begins when unreachability is first
                        // observed, never at the revision's publication time.
                        continue;
                    }
                    Err(error) => {
                        return Err(AppError::internal(format!(
                            "checker GC stat marker {}: {error}",
                            marker.display()
                        )));
                    }
                };
                let unreachable_since = marker_metadata.modified().map_err(|error| {
                    AppError::internal(format!("checker GC marker time: {error}"))
                })?;
                if !old_enough(unreachable_since, now, grace) {
                    continue;
                }
                candidates.push(candidate);
                if candidates.len() >= limit {
                    break 'games;
                }
            }
        }
    }
    Ok((canonical_root, candidates))
}

async fn collect_stale_checker_revisions_at(
    pool: &sqlx::PgPool,
    storage_root: &Path,
    grace: Duration,
    limit: usize,
) -> AppResult<u64> {
    let mut guard = acquire_checker_artifact_guard(pool).await?;
    let reachable = sqlx::query_scalar::<_, String>(REACHABLE_PATHS_SQL)
        .fetch_all(&mut **guard.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .collect::<HashSet<_>>();
    let (canonical_root, candidates) =
        collect_candidates(storage_root, grace, limit, &reachable).await?;
    let mut removed = 0;
    for candidate in candidates {
        let database_path = candidate.to_string_lossy();
        let reachable: bool = sqlx::query_scalar(REACHABLE_SQL)
            .bind(database_path.as_ref())
            .fetch_one(&mut **guard.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if reachable {
            let _ = tokio::fs::remove_file(candidate.join(UNREACHABLE_MARKER_FILE)).await;
            continue;
        }

        // Revalidate immediately before deletion. A rename or symlink swap
        // between discovery and sweep fails closed.
        let metadata = match tokio::fs::symlink_metadata(&candidate).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(AppError::internal(format!("checker GC restat: {error}")));
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let canonical_candidate = tokio::fs::canonicalize(&candidate)
            .await
            .map_err(|error| AppError::internal(format!("checker GC revalidate: {error}")))?;
        if !canonical_candidate.starts_with(&canonical_root) {
            continue;
        }
        let Some(_execution_fence) = try_acquire_checker_gc_lease(&candidate).map_err(|error| {
            AppError::internal(format!(
                "checker GC execution fence {}: {error}",
                candidate.display()
            ))
        })?
        else {
            continue;
        };
        tokio::fs::remove_dir_all(&candidate)
            .await
            .map_err(|error| {
                AppError::internal(format!(
                    "checker GC remove {}: {error}",
                    candidate.display()
                ))
            })?;
        removed += 1;
    }
    guard
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(removed)
}

/// Sweep a bounded batch of unreachable immutable checker revisions older than
/// the safety grace. Called only by singleton deployment maintenance.
pub async fn collect_stale_checker_revisions(state: &SharedState) -> AppResult<u64> {
    collect_stale_checker_revisions_at(
        state.pg(),
        Path::new(&state.config.storage_root),
        CHECKER_GC_GRACE,
        CHECKER_GC_BATCH,
    )
    .await
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

        pool.close().await;
        tokio::fs::remove_dir_all(&root).await.unwrap();
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
