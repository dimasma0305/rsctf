//! Atomic metadata operations for content-addressed, ref-counted blobs.
//!
//! Physical store/delete work is claimed durably, then performed without an
//! open PostgreSQL transaction. Short metadata transactions share a
//! content-hash advisory fence and revalidate the durable lease before publish
//! or cleanup, keeping replicas ordered without holding pool connections over
//! object-store I/O.

use std::sync::Arc;
use std::time::Duration;

use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::Semaphore;

use crate::storage::{BlobStorage, StoredBlob};
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

mod ad_snapshots;
mod attachments;
mod challenges;
#[cfg(test)]
mod poster_tests;
mod seaorm;
mod staging;
#[cfg(test)]
pub(crate) mod test_support;
mod writeups;
pub use ad_snapshots::{
    available_service_snapshots, load_service_snapshot, purge_expired_service_snapshots,
    store_service_snapshot, ServiceSnapshotBlob,
};
pub(crate) use attachments::delete_attachment_locked;
pub use attachments::{
    delete_attachment, delete_orphan_attachments, store_and_replace_challenge_attachment,
};
pub use challenges::{
    delete_challenge, delete_game_challenges, store_and_replace_challenge_archive,
    DeletedChallengeArtifacts,
};
pub(crate) use challenges::{
    delete_challenge_locked, delete_game_challenges_locked, purge_deleted_challenge_artifacts,
};
pub(crate) use seaorm::publish_staged_blob_in_seaorm_transaction;
pub(crate) use staging::{
    discard_unpublished_stage, load_ready_upload_stage, publish_staged_blob,
    publish_staged_blob_for_owner, purge_expired_stages, scoped_operation_id, stage_blob,
    StagedBlob,
};
#[cfg(test)]
use writeups::replace_writeup;
pub(crate) use writeups::store_and_replace_writeup;
pub use writeups::{clear_game_writeups, ClearedWriteups};

const UPSERT_FILE_SQL: &str = r#"
    INSERT INTO "Files" (hash, upload_time_utc, file_size, name, reference_count)
    VALUES ($1, now(), $2, $3, 1)
    ON CONFLICT (hash) DO UPDATE
       SET reference_count = "Files".reference_count + 1
    RETURNING id
"#;

const DELETE_DEADLINE: Duration = Duration::from_secs(45);
const LOCAL_DELETE_JOBS: usize = 4;
const DEPLOYMENT_DELETE_JOBS: i64 = 32;
const DELETE_ADMISSION_LOCK: &str = "rsctf:blob-delete-admission";
static DELETE_ADMISSION: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(LOCAL_DELETE_JOBS)));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOutcome {
    pub found: bool,
    /// Set only when this operation released the final logical reference.
    /// A zero-reference metadata tombstone remains until the post-commit
    /// physical delete in [`purge_if_unreferenced`] succeeds.
    pub deleted_hash: Option<String>,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

async fn lock_hash(
    transaction: &mut Transaction<'_, Postgres>,
    hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(hash)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Take every content lock needed by a direct-hash owner swap in canonical
/// order. Publication and release re-enter these transaction-scoped locks, so
/// concurrent accounts/games swapping the same two hashes cannot deadlock by
/// each taking its new hash before the other's old hash.
pub(crate) async fn lock_direct_hashes_locked<'a>(
    transaction: &mut Transaction<'_, Postgres>,
    hashes: impl IntoIterator<Item = &'a str>,
) -> AppResult<()> {
    for hash in canonical_hash_order(hashes) {
        lock_hash(transaction, hash).await.map_err(database_error)?;
    }
    Ok(())
}

fn canonical_hash_order<'a>(hashes: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut hashes = hashes.into_iter().collect::<Vec<_>>();
    hashes.sort_unstable();
    hashes.dedup();
    hashes
}

async fn acquire_locked(
    transaction: &mut Transaction<'_, Postgres>,
    hash: &str,
    name: &str,
    size: i64,
) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar::<_, i32>(UPSERT_FILE_SQL)
        .bind(hash)
        .bind(size)
        .bind(name)
        .fetch_one(&mut **transaction)
        .await
}

