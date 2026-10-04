//! Replayable bounded administrator import credential results.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdminCredentialJobs" (
  operation_id UUID PRIMARY KEY,
  requested_by UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
  request_digest BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
  row_count INTEGER NOT NULL CHECK (row_count BETWEEN 1 AND 200),
  status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
  created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  completed_at_utc TIMESTAMPTZ NULL,
  result_expires_at_utc TIMESTAMPTZ NULL
);
CREATE INDEX IF NOT EXISTS ix_admin_credential_jobs_expiry
  ON "AdminCredentialJobs"(result_expires_at_utc);

CREATE TABLE IF NOT EXISTS "AdminCredentialJobRows" (
  operation_id UUID NOT NULL REFERENCES "AdminCredentialJobs"(operation_id) ON DELETE CASCADE,
  row_index INTEGER NOT NULL CHECK (row_index BETWEEN 0 AND 199),
  status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 1),
  lease_token UUID NOT NULL,
  lease_expires_at_utc TIMESTAMPTZ NOT NULL,
  result_ciphertext BYTEA NULL CHECK (
    result_ciphertext IS NULL OR OCTET_LENGTH(result_ciphertext) <= 8192
  ),
  result_nonce BYTEA NULL CHECK (
    result_nonce IS NULL OR OCTET_LENGTH(result_nonce) = 12
  ),
  completed_at_utc TIMESTAMPTZ NULL,
  PRIMARY KEY (operation_id, row_index),
  CONSTRAINT ck_admin_credential_job_row_result CHECK (
    (status = 0 AND result_ciphertext IS NULL AND result_nonce IS NULL
                AND completed_at_utc IS NULL)
    OR (status = 1 AND result_ciphertext IS NOT NULL AND result_nonce IS NOT NULL
                   AND completed_at_utc IS NOT NULL)
  )
);
CREATE INDEX IF NOT EXISTS ix_admin_credential_job_row_lease
  ON "AdminCredentialJobRows"(status, lease_expires_at_utc) WHERE status = 0;

CREATE TABLE IF NOT EXISTS "AdminCredentialTargetLeases" (
  normalized_email TEXT PRIMARY KEY CHECK (OCTET_LENGTH(normalized_email) BETWEEN 3 AND 320),
  operation_id UUID NOT NULL REFERENCES "AdminCredentialJobs"(operation_id) ON DELETE CASCADE,
  expires_at_utc TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_admin_credential_target_lease_expiry
  ON "AdminCredentialTargetLeases"(expires_at_utc);

CREATE TABLE IF NOT EXISTS "AdminPasswordResetOperations" (
  operation_id UUID PRIMARY KEY,
  user_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
  requested_by UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
  status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
  lease_token UUID NOT NULL,
  lease_expires_at_utc TIMESTAMPTZ NOT NULL,
  result_ciphertext BYTEA NULL CHECK (
    result_ciphertext IS NULL OR OCTET_LENGTH(result_ciphertext) <= 1024
  ),
  result_nonce BYTEA NULL CHECK (
    result_nonce IS NULL OR OCTET_LENGTH(result_nonce) = 12
  ),
  result_expires_at_utc TIMESTAMPTZ NOT NULL
    DEFAULT clock_timestamp() + INTERVAL '15 minutes',
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_bound_rows_and_encrypt_each_replay_result() {
        assert!(UP_SQL.contains("row_count BETWEEN 1 AND 200"));
        assert!(UP_SQL.contains("lease_expires_at_utc TIMESTAMPTZ NOT NULL"));
        assert!(UP_SQL.contains("result_ciphertext BYTEA NULL"));
        assert!(UP_SQL.contains("status = 0 AND result_ciphertext IS NULL"));
        assert!(UP_SQL.contains("status = 1 AND result_ciphertext IS NOT NULL"));
        assert!(UP_SQL.contains("OCTET_LENGTH(normalized_email) BETWEEN 3 AND 320"));
        assert!(UP_SQL.contains("PRIMARY KEY (operation_id, row_index)"));
        assert!(UP_SQL.contains("AdminPasswordResetOperations"));
    }
}
