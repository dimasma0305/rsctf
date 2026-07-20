//! Compatibility adapter for atomic blob acquisition from legacy SeaORM writes.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement};

use crate::storage::{BlobStorage, StoredBlob};
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

use super::UPSERT_FILE_SQL;

/// Keep blob metadata in the caller's transaction with its owning domain row.
/// New write paths should prefer the SQLx helper in the parent module; archive
/// import uses this because its existing enum-rich inserts are SeaORM models.
pub(crate) async fn store_and_acquire_in_seaorm_transaction(
    storage: &dyn BlobStorage,
    transaction: &DatabaseTransaction,
    name: &str,
    bytes: &[u8],
) -> AppResult<(StoredBlob, i32)> {
    let expected_hash = sha256_hex(bytes);
    transaction
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [expected_hash.clone().into()],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let blob = storage.store(name, bytes).await?;
    if blob.hash != expected_hash {
        return Err(AppError::internal(
            "blob storage returned a hash that does not match its content",
        ));
    }
    let row = transaction
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            UPSERT_FILE_SQL,
            [
                blob.hash.clone().into(),
                blob.size.into(),
                name.to_owned().into(),
            ],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::internal("blob metadata upsert returned no row"))?;
    let id = row
        .try_get::<i32>("", "id")
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((blob, id))
}