/// Store bytes and add one logical reference under the same distributed hash
/// identity used by deletion. Storage happens under a durable stage and an
/// absolute deadline before the short reference transaction.
pub async fn store_and_acquire(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    name: &str,
    bytes: &[u8],
) -> AppResult<(StoredBlob, i32)> {
    let staged = stage_blob(
        pool,
        storage,
        uuid::Uuid::new_v4(),
        "standalone-asset",
        None,
        name,
        bytes,
    )
    .await?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let file_id = match publish_staged_blob(&mut transaction, &staged).await {
        Ok(file_id) => file_id,
        Err(error) => {
            let _ = transaction.rollback().await;
            if let Err(cleanup_error) = discard_unpublished_stage(pool, storage, &staged).await {
                tracing::warn!(%cleanup_error, hash = %staged.blob.hash, "standalone blob rollback cleanup deferred");
            }
            return Err(error);
        }
    };
    if let Err(error) = transaction.commit().await.map_err(database_error) {
        if let Err(cleanup_error) = discard_unpublished_stage(pool, storage, &staged).await {
            tracing::warn!(%cleanup_error, hash = %staged.blob.hash, "uncertain standalone blob cleanup deferred");
        }
        return Err(error);
    }
    Ok((staged.blob, file_id))
}

#[cfg(test)]
async fn acquire(pool: &PgPool, hash: &str, name: &str, size: i64) -> AppResult<i32> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let id = acquire_locked(&mut transaction, hash, name, size)
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(id)
}

async fn release_locked(
    transaction: &mut Transaction<'_, Postgres>,
    id: i32,
) -> Result<ReleaseOutcome, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, i64)>(
        r#"SELECT hash, reference_count
             FROM "Files"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some((hash, reference_count)) = row else {
        return Ok(ReleaseOutcome {
            found: false,
            deleted_hash: None,
        });
    };

    if reference_count > 1 {
        sqlx::query(
            r#"UPDATE "Files"
                  SET reference_count = reference_count - 1
                WHERE id = $1"#,
        )
        .bind(id)
        .execute(&mut **transaction)
        .await?;
        Ok(ReleaseOutcome {
            found: true,
            deleted_hash: None,
        })
    } else {
        let has_owner: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "Attachments" WHERE local_file_id = $1)
                   OR EXISTS(SELECT 1 FROM "Participations" WHERE writeup_id = $1)
                   OR EXISTS(SELECT 1 FROM "AdServiceSnapshots" WHERE local_file_id = $1)
                   OR EXISTS(SELECT 1 FROM "AspNetUsers" WHERE avatar_hash = $2)
                   OR EXISTS(SELECT 1 FROM "Teams" WHERE avatar_hash = $2)
                   OR EXISTS(SELECT 1 FROM "Games" WHERE poster_hash = $2)
                   OR EXISTS(
                        SELECT 1 FROM "Configs"
                         WHERE config_key IN (
                               'GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash'
                         )
                           AND value = $2
                   )
                   OR EXISTS(
                        SELECT 1 FROM "GameChallenges"
                         WHERE original_archive_blob_path = $2
                   )"#,
        )
        .bind(id)
        .bind(&hash)
        .fetch_one(&mut **transaction)
        .await?;
        if has_owner {
            sqlx::query(r#"UPDATE "Files" SET reference_count = 1 WHERE id = $1"#)
                .bind(id)
                .execute(&mut **transaction)
                .await?;
            return Ok(ReleaseOutcome {
                found: true,
                deleted_hash: None,
            });
        }
        // Keep a durable zero-reference tombstone until physical deletion has
        // succeeded. If this process crashes after the owning row commits but
        // before object storage is touched, singleton maintenance can retry by
        // scanning these rows. A concurrent acquire runs under the same hash
        // lock and atomically raises this back to one before recreating bytes.
        sqlx::query(r#"UPDATE "Files" SET reference_count = 0 WHERE id = $1"#)
            .bind(id)
            .execute(&mut **transaction)
            .await?;
        Ok(ReleaseOutcome {
            found: true,
            deleted_hash: Some(hash),
        })
    }
}

