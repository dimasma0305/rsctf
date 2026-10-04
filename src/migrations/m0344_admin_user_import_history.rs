//! Durable, non-secret administrator user-import history.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdminCredentialJobs"
  ADD COLUMN IF NOT EXISTS source_name VARCHAR(255) NULL;

CREATE INDEX IF NOT EXISTS ix_admin_credential_jobs_history
  ON "AdminCredentialJobs" (created_at_utc DESC, operation_id DESC);

CREATE TABLE IF NOT EXISTS "AdminUserImportHistoryRows" (
  operation_id UUID NOT NULL
    REFERENCES "AdminCredentialJobs"(operation_id) ON DELETE CASCADE,
  row_index INTEGER NOT NULL CHECK (row_index BETWEEN 0 AND 199),
  user_id UUID NULL REFERENCES "AspNetUsers"(id) ON DELETE SET NULL,
  email VARCHAR(512) NOT NULL
    CHECK (OCTET_LENGTH(email) <= 512),
  real_name VARCHAR(512) NOT NULL
    CHECK (OCTET_LENGTH(real_name) <= 512),
  user_name VARCHAR(128) NOT NULL
    CHECK (OCTET_LENGTH(user_name) BETWEEN 1 AND 128),
  team_name VARCHAR(512) NULL
    CHECK (team_name IS NULL OR OCTET_LENGTH(team_name) <= 512),
  outcome VARCHAR(16) NOT NULL
    CHECK (outcome IN ('created', 'updated', 'skipped')),
  error VARCHAR(1024) NULL
    CHECK (error IS NULL OR OCTET_LENGTH(error) <= 1024),
  direct_delivery_status SMALLINT NOT NULL DEFAULT 0
    CHECK (direct_delivery_status BETWEEN 0 AND 2),
  direct_delivery_error VARCHAR(512) NULL
    CHECK (direct_delivery_error IS NULL OR OCTET_LENGTH(direct_delivery_error) <= 512),
  delivery_attempted_at_utc TIMESTAMPTZ NULL,
  last_mail_operation_id UUID NULL
    REFERENCES "MailOutbox"(operation_id) ON DELETE SET NULL,
  PRIMARY KEY (operation_id, row_index)
);

CREATE INDEX IF NOT EXISTS ix_admin_user_import_history_user
  ON "AdminUserImportHistoryRows" (user_id)
  WHERE user_id IS NOT NULL;

ALTER TABLE "AdminCredentialJobRows"
  DROP CONSTRAINT IF EXISTS ck_admin_credential_job_row_result;
ALTER TABLE "AdminCredentialJobRows"
  ADD CONSTRAINT ck_admin_credential_job_row_result CHECK (
    (status = 0 AND result_ciphertext IS NULL AND result_nonce IS NULL
                AND completed_at_utc IS NULL)
    OR (status = 1 AND completed_at_utc IS NOT NULL
        AND ((result_ciphertext IS NOT NULL AND result_nonce IS NOT NULL)
             OR (result_ciphertext IS NULL AND result_nonce IS NULL)))
  );
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

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn history_is_bounded_non_secret_and_keeps_credential_expiry_separate() {
        assert!(UP_SQL.contains("row_index BETWEEN 0 AND 199"));
        assert!(UP_SQL.contains("email VARCHAR(512)"));
        assert!(UP_SQL.contains("last_mail_operation_id"));
        assert!(!UP_SQL.contains("password"));
        assert!(UP_SQL.contains("result_ciphertext IS NULL AND result_nonce IS NULL"));
        assert!(UP_SQL.contains("ix_admin_credential_jobs_history"));
    }
}
