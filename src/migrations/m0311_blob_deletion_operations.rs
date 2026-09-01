//! Durable object-store deletion leases.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "BlobDeletionOperations" (
    content_hash VARCHAR(64) PRIMARY KEY,
    operation_id UUID NOT NULL,
    state VARCHAR(16) NOT NULL CHECK (state IN ('Deleting', 'Failed')),
    lease_expires_at_utc TIMESTAMPTZ NOT NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_error TEXT NULL,
    CHECK (content_hash ~ '^[0-9a-f]{64}$')
);
CREATE INDEX IF NOT EXISTS ix_blob_deletion_operation_expiry
    ON "BlobDeletionOperations" (lease_expires_at_utc, content_hash);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn deletion_identity_and_lease_are_durable_and_bounded() {
        assert!(UP_SQL.contains("content_hash VARCHAR(64) PRIMARY KEY"));
        assert!(UP_SQL.contains("operation_id UUID NOT NULL"));
        assert!(UP_SQL.contains("state IN ('Deleting', 'Failed')"));
        assert!(UP_SQL.contains("ix_blob_deletion_operation_expiry"));
    }
}
