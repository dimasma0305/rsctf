//! One-time bounded reconciliation of legacy capture trees into PostgreSQL.

use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use sqlx::PgPool;

use crate::utils::error::{AppError, AppResult};
use crate::utils::single_flight::SingleFlight;

use super::{commit, database_error, locked_transaction, upsert_files_in, InventoryFile};

const MAX_RECONCILE_CHALLENGES: usize = 10_000;
const MAX_RECONCILE_BUCKETS: usize = 100_000;
const MAX_RECONCILE_FILES: usize = 100_000;
const MAX_RECONCILE_FILES_PER_BUCKET: usize = 4_096;
const RECONCILE_INSERT_BATCH: usize = 1_000;
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(120);

static RECONCILE_FLIGHT: LazyLock<SingleFlight<bool>> = LazyLock::new(SingleFlight::new);
static RECONCILE_SLOT: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(1)));

pub(super) async fn ensure_reconciled(pool: &PgPool, capture_root: &Path) -> AppResult<()> {
    if inventory_is_reconciled(pool).await? {
        return Ok(());
    }

    let pool = pool.clone();
    let capture_root = capture_root.to_path_buf();
    let succeeded = RECONCILE_FLIGHT
        .run_with_timeout(
            "traffic-capture-inventory",
            RECONCILE_TIMEOUT,
            move || async move {
                match reconcile_once(&pool, &capture_root).await {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(%error, "traffic capture inventory reconciliation failed");
                        false
                    }
                }
            },
        )
        .await;
    if succeeded {
        Ok(())
    } else {
        Err(AppError::unavailable(
            "Capture inventory is being reconciled; retry shortly",
        ))
    }
}

async fn inventory_is_reconciled(pool: &PgPool) -> AppResult<bool> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT reconciled_at_utc IS NOT NULL
             FROM "TrafficCaptureInventoryState"
            WHERE singleton = TRUE"#,
    )
    .fetch_optional(pool)
    .await
    .map_err(database_error)
    .map(|value| value.unwrap_or(false))
}

pub(super) async fn reconcile_once(pool: &PgPool, capture_root: &Path) -> AppResult<()> {
    let listing_permit =
        RECONCILE_SLOT.clone().acquire_owned().await.map_err(|_| {
            AppError::unavailable("Capture inventory reconciliation is shutting down")
        })?;
    // The transaction-scoped advisory lock is shared with every inventory
    // mutation. A writer that changes the filesystem while this scan runs will
    // publish its metadata only after the replacement snapshot commits.
    let mut transaction = locked_transaction(pool).await?;
    let already_done = sqlx::query_scalar::<_, bool>(
        r#"SELECT reconciled_at_utc IS NOT NULL
             FROM "TrafficCaptureInventoryState"
            WHERE singleton = TRUE"#,
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .unwrap_or(false);
    if already_done {
        return commit(transaction).await;
    }

    let root = capture_root.to_path_buf();
    let files = spawn_blocking_with_permit(listing_permit, move || scan_capture_tree(&root))
        .await
        .map_err(|error| AppError::internal(format!("capture inventory scan failed: {error}")))??;

    sqlx::query(r#"DELETE FROM "TrafficCaptureFiles""#)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    sqlx::query(r#"DELETE FROM "TrafficCaptureBuckets""#)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    for batch in files.chunks(RECONCILE_INSERT_BATCH) {
        upsert_files_in(&mut transaction, batch).await?;
    }
    sqlx::query(
        r#"UPDATE "TrafficCaptureInventoryState"
              SET reconciled_at_utc = clock_timestamp(),
                  updated_at_utc = clock_timestamp()
            WHERE singleton = TRUE"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    commit(transaction).await
}

pub(super) async fn spawn_blocking_with_permit<T, F>(
    permit: tokio::sync::OwnedSemaphorePermit,
    work: F,
) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
}

fn scan_capture_tree(root: &Path) -> AppResult<Vec<InventoryFile>> {
    let challenge_entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AppError::internal(format!(
                "failed to enumerate capture root {}: {error}",
                root.display()
            )))
        }
    };
    let mut challenge_count = 0usize;
    let mut bucket_count = 0usize;
    let mut files = Vec::new();
    for challenge_entry in challenge_entries {
        let challenge_entry = challenge_entry.map_err(scan_error)?;
        let challenge_path = challenge_entry.path();
        let metadata = std::fs::symlink_metadata(&challenge_path).map_err(scan_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(challenge_id) = positive_id(&challenge_entry.file_name()) else {
            continue;
        };
        challenge_count += 1;
        if challenge_count > MAX_RECONCILE_CHALLENGES {
            return Err(AppError::unavailable(
                "Capture inventory challenge scan limit was exceeded",
            ));
        }

        let participation_entries = std::fs::read_dir(&challenge_path).map_err(scan_error)?;
        for participation_entry in participation_entries {
            let participation_entry = participation_entry.map_err(scan_error)?;
            let participation_path = participation_entry.path();
            let metadata = std::fs::symlink_metadata(&participation_path).map_err(scan_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let Some(participation_id) = positive_id(&participation_entry.file_name()) else {
                continue;
            };
            bucket_count += 1;
            if bucket_count > MAX_RECONCILE_BUCKETS {
                return Err(AppError::unavailable(
                    "Capture inventory participation scan limit was exceeded",
                ));
            }
            scan_bucket(
                challenge_id,
                participation_id,
                &participation_path,
                &mut files,
            )?;
        }
    }
    Ok(files)
}

fn scan_bucket(
    challenge_id: i32,
    participation_id: i32,
    directory: &Path,
    files: &mut Vec<InventoryFile>,
) -> AppResult<()> {
    let entries = std::fs::read_dir(directory).map_err(scan_error)?;
    let mut bucket_files = 0usize;
    for entry in entries {
        let entry = entry.map_err(scan_error)?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(scan_error)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pcap"))
        {
            continue;
        }
        bucket_files += 1;
        if bucket_files > MAX_RECONCILE_FILES_PER_BUCKET {
            return Err(AppError::unavailable(
                "A capture participation exceeds the reconciliation file limit",
            ));
        }
        if files.len() >= MAX_RECONCILE_FILES {
            return Err(AppError::unavailable(
                "Capture inventory file scan limit was exceeded",
            ));
        }
        files.push(InventoryFile::from_path(
            challenge_id,
            participation_id,
            &path,
        )?);
    }
    Ok(())
}

fn positive_id(name: &std::ffi::OsStr) -> Option<i32> {
    name.to_str()?.parse::<i32>().ok().filter(|id| *id > 0)
}

fn scan_error(error: std::io::Error) -> AppError {
    AppError::internal(format!("capture inventory filesystem error: {error}"))
}

#[cfg(test)]
pub(super) fn scan_for_test(root: &Path) -> AppResult<Vec<(i32, i32, String, i64)>> {
    scan_capture_tree(root).map(|files| {
        files
            .into_iter()
            .map(|file| {
                (
                    file.challenge_id,
                    file.participation_id,
                    file.file_name,
                    file.size_bytes,
                )
            })
            .collect()
    })
}
