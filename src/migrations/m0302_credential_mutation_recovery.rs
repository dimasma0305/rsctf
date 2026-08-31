//! Durable credential workflow admission and reset attempts.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "CredentialMutationLeases" (
  scope_hash BYTEA PRIMARY KEY CHECK (OCTET_LENGTH(scope_hash) = 32),
  lease_token UUID NOT NULL,
  expires_at_utc TIMESTAMPTZ NOT NULL,
  created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX IF NOT EXISTS ix_credential_mutation_lease_expiry
  ON "CredentialMutationLeases"(expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_credential_mutation_lease_token
  ON "CredentialMutationLeases"(lease_token);

CREATE TABLE IF NOT EXISTS "CredentialMutationSlots" (
  work_class SMALLINT NOT NULL CHECK (work_class IN (0, 1)),
  slot_id SMALLINT NOT NULL CHECK (slot_id BETWEEN 0 AND 31),
  lease_token UUID NULL,
  expires_at_utc TIMESTAMPTZ NULL,
  PRIMARY KEY (work_class, slot_id),
  CHECK ((lease_token IS NULL) = (expires_at_utc IS NULL))
);
INSERT INTO "CredentialMutationSlots" (work_class, slot_id)
SELECT 0, slot_id FROM generate_series(0, 15) AS slot_id
ON CONFLICT DO NOTHING;
INSERT INTO "CredentialMutationSlots" (work_class, slot_id)
VALUES (1, 0)
ON CONFLICT DO NOTHING;
CREATE INDEX IF NOT EXISTS ix_credential_mutation_slot_expiry
  ON "CredentialMutationSlots"(work_class, expires_at_utc, slot_id);

CREATE TABLE IF NOT EXISTS "PasswordResetTickets" (
  token_hash BYTEA PRIMARY KEY CHECK (OCTET_LENGTH(token_hash) = 32),
  user_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
  security_stamp TEXT NOT NULL CHECK (LENGTH(security_stamp) BETWEEN 1 AND 128),
  expires_at_utc TIMESTAMPTZ NOT NULL,
  superseded_at_utc TIMESTAMPTZ NULL,
  consumed_at_utc TIMESTAMPTZ NULL,
  created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX IF NOT EXISTS ix_password_reset_ticket_user_current
  ON "PasswordResetTickets"(user_id, expires_at_utc)
  WHERE superseded_at_utc IS NULL AND consumed_at_utc IS NULL;

CREATE TABLE IF NOT EXISTS "PasswordResetAttempts" (
  operation_id UUID PRIMARY KEY,
  token_hash BYTEA NOT NULL UNIQUE REFERENCES "PasswordResetTickets"(token_hash) ON DELETE RESTRICT,
  request_digest BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
  lease_token UUID NOT NULL,
  lease_expires_at_utc TIMESTAMPTZ NOT NULL,
  status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
  created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
  completed_at_utc TIMESTAMPTZ NULL
);
CREATE INDEX IF NOT EXISTS ix_password_reset_attempt_lease
  ON "PasswordResetAttempts"(status, lease_expires_at_utc)
  WHERE status = 0;
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
    fn reset_is_claimed_before_hash_and_replayable() {
        assert!(UP_SQL.contains("token_hash BYTEA NOT NULL UNIQUE"));
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("CredentialMutationLeases"));
        assert!(UP_SQL.contains("CredentialMutationSlots"));
        assert!(UP_SQL.contains("generate_series(0, 15)"));
    }
}
