//! Durable, replayable blob staging outside domain transactions.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::*;

const STORE_DEADLINE: Duration = Duration::from_secs(45);
const LOCAL_STORE_JOBS: usize = 4;
const DEPLOYMENT_STORE_JOBS: i64 = 32;
const STAGE_CLAIM_LOCK: &str = "rsctf:blob-stage-admission";

static STORE_ADMISSION: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(LOCAL_STORE_JOBS)));

/// Derive an endpoint-scoped blob operation from a caller's idempotency key.
/// This keeps the staging table globally unique without making operation IDs
/// from unrelated endpoints conflict with each other.
pub(crate) fn scoped_operation_id(root: Uuid, scope: &str, ordinal: u64) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(scope.as_bytes());
    digest.update(root.as_bytes());
    digest.update(ordinal.to_be_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[derive(Clone, Debug)]
pub(crate) struct StagedBlob {
    pub operation_id: Uuid,
    pub owner_scope: String,
    pub owner_user_id: Option<Uuid>,
    pub blob: StoredBlob,
}

#[derive(sqlx::FromRow)]
struct StageRow {
    owner_scope: String,
    owner_user_id: Option<Uuid>,
    content_hash: String,
    file_name: String,
    file_size: i64,
    state: String,
    lease_expires_at_utc: DateTime<Utc>,
}

enum Claim {
    Store,
    Recovered(StagedBlob),
}

fn exact_stage(
    operation_id: Uuid,
    row: &StageRow,
    owner_scope: &str,
    owner_user_id: Option<Uuid>,
    hash: &str,
    name: &str,
    size: i64,
) -> AppResult<StagedBlob> {
    if row.owner_scope != owner_scope
        || row.owner_user_id != owner_user_id
        || row.content_hash != hash
        || row.file_name != name
        || row.file_size != size
    {
        return Err(AppError::conflict(
            "Blob operation identity was reused for different content",
        ));
    }
    Ok(StagedBlob {
        operation_id,
        owner_scope: row.owner_scope.clone(),
        owner_user_id: row.owner_user_id,
        blob: StoredBlob {
            hash: row.content_hash.clone(),
            size: row.file_size,
            name: row.file_name.clone(),
        },
    })
}

async fn claim_stage(
    pool: &PgPool,
    operation_id: Uuid,
    owner_scope: &str,
    owner_user_id: Option<Uuid>,
    hash: &str,
    name: &str,
    size: i64,
) -> AppResult<Claim> {
    let mut tx = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(STAGE_CLAIM_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    lock_hash(&mut tx, hash).await.map_err(database_error)?;
    let deletion_active: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "BlobDeletionOperations"
                WHERE content_hash = $1 AND state = 'Deleting'
                  AND lease_expires_at_utc > clock_timestamp()
           )"#,
    )
    .bind(hash)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    if deletion_active {
        tx.commit().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "This blob is being reclaimed; retry in a moment",
            2,
        ));
    }

    let existing = sqlx::query_as::<_, StageRow>(
        r#"SELECT owner_scope, owner_user_id, content_hash, file_name, file_size,
                  state, lease_expires_at_utc
             FROM "BlobStagingOperations"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    if let Some(row) = existing {
        let staged = exact_stage(
            operation_id,
            &row,
            owner_scope,
            owner_user_id,
            hash,
            name,
            size,
        )?;
        match row.state.as_str() {
            "Ready" | "Published" => {
                tx.commit().await.map_err(database_error)?;
                return Ok(Claim::Recovered(staged));
            }
            "Storing" if row.lease_expires_at_utc > Utc::now() => {
                tx.commit().await.map_err(database_error)?;
                return Err(AppError::overloaded(
                    "The same blob operation is still running",
                    2,
                ));
            }
            "Storing" | "Failed" => {
                let active: i64 = sqlx::query_scalar(
                    r#"SELECT COUNT(*)::bigint
                         FROM "BlobStagingOperations"
                        WHERE state = 'Storing'
                          AND lease_expires_at_utc > clock_timestamp()"#,
                )
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
                if active >= DEPLOYMENT_STORE_JOBS {
                    tx.commit().await.map_err(database_error)?;
                    return Err(AppError::overloaded(
                        "Blob storage capacity is busy; retry in a moment",
                        2,
                    ));
                }
                sqlx::query(
                    r#"UPDATE "BlobStagingOperations"
                          SET state = 'Storing',
                              lease_expires_at_utc = clock_timestamp() + interval '15 minutes',
                              last_error = NULL
                        WHERE operation_id = $1"#,
                )
                .bind(operation_id)
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
                tx.commit().await.map_err(database_error)?;
                return Ok(Claim::Store);
            }
            _ => return Err(AppError::internal("invalid blob staging state")),
        }
    }

    let active: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
             FROM "BlobStagingOperations"
            WHERE state = 'Storing' AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    if active >= DEPLOYMENT_STORE_JOBS {
        tx.commit().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "Blob storage capacity is busy; retry in a moment",
            2,
        ));
    }
    sqlx::query(
        r#"INSERT INTO "BlobStagingOperations"
               (operation_id, owner_scope, owner_user_id, content_hash,
                file_name, file_size, state, lease_expires_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, 'Storing',
                   clock_timestamp() + interval '15 minutes')
           ON CONFLICT (operation_id) DO NOTHING"#,
    )
    .bind(operation_id)
    .bind(owner_scope)
    .bind(owner_user_id)
    .bind(hash)
    .bind(name)
    .bind(size)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(Claim::Store)
}

