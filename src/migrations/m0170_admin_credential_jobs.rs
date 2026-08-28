//! Replayable bounded administrator import credential results.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdminCredentialJobs" (
  operation_id UUID PRIMARY KEY,
  requested_by UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE RESTRICT,
  request_digest BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
  row_count INTEGER NOT NULL CHECK (row_count BETWEEN 1 AND 200),
  status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
  created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  completed_at_utc TIMESTAMPTZ NULL,
  result_expires_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp() + INTERVAL '1 hour'
);
CREATE INDEX IF NOT EXISTS ix_admin_credential_jobs_expiry
  ON "AdminCredentialJobs"(result_expires_at_utc);

CREATE TABLE IF NOT EXISTS "AdminCredentialJobRows" (
  operation_id UUID NOT NULL REFERENCES "AdminCredentialJobs"(operation_id) ON DELETE CASCADE,
  row_index INTEGER NOT NULL CHECK (row_index BETWEEN 0 AND 199),
  result_ciphertext BYTEA NOT NULL CHECK (OCTET_LENGTH(result_ciphertext) <= 8192),
  result_nonce BYTEA NOT NULL CHECK (OCTET_LENGTH(result_nonce) = 12),
  completed_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (operation_id, row_index)
);

CREATE TABLE IF NOT EXISTS "AdminCredentialTargetLeases" (
  normalized_email TEXT PRIMARY KEY CHECK (LENGTH(normalized_email) BETWEEN 3 AND 256),
  operation_id UUID NOT NULL REFERENCES "AdminCredentialJobs"(operation_id) ON DELETE CASCADE,
  expires_at_utc TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_admin_credential_target_lease_expiry
  ON "AdminCredentialTargetLeases"(expires_at_utc);
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
        assert!(UP_SQL.contains("result_ciphertext BYTEA"));
        assert!(UP_SQL.contains("PRIMARY KEY (operation_id, row_index)"));
    }
}