/// Release one reference selected by its content hash.
pub async fn release_by_hash(pool: &PgPool, hash: &str) -> AppResult<ReleaseOutcome> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let id = sqlx::query_scalar::<_, i32>(r#"SELECT id FROM "Files" WHERE hash = $1 FOR UPDATE"#)
        .bind(hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
    let outcome = match id {
        Some(id) => release_locked(&mut transaction, id)
            .await
            .map_err(database_error)?,
        None => ReleaseOutcome {
            found: false,
            deleted_hash: None,
        },
    };
    transaction.commit().await.map_err(database_error)?;
    Ok(outcome)
}

/// Release a hash reference inside a caller-owned transaction after its direct
/// owner row (for example, `Games.poster_hash`) has been detached or deleted.
/// Keeping both changes in one transaction prevents a committed owner deletion
/// from leaking its logical blob reference if the metadata update fails.
pub(crate) async fn release_direct_hash_locked(
    transaction: &mut Transaction<'_, Postgres>,
    hash: &str,
) -> AppResult<ReleaseOutcome> {
    lock_hash(transaction, hash).await.map_err(database_error)?;
    let id = sqlx::query_scalar::<_, i32>(r#"SELECT id FROM "Files" WHERE hash = $1 FOR UPDATE"#)
        .bind(hash)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?;
    match id {
        Some(id) => release_locked(transaction, id)
            .await
            .map_err(database_error),
        None => Ok(ReleaseOutcome {
            found: false,
            deleted_hash: None,
        }),
    }
}

/// Release a direct hash owner (avatar, poster, build archive, or branding)
/// and purge legacy untracked content when no durable owner remains.
pub async fn release_and_purge(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    hash: &str,
) -> AppResult<bool> {
    let outcome = release_by_hash(pool, hash).await?;
    if outcome.found && outcome.deleted_hash.is_none() {
        return Ok(false);
    }
    purge_if_unreferenced(pool, storage, hash).await
}

/// Delete physical content only when a fresh post-commit query confirms that
/// no metadata or direct-hash owner currently references it. Returns whether
/// deletion ran.
pub async fn purge_if_unreferenced(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    hash: &str,
) -> AppResult<bool> {
    let _permit = DELETE_ADMISSION
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::overloaded("Blob deletion capacity is busy", 2))?;
    let Some(operation_id) = claim_blob_deletion(pool, hash).await? else {
        return Ok(false);
    };

    let deleted = tokio::time::timeout(DELETE_DEADLINE, storage.delete(hash)).await;
    match deleted {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            mark_blob_deletion_failed(pool, hash, operation_id, &error.to_string()).await;
            return Err(error);
        }
        Err(_) => {
            let message = "Blob deletion timed out";
            mark_blob_deletion_failed(pool, hash, operation_id, message).await;
            return Err(AppError::overloaded(message, 2));
        }
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let owns_lease: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "BlobDeletionOperations"
                WHERE content_hash = $1 AND operation_id = $2
                  AND state = 'Deleting'
           )"#,
    )
    .bind(hash)
    .bind(operation_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if !owns_lease {
        return Err(AppError::conflict("Blob deletion ownership changed"));
    }
    if hash_is_referenced(&mut transaction, hash).await? {
        sqlx::query(
            r#"DELETE FROM "BlobDeletionOperations"
                WHERE content_hash = $1 AND operation_id = $2"#,
        )
        .bind(hash)
        .bind(operation_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        return Err(AppError::internal(
            "Blob gained an owner while deletion was in progress",
        ));
    }
    sqlx::query(
        r#"DELETE FROM "Files"
            WHERE hash = $1 AND reference_count <= 0"#,
    )
    .bind(hash)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"DELETE FROM "BlobDeletionOperations"
            WHERE content_hash = $1 AND operation_id = $2"#,
    )
    .bind(hash)
    .bind(operation_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(true)
}

async fn hash_is_referenced(
    transaction: &mut Transaction<'_, Postgres>,
    hash: &str,
) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS(
                    SELECT 1 FROM "Files"
                     WHERE hash = $1 AND reference_count > 0
               )
               OR EXISTS(
                    SELECT 1 FROM "Attachments" attachment
                    JOIN "Files" file ON file.id = attachment.local_file_id
                     WHERE file.hash = $1
               )
               OR EXISTS(
                    SELECT 1 FROM "Participations" participation
                    JOIN "Files" file ON file.id = participation.writeup_id
                     WHERE file.hash = $1
               )
               OR EXISTS(
                    SELECT 1 FROM "AdServiceSnapshots" snapshot
                    JOIN "Files" file ON file.id = snapshot.local_file_id
                     WHERE file.hash = $1
               )
               OR EXISTS(SELECT 1 FROM "AspNetUsers" WHERE avatar_hash = $1)
               OR EXISTS(SELECT 1 FROM "Teams" WHERE avatar_hash = $1)
               OR EXISTS(SELECT 1 FROM "Games" WHERE poster_hash = $1)
               OR EXISTS(
                    SELECT 1 FROM "Configs"
                     WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
                       AND value = $1
               )
               OR EXISTS(
                    SELECT 1 FROM "GameChallenges"
                     WHERE original_archive_blob_path = $1
               )
               OR EXISTS(
                    SELECT 1 FROM "BlobStagingOperations" stage
                     WHERE stage.content_hash = $1
                       AND stage.state IN ('Storing', 'Ready')
                       AND stage.lease_expires_at_utc > clock_timestamp()
               )"#,
    )
    .bind(hash)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)
}