async fn mark_failed(pool: &PgPool, operation_id: Uuid, error: &str) {
    if let Err(update_error) = sqlx::query(
        r#"UPDATE "BlobStagingOperations"
              SET state = 'Failed', lease_expires_at_utc = clock_timestamp(),
                  last_error = left($2, 1000)
            WHERE operation_id = $1 AND state = 'Storing'"#,
    )
    .bind(operation_id)
    .bind(error)
    .execute(pool)
    .await
    {
        tracing::warn!(%operation_id, %update_error, "failed to persist blob staging failure");
    }
}

/// Persist intent, perform immutable storage without a DB/domain lock, then
/// publish a recoverable ready stage. Exact operation replays never store twice.
pub(crate) async fn stage_blob(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    operation_id: Uuid,
    owner_scope: &str,
    owner_user_id: Option<Uuid>,
    name: &str,
    bytes: &[u8],
) -> AppResult<StagedBlob> {
    let size =
        i64::try_from(bytes.len()).map_err(|_| AppError::payload_too_large("Blob is too large"))?;
    if size <= 0 {
        return Err(AppError::bad_request("File is empty"));
    }
    let hash = sha256_hex(bytes);
    match claim_stage(
        pool,
        operation_id,
        owner_scope,
        owner_user_id,
        &hash,
        name,
        size,
    )
    .await?
    {
        Claim::Recovered(staged) => return Ok(staged),
        Claim::Store => {}
    }

    let permit = match STORE_ADMISSION.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            mark_failed(pool, operation_id, "Local blob storage capacity is busy").await;
            return Err(AppError::overloaded(
                "Local blob storage capacity is busy",
                2,
            ));
        }
    };
    let stored = tokio::time::timeout(STORE_DEADLINE, storage.store(name, bytes)).await;
    drop(permit);
    let blob = match stored {
        Ok(Ok(blob)) if blob.hash == hash && blob.size == size => blob,
        Ok(Ok(_)) => {
            let message = "blob storage returned metadata that does not match its content";
            mark_failed(pool, operation_id, message).await;
            return Err(AppError::internal(message));
        }
        Ok(Err(error)) => {
            mark_failed(pool, operation_id, &error.to_string()).await;
            return Err(error);
        }
        Err(_) => {
            let message = "Blob storage timed out";
            mark_failed(pool, operation_id, message).await;
            return Err(AppError::overloaded(message, 2));
        }
    };

    let mut ready_tx = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut ready_tx, &hash)
        .await
        .map_err(database_error)?;
    let deletion_active: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "BlobDeletionOperations"
                WHERE content_hash = $1 AND state = 'Deleting'
                  AND lease_expires_at_utc > clock_timestamp()
           )"#,
    )
    .bind(&hash)
    .fetch_one(&mut *ready_tx)
    .await
    .map_err(database_error)?;
    if deletion_active {
        ready_tx.rollback().await.map_err(database_error)?;
        mark_failed(pool, operation_id, "Blob deletion won the publication race").await;
        return Err(AppError::overloaded(
            "This blob is being reclaimed; retry in a moment",
            2,
        ));
    }
    sqlx::query(
        r#"INSERT INTO "Files" (hash, upload_time_utc, file_size, name, reference_count)
           VALUES ($1, clock_timestamp(), $2, $3, 0)
           ON CONFLICT (hash) DO UPDATE
              SET file_size = EXCLUDED.file_size,
                  name = EXCLUDED.name"#,
    )
    .bind(&hash)
    .bind(size)
    .bind(name)
    .execute(&mut *ready_tx)
    .await
    .map_err(database_error)?;
    let updated = sqlx::query(
        r#"UPDATE "BlobStagingOperations"
              SET state = 'Ready', lease_expires_at_utc = clock_timestamp() + interval '15 minutes'
            WHERE operation_id = $1 AND state = 'Storing' AND content_hash = $2"#,
    )
    .bind(operation_id)
    .bind(&hash)
    .execute(&mut *ready_tx)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("Blob staging ownership changed"));
    }
    ready_tx.commit().await.map_err(database_error)?;
    Ok(StagedBlob {
        operation_id,
        owner_scope: owner_scope.to_string(),
        owner_user_id,
        blob,
    })
}

