//! Durable, replayable blob staging outside domain transactions.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::*;

mod identity;
mod owners;
pub(crate) use identity::scoped_operation_id;

const STORE_DEADLINE: Duration = Duration::from_secs(45);
const LOCAL_STORE_JOBS: usize = 4;
const DEPLOYMENT_STORE_JOBS: i64 = 32;
const DEPLOYMENT_STAGE_RECORDS: i64 = 4_096;
const DEPLOYMENT_STORE_BYTES: i64 = 1024 * 1024 * 1024;
const STAGE_CLAIM_LOCK: &str = "rsctf:blob-stage-admission";

static STORE_ADMISSION: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(LOCAL_STORE_JOBS)));

#[derive(Clone, Debug)]
pub(crate) struct StagedBlob {
    pub operation_id: Uuid,
    pub owner_scope: String,
    pub owner_user_id: Option<Uuid>,
    pub blob: StoredBlob,
}

impl StagedBlob {
    pub(crate) async fn consume_with_existing_reference(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> AppResult<i32> {
        consume_staged_blob_with_existing_reference(transaction, self).await
    }

    pub(crate) async fn consume_with_existing_reference_as(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        publication_scope: &str,
    ) -> AppResult<i32> {
        consume_staged_blob_with_existing_reference_as(transaction, self, publication_scope).await
    }
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
    published_owner_scope: Option<String>,
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
    let mut tx = match tokio::time::timeout(
        Duration::from_millis(250),
        crate::utils::database::begin_sqlx_transaction(pool),
    )
    .await
    {
        Ok(result) => result.map_err(database_error)?,
        Err(_) => return Err(AppError::overloaded("Blob staging admission is busy", 1)),
    };
    let admitted =
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(STAGE_CLAIM_LOCK)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
    if !admitted {
        tx.rollback().await.map_err(database_error)?;
        return Err(AppError::overloaded("Blob staging admission is busy", 1));
    }
    let hash_admitted =
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(hash)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
    if !hash_admitted {
        tx.rollback().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "This blob is being published; retry in a moment",
            1,
        ));
    }
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
                  state, lease_expires_at_utc, published_owner_scope
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
                if row.lease_expires_at_utc <= Utc::now() {
                    let retention = if row.state == "Published" {
                        "24 hours"
                    } else {
                        "15 minutes"
                    };
                    sqlx::query(
                        r#"UPDATE "BlobStagingOperations"
                              SET lease_expires_at_utc = clock_timestamp()
                                  + $2::interval
                            WHERE operation_id = $1
                              AND state IN ('Ready', 'Published')"#,
                    )
                    .bind(operation_id)
                    .bind(retention)
                    .execute(&mut *tx)
                    .await
                    .map_err(database_error)?;
                }
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
            "Failed" if row.lease_expires_at_utc > Utc::now() => {
                tx.commit().await.map_err(database_error)?;
                return Err(AppError::overloaded(
                    "The same blob operation is being reclaimed",
                    2,
                ));
            }
            "Storing" | "Failed" => {
                let (active, active_bytes): (i64, i64) = sqlx::query_as(
                    r#"SELECT COUNT(*) FILTER (WHERE state = 'Storing')::bigint,
                              COALESCE(SUM(file_size) FILTER (
                                  WHERE state IN ('Storing', 'Ready')
                              ), 0)::bigint
                         FROM "BlobStagingOperations"
                        WHERE lease_expires_at_utc > clock_timestamp()"#,
                )
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
                if active >= DEPLOYMENT_STORE_JOBS
                    || active_bytes.saturating_add(size) > DEPLOYMENT_STORE_BYTES
                {
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

    let (active, stage_records, active_bytes): (i64, i64, i64) = sqlx::query_as(
        r#"SELECT COUNT(*) FILTER (WHERE state = 'Storing')::bigint,
                  COUNT(*)::bigint,
                  COALESCE(SUM(file_size) FILTER (
                      WHERE state IN ('Storing', 'Ready')
                  ), 0)::bigint
             FROM "BlobStagingOperations"
            WHERE lease_expires_at_utc > clock_timestamp()"#,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    if active >= DEPLOYMENT_STORE_JOBS
        || stage_records >= DEPLOYMENT_STAGE_RECORDS
        || active_bytes.saturating_add(size) > DEPLOYMENT_STORE_BYTES
    {
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
                  state, lease_expires_at_utc, published_owner_scope
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
        || !matches!(row.state.as_str(), "Ready" | "Published")
        || row.lease_expires_at_utc <= Utc::now()
    {
        return Err(AppError::conflict(
            "Upload operation does not own this staged asset",
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
    publish_staged_blob_for_owner(transaction, staged, &staged.owner_scope).await
}

/// Publish one staged reference for exactly one durable domain owner. A
/// `Published` receipt may be replayed only by that same owner; it cannot be
/// reused to create a second attachment without a second logical reference.
pub(crate) async fn publish_staged_blob_for_owner(
    transaction: &mut Transaction<'_, Postgres>,
    staged: &StagedBlob,
    publication_scope: &str,
) -> AppResult<i32> {
    validate_publication_scope(publication_scope)?;
    // All publication/deletion paths take the content fence before operation
    // rows. This prevents a replay from deadlocking an owner swap that already
    // fenced both its old and new hashes in canonical order.
    lock_hash(transaction, &staged.blob.hash)
        .await
        .map_err(database_error)?;
    let row = sqlx::query_as::<_, StageRow>(
        r#"SELECT owner_scope, owner_user_id, content_hash, file_name, file_size,
                  state, lease_expires_at_utc, published_owner_scope
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
        if row.published_owner_scope.as_deref() != Some(publication_scope) {
            return Err(AppError::conflict(
                "Blob upload receipt was already consumed by another owner",
            ));
        }
        let file_id = sqlx::query_scalar::<_, i32>(
            r#"SELECT id FROM "Files" WHERE hash = $1 AND reference_count > 0"#,
        )
        .bind(&staged.blob.hash)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::conflict("published blob is no longer owned"))?;
        if !owners::published_owner_still_matches(transaction, staged, publication_scope, file_id)
            .await?
        {
            return Err(AppError::conflict(
                "Blob upload receipt owner changed after publication",
            ));
        }
        return Ok(file_id);
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
                  published_owner_scope = $2,
                  lease_expires_at_utc = clock_timestamp() + interval '24 hours'
            WHERE operation_id = $1 AND state = 'Ready'
              AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .bind(staged.operation_id)
    .bind(publication_scope)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("Blob staging operation is not ready"));
    }
    Ok(file_id)
}

/// Consume a stage without acquiring another reference when the caller has
/// already proved that this exact content is its current domain-owned blob.
/// This closes the commit-acknowledgement gap: an exact retry can publish a
/// still-ready stage or replay a published receipt without incrementing and
/// subsequently releasing the owner's only reference.
pub(crate) async fn consume_staged_blob_with_existing_reference(
    transaction: &mut Transaction<'_, Postgres>,
    staged: &StagedBlob,
) -> AppResult<i32> {
    consume_staged_blob_with_existing_reference_as(transaction, staged, &staged.owner_scope).await
}

pub(crate) async fn consume_staged_blob_with_existing_reference_as(
    transaction: &mut Transaction<'_, Postgres>,
    staged: &StagedBlob,
    publication_scope: &str,
) -> AppResult<i32> {
    validate_publication_scope(publication_scope)?;
    lock_hash(transaction, &staged.blob.hash)
        .await
        .map_err(database_error)?;
    let row = sqlx::query_as::<_, StageRow>(
        r#"SELECT owner_scope, owner_user_id, content_hash, file_name, file_size,
                  state, lease_expires_at_utc, published_owner_scope
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
    if !matches!(row.state.as_str(), "Ready" | "Published")
        || row.lease_expires_at_utc <= Utc::now()
    {
        return Err(AppError::conflict("Blob staging operation is not ready"));
    }

    let file_id = sqlx::query_scalar::<_, i32>(
        r#"SELECT id FROM "Files" WHERE hash = $1 AND reference_count > 0"#,
    )
    .bind(&staged.blob.hash)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("current blob is no longer owned"))?;
    if row.state == "Published" && row.published_owner_scope.as_deref() != Some(publication_scope) {
        return Err(AppError::conflict(
            "Blob upload receipt was already consumed by another owner",
        ));
    }
    if row.state == "Ready" {
        let updated = sqlx::query(
            r#"UPDATE "BlobStagingOperations"
                  SET state = 'Published', published_at_utc = clock_timestamp(),
                      published_owner_scope = $2,
                      lease_expires_at_utc = clock_timestamp() + interval '24 hours'
                WHERE operation_id = $1 AND state = 'Ready'
                  AND lease_expires_at_utc > clock_timestamp()"#,
        )
        .bind(staged.operation_id)
        .bind(publication_scope)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        if updated.rows_affected() != 1 {
            return Err(AppError::conflict("Blob staging operation is not ready"));
        }
    }
    Ok(file_id)
}

fn validate_publication_scope(publication_scope: &str) -> AppResult<()> {
    if publication_scope.is_empty() || publication_scope.len() > 255 {
        return Err(AppError::bad_request("Invalid blob publication owner"));
    }
    Ok(())
}

enum StageCleanupClaim {
    ReceiptRemoved,
    Unpublished { token: String },
}

struct StageCleanupResult {
    finalized: bool,
    purged: bool,
}

/// Fence one stage before touching object storage. An unpublished row remains
/// as a durable, non-publishable cleanup claim until deletion is acknowledged.
async fn claim_stage_cleanup(
    pool: &PgPool,
    operation_id: Uuid,
    hash: &str,
    exact_ready: Option<&StagedBlob>,
) -> AppResult<Option<StageCleanupClaim>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let row = sqlx::query_as::<_, StageRow>(
        r#"SELECT owner_scope, owner_user_id, content_hash, file_name, file_size,
                  state, lease_expires_at_utc, published_owner_scope
             FROM "BlobStagingOperations"
            WHERE operation_id = $1 AND content_hash = $2
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .bind(hash)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    let Some(row) = row else {
        transaction.commit().await.map_err(database_error)?;
        return Ok(None);
    };

    if let Some(staged) = exact_ready {
        if row.state != "Ready"
            || row.owner_scope != staged.owner_scope
            || row.owner_user_id != staged.owner_user_id
            || row.content_hash != staged.blob.hash
            || row.file_name != staged.blob.name
            || row.file_size != staged.blob.size
        {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        }
    }

    if row.state == "Published" {
        if exact_ready.is_some() {
            transaction.commit().await.map_err(database_error)?;
            return Ok(None);
        }
        let removed = sqlx::query(
            r#"DELETE FROM "BlobStagingOperations"
                WHERE operation_id = $1 AND content_hash = $2
                  AND state = 'Published'
                  AND lease_expires_at_utc <= clock_timestamp()"#,
        )
        .bind(operation_id)
        .bind(hash)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        return Ok((removed.rows_affected() == 1).then_some(StageCleanupClaim::ReceiptRemoved));
    }
    if !matches!(row.state.as_str(), "Storing" | "Ready" | "Failed") {
        return Err(AppError::internal("invalid blob staging state"));
    }

    let token = format!("stage-cleanup:{}", Uuid::new_v4());
    let claimed = sqlx::query(
        r#"UPDATE "BlobStagingOperations"
              SET state = 'Failed',
                  lease_expires_at_utc = clock_timestamp() + interval '2 minutes',
                  last_error = $3
            WHERE operation_id = $1 AND content_hash = $2
              AND state = $4
              AND ($5 = FALSE OR lease_expires_at_utc <= clock_timestamp())"#,
    )
    .bind(operation_id)
    .bind(hash)
    .bind(&token)
    .bind(&row.state)
    .bind(exact_ready.is_none())
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok((claimed.rows_affected() == 1).then_some(StageCleanupClaim::Unpublished { token }))
}

async fn delete_stage_cleanup_claim(
    pool: &PgPool,
    operation_id: Uuid,
    hash: &str,
    token: &str,
) -> AppResult<bool> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let removed = sqlx::query(
        r#"DELETE FROM "BlobStagingOperations"
            WHERE operation_id = $1 AND content_hash = $2
              AND state = 'Failed' AND last_error = $3"#,
    )
    .bind(operation_id)
    .bind(hash)
    .bind(token)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(removed.rows_affected() == 1)
}

async fn defer_stage_cleanup_claim(
    pool: &PgPool,
    operation_id: Uuid,
    hash: &str,
    token: &str,
    error: &str,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "BlobStagingOperations"
              SET lease_expires_at_utc = clock_timestamp() + interval '30 seconds',
                  last_error = left($4, 1000)
            WHERE operation_id = $1 AND content_hash = $2
              AND state = 'Failed' AND last_error = $3"#,
    )
    .bind(operation_id)
    .bind(hash)
    .bind(token)
    .bind(error)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(())
}

async fn finish_stage_cleanup_without_purge(
    pool: &PgPool,
    operation_id: Uuid,
    hash: &str,
    token: &str,
) -> AppResult<bool> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let owns_claim: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "BlobStagingOperations"
                WHERE operation_id = $1 AND content_hash = $2
                  AND state = 'Failed' AND last_error = $3
           )"#,
    )
    .bind(operation_id)
    .bind(hash)
    .bind(token)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if !owns_claim {
        transaction.commit().await.map_err(database_error)?;
        return Ok(false);
    }

    let finalized = if hash_is_referenced(&mut transaction, hash).await? {
        sqlx::query(
            r#"DELETE FROM "BlobStagingOperations"
                WHERE operation_id = $1 AND content_hash = $2
                  AND state = 'Failed' AND last_error = $3"#,
        )
        .bind(operation_id)
        .bind(hash)
        .bind(token)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        true
    } else {
        sqlx::query(
            r#"UPDATE "BlobStagingOperations"
                  SET lease_expires_at_utc = clock_timestamp() + interval '30 seconds',
                      last_error = 'blob cleanup deferred while deletion is active'
                WHERE operation_id = $1 AND content_hash = $2
                  AND state = 'Failed' AND last_error = $3"#,
        )
        .bind(operation_id)
        .bind(hash)
        .bind(token)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        false
    };
    transaction.commit().await.map_err(database_error)?;
    Ok(finalized)
}

