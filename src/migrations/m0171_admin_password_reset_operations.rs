//! Idempotent administrator-issued password reset results.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdminPasswordResetOperations" (
  operation_id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
  requested_by UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
  status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
  lease_token UUID NOT NULL,
  lease_expires_at_utc TIMESTAMPTZ NOT NULL,
  result_ciphertext BYTEA NULL CHECK (result_ciphertext IS NULL OR OCTET_LENGTH(result_ciphertext) <= 1024),
  result_nonce BYTEA NULL CHECK (result_nonce IS NULL OR OCTET_LENGTH(result_nonce) = 12),
  result_expires_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp() + INTERVAL '15 minutes',
  created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  completed_at_utc TIMESTAMPTZ NULL,
  CONSTRAINT ck_admin_password_reset_result CHECK (
    (status = 1 AND result_ciphertext IS NOT NULL AND result_nonce IS NOT NULL)
    OR status <> 1
  )
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_admin_password_reset_active_user
  ON "AdminPasswordResetOperations"(user_id) WHERE status = 0;
CREATE INDEX IF NOT EXISTS ix_admin_password_reset_result_expiry
  ON "AdminPasswordResetOperations"(result_expires_at_utc);
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
