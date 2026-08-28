//! Hashed, bounded, replayable account-link attempts.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AccountLinkAttempts" (
    token_digest TEXT PRIMARY KEY,
    purpose TEXT NOT NULL,
    account_id UUID NOT NULL,
    security_generation_digest TEXT NOT NULL,
    destination_digest TEXT NOT NULL,
    expires_at_utc TIMESTAMPTZ NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    terminal_result SMALLINT NULL,
    safe_result TEXT NULL,
    issued_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    issued_sequence BIGSERIAL NOT NULL,
    completed_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_accountlinkattempts_purpose
        CHECK (purpose IN ('registration', 'email_change')),
    CONSTRAINT ck_accountlinkattempts_digests
        CHECK (char_length(token_digest) = 64
           AND char_length(security_generation_digest) = 64
           AND char_length(destination_digest) = 64),
    CONSTRAINT ck_accountlinkattempts_result
        CHECK (terminal_result IS NULL OR terminal_result IN (1, 2))
);

ALTER TABLE "AccountLinkAttempts"
    ADD COLUMN IF NOT EXISTS issued_sequence BIGSERIAL;
CREATE UNIQUE INDEX IF NOT EXISTS ux_accountlinkattempts_issued_sequence
    ON "AccountLinkAttempts" (issued_sequence);

CREATE INDEX IF NOT EXISTS ix_accountlinkattempts_account_purpose_issued
    ON "AccountLinkAttempts" (account_id, purpose, issued_sequence DESC);
CREATE INDEX IF NOT EXISTS ix_accountlinkattempts_retention
    ON "AccountLinkAttempts" (expires_at_utc, completed_at_utc);
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