async fn run_stage_cleanup(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    operation_id: Uuid,
    hash: &str,
    token: &str,
) -> AppResult<StageCleanupResult> {
    match purge_if_unreferenced(pool, storage, hash).await {
        Ok(true) => Ok(StageCleanupResult {
            finalized: delete_stage_cleanup_claim(pool, operation_id, hash, token).await?,
            purged: true,
        }),
        Ok(false) => Ok(StageCleanupResult {
            finalized: finish_stage_cleanup_without_purge(pool, operation_id, hash, token).await?,
            purged: false,
        }),
        Err(error) => {
            if let Err(persist_error) =
                defer_stage_cleanup_claim(pool, operation_id, hash, token, &error.to_string()).await
            {
                tracing::warn!(
                    %operation_id,
                    %hash,
                    %persist_error,
                    "failed to persist deferred stage cleanup"
                );
            }
            Err(error)
        }
    }
}

/// Discard a one-shot stage after its owner transaction definitely failed.
/// A concurrently published receipt is preserved; only this exact still-ready
/// operation is claimed before the usual fresh reachability-checked purge.
pub(crate) async fn discard_unpublished_stage(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    staged: &StagedBlob,
) -> AppResult<bool> {
    let claim =
        claim_stage_cleanup(pool, staged.operation_id, &staged.blob.hash, Some(staged)).await?;
    let Some(StageCleanupClaim::Unpublished { token }) = claim else {
        return Ok(false);
    };
    Ok(run_stage_cleanup(
        pool,
        storage,
        staged.operation_id,
        &staged.blob.hash,
        &token,
    )
    .await?
    .purged)
}

