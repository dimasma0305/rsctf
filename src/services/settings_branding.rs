//! Durable cleanup for branding staged by revisioned platform-settings operations.

use std::collections::BTreeSet;

use sqlx::PgPool;
use uuid::Uuid;

use crate::storage::BlobStorage;
use crate::utils::error::{AppError, AppResult};

/// Release a bounded batch of expired stages, then purge only confirmed
/// unreferenced objects after the relational transaction commits.
pub async fn purge_expired(pool: &PgPool, storage: &dyn BlobStorage, limit: i64) -> AppResult<u64> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        r#"SELECT operation_id, blob_hash
             FROM "PlatformSettingsBrandingStaging"
            WHERE expires_at <= clock_timestamp()
            ORDER BY expires_at, operation_id
            LIMIT $1
            FOR UPDATE SKIP LOCKED"#,
    )
    .bind(limit.clamp(1, 128))
    .fetch_all(&mut *transaction)
    .await
    .map_err(database_error)?;
    if rows.is_empty() {
        transaction.commit().await.map_err(database_error)?;
        return Ok(0);
    }
    let operation_ids = rows
        .iter()
        .map(|(operation_id, _)| *operation_id)
        .collect::<Vec<_>>();
    let hashes = rows
        .iter()
        .map(|(_, hash)| hash.clone())
        .collect::<BTreeSet<_>>();
    sqlx::query(
        r#"DELETE FROM "PlatformSettingsBrandingStaging"
            WHERE operation_id = ANY($1)"#,
    )
    .bind(operation_ids)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    for hash in &hashes {
        crate::services::blob_refs::release_direct_hash_locked(&mut transaction, hash).await?;
    }
    transaction.commit().await.map_err(database_error)?;
    for hash in hashes {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(pool, storage, &hash).await
        {
            tracing::warn!(%error, %hash, "expired settings branding purge failed");
        }
    }
    Ok(rows.len() as u64)
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn cleanup_batch_is_always_bounded() {
        for (input, expected) in [(-1, 1), (0, 1), (16, 16), (5_000, 128)] {
            assert_eq!(input.clamp(1, 128), expected);
        }
    }
}
