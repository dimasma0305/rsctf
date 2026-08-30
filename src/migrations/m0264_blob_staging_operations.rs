//! Durable blob intent used before external object storage I/O.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "BlobStagingOperations" (
    operation_id UUID PRIMARY KEY,
    owner_scope TEXT NOT NULL CHECK (octet_length(owner_scope) BETWEEN 1 AND 512),
    owner_user_id UUID NULL,
    content_hash VARCHAR(64) NOT NULL,
    file_name VARCHAR(255) NOT NULL,
    file_size BIGINT NOT NULL CHECK (file_size > 0),
    state VARCHAR(16) NOT NULL CHECK (state IN ('Storing', 'Ready', 'Published', 'Failed')),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_expires_at_utc TIMESTAMPTZ NOT NULL,
    published_at_utc TIMESTAMPTZ NULL,
    last_error TEXT CHECK (last_error IS NULL OR octet_length(last_error) <= 1000),
    CHECK (content_hash ~ '^[0-9a-f]{64}$'),
    CHECK ((state = 'Published') = (published_at_utc IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS ix_blob_staging_expiry
    ON "BlobStagingOperations" (lease_expires_at_utc, operation_id);
CREATE INDEX IF NOT EXISTS ix_blob_staging_hash
    ON "BlobStagingOperations" (content_hash)
    WHERE state <> 'Published';
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
    fn operation_identity_content_and_expiry_are_bounded_by_schema() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("CHECK (file_size > 0)"));
        assert!(UP_SQL.contains("state IN ('Storing', 'Ready', 'Published', 'Failed')"));
        assert!(UP_SQL.contains("ix_blob_staging_expiry"));
        assert!(UP_SQL.contains("octet_length(last_error) <= 1000"));
    }
}