pub(crate) async fn load_ready_upload_stage(
    pool: &PgPool,
    operation_id: Uuid,
    owner_user_id: Uuid,
    expected_hash: &str,
) -> AppResult<StagedBlob> {
    let row = sqlx::query_as::<_, StageRow>(
        r#"SELECT owner_scope, owner_user_id, content_hash, file_name, file_size,
                  state, lease_expires_at_utc
             FROM "BlobStagingOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("Upload operation expired"))?;
    if row.owner_user_id != Some(owner_user_id)
        || !row.owner_scope.starts_with("asset-upload:")
        || row.content_hash != expected_hash
        || row.state != "Ready"
        || row.lease_expires_at_utc <= Utc::now()
    {
        return Err(AppError::conflict(
            "Upload operation does not own this ready asset",
        ));
    }
    exact_stage(
        operation_id,
        &row,
        &row.owner_scope,
        Some(owner_user_id),
        expected_hash,
        &row.file_name,
        row.file_size,
    )
}

/// Acquire exactly one logical reference and consume a ready stage in the
/// caller's short domain transaction. A published replay returns the same file
/// id without incrementing its reference count again.
pub(crate) async fn publish_staged_blob(
    transaction: &mut Transaction<'_, Postgres>,
    staged: &StagedBlob,
) -> AppResult<i32> {
    // All publication/deletion paths take the content fence before operation
    // rows. This prevents a replay from deadlocking an owner swap that already
    // fenced both its old and new hashes in canonical order.
    lock_hash(transaction, &staged.blob.hash)
        .await
        .map_err(database_error)?;
    let row = sqlx::query_as::<_, StageRow>(
        r#"SELECT owner_scope, owner_user_id, content_hash, file_name, file_size,
                  state, lease_expires_at_utc
             FROM "BlobStagingOperations"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(staged.operation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("Blob staging operation expired"))?;
    exact_stage(
        staged.operation_id,
        &row,
        &staged.owner_scope,
        staged.owner_user_id,
        &staged.blob.hash,
        &staged.blob.name,
        staged.blob.size,
    )?;
    if row.state == "Published" {
        return sqlx::query_scalar::<_, i32>(
            r#"SELECT id FROM "Files" WHERE hash = $1 AND reference_count > 0"#,
        )
        .bind(&staged.blob.hash)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::conflict("published blob is no longer owned"));
    }
    if row.state != "Ready" || row.lease_expires_at_utc <= Utc::now() {
        return Err(AppError::conflict("Blob staging operation is not ready"));
    }
    let file_id = acquire_locked(
        transaction,
        &staged.blob.hash,
        &staged.blob.name,
        staged.blob.size,
    )
    .await
    .map_err(database_error)?;
    let updated = sqlx::query(
        r#"UPDATE "BlobStagingOperations"
              SET state = 'Published', published_at_utc = clock_timestamp(),
                  lease_expires_at_utc = clock_timestamp() + interval '24 hours'
            WHERE operation_id = $1 AND state = 'Ready'
              AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .bind(staged.operation_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("Blob staging operation is not ready"));
    }
    Ok(file_id)
}

/// Reclaim a bounded batch. Published result receipts expire without touching
/// content; abandoned stages delete only after a fresh owner/reference check.
pub(crate) async fn purge_expired_stages(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    limit: i64,
) -> AppResult<u64> {
    let rows = sqlx::query_as::<_, (String, String)>(
        r#"DELETE FROM "BlobStagingOperations"
            WHERE operation_id IN (
                SELECT operation_id
                  FROM "BlobStagingOperations"
                 WHERE lease_expires_at_utc <= clock_timestamp()
                 ORDER BY lease_expires_at_utc, operation_id
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED
            )
            RETURNING content_hash, state"#,
    )
    .bind(limit.clamp(1, 128))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let mut reclaimed = 0_u64;
    for (hash, state) in rows {
        if state != "Published" {
            let another_stage: bool = sqlx::query_scalar(
                r#"SELECT EXISTS(
                       SELECT 1 FROM "BlobStagingOperations"
                        WHERE content_hash = $1 AND state <> 'Published'
                          AND lease_expires_at_utc > clock_timestamp()
                   )"#,
            )
            .bind(&hash)
            .fetch_one(pool)
            .await
            .map_err(database_error)?;
            if !another_stage {
                let _ = purge_if_unreferenced(pool, storage, &hash).await?;
            }
        }
        reclaimed += 1;
    }
    Ok(reclaimed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::blob_refs::test_support::CoordinatedStorage;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::atomic::Ordering;

    #[test]
    fn staging_limits_are_finite() {
        assert!(LOCAL_STORE_JOBS > 0);
        assert!(DEPLOYMENT_STORE_JOBS > LOCAL_STORE_JOBS as i64);
        assert!(STORE_DEADLINE <= Duration::from_secs(60));
    }

    #[test]
    fn scoped_operations_are_stable_and_isolated() {
        let root = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        assert_eq!(
            scoped_operation_id(root, "asset-upload", 0),
            scoped_operation_id(root, "asset-upload", 0)
        );
        assert_ne!(
            scoped_operation_id(root, "asset-upload", 0),
            scoped_operation_id(root, "asset-upload", 1)
        );
        assert_ne!(
            scoped_operation_id(root, "asset-upload", 0),
            scoped_operation_id(root, "challenge-import", 0)
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn exact_replay_stores_once_and_publishes_one_reference() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("blob_stage_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let search_path = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .after_connect(move |connection, _| {
                let statement = format!(r#"SET search_path TO "{search_path}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "Files" (
                   id SERIAL PRIMARY KEY, hash VARCHAR(64) NOT NULL UNIQUE,
                   upload_time_utc TIMESTAMPTZ NOT NULL, file_size BIGINT NOT NULL,
                   name TEXT NOT NULL, reference_count BIGINT NOT NULL
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::services::blob_refs::test_support::install_operation_tables(&pool).await;

        let storage = CoordinatedStorage::default();
        let owner = Uuid::new_v4();
        let operation = Uuid::new_v4();
        let first = stage_blob(
            &pool,
            &storage,
            operation,
            "asset-upload:test:0",
            Some(owner),
            "proof.bin",
            b"immutable",
        )
        .await
        .unwrap();
        let replay = stage_blob(
            &pool,
            &storage,
            operation,
            "asset-upload:test:0",
            Some(owner),
            "proof.bin",
            b"immutable",
        )
        .await
        .unwrap();
        assert_eq!(first.blob.hash, replay.blob.hash);
        assert_eq!(storage.stores.load(Ordering::SeqCst), 1);

        let mut first_publish = pool.begin().await.unwrap();
        let first_id = publish_staged_blob(&mut first_publish, &first)
            .await
            .unwrap();
        first_publish.commit().await.unwrap();
        let mut replay_publish = pool.begin().await.unwrap();
        let replay_id = publish_staged_blob(&mut replay_publish, &replay)
            .await
            .unwrap();
        replay_publish.commit().await.unwrap();
        assert_eq!(first_id, replay_id);
        let references: i64 =
            sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE id = $1"#)
                .bind(first_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(references, 1);

        sqlx::query(r#"UPDATE "Files" SET reference_count = 0 WHERE id = $1"#)
            .bind(first_id)
            .execute(&pool)
            .await
            .unwrap();
        let mut expired_owner_replay = pool.begin().await.unwrap();
        assert!(
            publish_staged_blob(&mut expired_owner_replay, &replay)
                .await
                .is_err(),
            "a publication receipt must not resurrect a released physical blob"
        );
        expired_owner_replay.rollback().await.unwrap();

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
