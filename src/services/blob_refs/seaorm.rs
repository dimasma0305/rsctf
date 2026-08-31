//! Compatibility adapter for staged blob publication from legacy SeaORM writes.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement};

use crate::utils::error::{AppError, AppResult};

use super::{StagedBlob, UPSERT_FILE_SQL};

/// Publish bytes that were already staged outside the caller's transaction.
/// Archive import retains SeaORM for its enum-rich domain inserts, but object
/// storage must never run while that transaction owns database rows.
pub(crate) async fn publish_staged_blob_in_seaorm_transaction(
    transaction: &DatabaseTransaction,
    staged: &StagedBlob,
) -> AppResult<i32> {
    transaction
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [staged.blob.hash.clone().into()],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let row = transaction
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT owner_scope, owner_user_id, content_hash, file_name, file_size,
                      state, published_owner_scope,
                      lease_expires_at_utc > clock_timestamp() AS lease_active
                 FROM "BlobStagingOperations"
                WHERE operation_id = $1
                FOR UPDATE"#,
            [staged.operation_id.into()],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::conflict("Blob staging operation expired"))?;
    let owner_scope = row
        .try_get::<String>("", "owner_scope")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let owner_user_id = row
        .try_get::<Option<uuid::Uuid>>("", "owner_user_id")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let content_hash = row
        .try_get::<String>("", "content_hash")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let file_name = row
        .try_get::<String>("", "file_name")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let file_size = row
        .try_get::<i64>("", "file_size")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let state = row
        .try_get::<String>("", "state")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let published_owner_scope = row
        .try_get::<Option<String>>("", "published_owner_scope")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let lease_active = row
        .try_get::<bool>("", "lease_active")
        .map_err(|error| AppError::internal(error.to_string()))?;
    if owner_scope != staged.owner_scope
        || owner_user_id != staged.owner_user_id
        || content_hash != staged.blob.hash
        || file_name != staged.blob.name
        || file_size != staged.blob.size
    {
        return Err(AppError::conflict(
            "Blob operation identity was reused for different content",
        ));
    }
    if state == "Published" {
        if published_owner_scope.as_deref() != Some(staged.owner_scope.as_str()) {
            return Err(AppError::conflict(
                "Blob upload receipt was already consumed by another owner",
            ));
        }
        return transaction
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                r#"SELECT id FROM "Files" WHERE hash = $1 AND reference_count > 0"#,
                [content_hash.into()],
            ))
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::conflict("published blob is no longer owned"))?
            .try_get::<i32>("", "id")
            .map_err(|error| AppError::internal(error.to_string()));
    }
    if state != "Ready" || !lease_active {
        return Err(AppError::conflict("Blob staging operation is not ready"));
    }
    let row = transaction
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            UPSERT_FILE_SQL,
            [
                staged.blob.hash.clone().into(),
                staged.blob.size.into(),
                staged.blob.name.clone().into(),
            ],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::internal("blob metadata upsert returned no row"))?;
    let id = row
        .try_get::<i32>("", "id")
        .map_err(|error| AppError::internal(error.to_string()))?;
    let updated = transaction
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"UPDATE "BlobStagingOperations"
                  SET state = 'Published', published_at_utc = clock_timestamp(),
                      published_owner_scope = $2,
                      lease_expires_at_utc = clock_timestamp() + interval '24 hours'
                WHERE operation_id = $1 AND state = 'Ready'
                  AND lease_expires_at_utc > clock_timestamp()"#,
            [
                staged.operation_id.into(),
                staged.owner_scope.clone().into(),
            ],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("Blob staging operation is not ready"));
    }
    Ok(id)
}
