//! One recoverable renewal per worker; old certificates have a bounded overlap.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "WorkerCertificateRotations" (
    worker_id UUID PRIMARY KEY REFERENCES "WorkerNodes"(id) ON DELETE CASCADE,
    operation_id UUID NOT NULL UNIQUE,
    csr_digest BYTEA NOT NULL CHECK (octet_length(csr_digest) = 32),
    old_fingerprint BYTEA NOT NULL CHECK (octet_length(old_fingerprint) = 32),
    new_fingerprint BYTEA NOT NULL CHECK (octet_length(new_fingerprint) = 32),
    old_expires_at TIMESTAMPTZ NOT NULL,
    overlap_until TIMESTAMPTZ NOT NULL,
    response JSONB NOT NULL CHECK (octet_length(response::text) <= 16384),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_worker_rotation_old_fingerprint
    ON "WorkerCertificateRotations" (old_fingerprint);
"#;

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