async fn claim_blob_deletion(pool: &PgPool, hash: &str) -> AppResult<Option<uuid::Uuid>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(DELETE_ADMISSION_LOCK)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    if hash_is_referenced(&mut transaction, hash).await? {
        sqlx::query(r#"DELETE FROM "BlobDeletionOperations" WHERE content_hash = $1"#)
            .bind(hash)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        return Ok(None);
    }
    let active: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "BlobDeletionOperations"
            WHERE state = 'Deleting'
              AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    let already_active: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "BlobDeletionOperations"
                WHERE content_hash = $1 AND state = 'Deleting'
                  AND lease_expires_at_utc > clock_timestamp()
           )"#,
    )
    .bind(hash)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if already_active {
        transaction.commit().await.map_err(database_error)?;
        return Ok(None);
    }
    if active >= DEPLOYMENT_DELETE_JOBS {
        transaction.commit().await.map_err(database_error)?;
        return Err(AppError::overloaded("Blob deletion capacity is busy", 2));
    }
    let operation_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "BlobDeletionOperations"
               (content_hash, operation_id, state, lease_expires_at_utc,
                updated_at_utc, last_error)
           VALUES ($1, $2, 'Deleting',
                   clock_timestamp() + interval '2 minutes',
                   clock_timestamp(), NULL)
           ON CONFLICT (content_hash) DO UPDATE
             SET operation_id = EXCLUDED.operation_id,
                 state = 'Deleting',
                 lease_expires_at_utc = EXCLUDED.lease_expires_at_utc,
                 updated_at_utc = clock_timestamp(),
                 last_error = NULL"#,
    )
    .bind(hash)
    .bind(operation_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(Some(operation_id))
}

async fn mark_blob_deletion_failed(
    pool: &PgPool,
    hash: &str,
    operation_id: uuid::Uuid,
    error: &str,
) {
    if let Err(update_error) = sqlx::query(
        r#"UPDATE "BlobDeletionOperations"
              SET state = 'Failed', lease_expires_at_utc = clock_timestamp(),
                  updated_at_utc = clock_timestamp(), last_error = left($3, 1000)
            WHERE content_hash = $1 AND operation_id = $2"#,
    )
    .bind(hash)
    .bind(operation_id)
    .bind(error)
    .execute(pool)
    .await
    {
        tracing::warn!(%hash, %operation_id, %update_error, "failed to persist blob deletion failure");
    }
}

/// Retry a bounded batch of durable zero-reference blob tombstones.
///
/// The final-release transaction intentionally leaves these rows behind until
/// object storage acknowledges deletion. Running this from singleton
/// maintenance closes the commit-to-object-delete crash window. The durable
/// deletion lease makes the external delete retryable across replicas.
pub async fn purge_pending(pool: &PgPool, storage: &dyn BlobStorage, limit: i64) -> AppResult<u64> {
    let hashes = sqlx::query_scalar::<_, String>(
        r#"SELECT file.hash FROM "Files" file
            WHERE file.reference_count <= 0
              AND NOT EXISTS (
                  SELECT 1 FROM "BlobStagingOperations" stage
                   WHERE stage.content_hash = file.hash
                     AND stage.state IN ('Storing', 'Ready')
                     AND stage.lease_expires_at_utc > clock_timestamp()
              )
            ORDER BY file.id
            LIMIT $1"#,
    )
    .bind(limit.clamp(1, 256))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let mut purged = 0;
    for hash in hashes {
        purged += u64::from(purge_if_unreferenced(pool, storage, &hash).await?);
    }
    Ok(purged)
}

#[cfg(test)]
mod tests;