/// Reclaim a bounded batch. Published result receipts expire without touching
/// content; abandoned stages delete only after a fresh owner/reference check.
pub(crate) async fn purge_expired_stages(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    limit: i64,
) -> AppResult<u64> {
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        r#"SELECT operation_id, content_hash
             FROM "BlobStagingOperations"
            WHERE lease_expires_at_utc <= clock_timestamp()
            ORDER BY lease_expires_at_utc, operation_id
            LIMIT $1"#,
    )
    .bind(limit.clamp(1, 128))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let mut reclaimed = 0_u64;
    for (operation_id, hash) in rows {
        match claim_stage_cleanup(pool, operation_id, &hash, None).await {
            Ok(Some(StageCleanupClaim::ReceiptRemoved)) => reclaimed += 1,
            Ok(Some(StageCleanupClaim::Unpublished { token })) => {
                match run_stage_cleanup(pool, storage, operation_id, &hash, &token).await {
                    Ok(result) if result.finalized => reclaimed += 1,
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            %operation_id,
                            %hash,
                            %error,
                            "expired blob stage cleanup deferred"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    %operation_id,
                    %hash,
                    %error,
                    "failed to claim expired blob stage cleanup"
                );
            }
        }
    }
    Ok(reclaimed)
}

#[cfg(test)]
mod tests;
