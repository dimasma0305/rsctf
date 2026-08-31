use sea_orm_migration::prelude::*;

pub const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AccountLinkAttempts" (
    token_digest BYTEA PRIMARY KEY CHECK (octet_length(token_digest) = 32),
    purpose SMALLINT NOT NULL CHECK (purpose IN (0, 1)),
    account_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL CHECK (generation > 0),
    security_generation_digest BYTEA NOT NULL
        CHECK (octet_length(security_generation_digest) = 32),
    destination_digest BYTEA NOT NULL CHECK (octet_length(destination_digest) = 32),
    expires_at_utc TIMESTAMPTZ NOT NULL,
    is_current BOOLEAN NOT NULL DEFAULT FALSE,
    delivered_at_utc TIMESTAMPTZ,
    consumed_at_utc TIMESTAMPTZ,
    result JSONB,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK ((consumed_at_utc IS NULL) = (result IS NULL)),
    CHECK (NOT is_current OR delivered_at_utc IS NOT NULL),
    CHECK (result IS NULL OR jsonb_typeof(result) = 'object')
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_account_link_current_generation
    ON "AccountLinkAttempts" (account_id, purpose)
    WHERE is_current AND consumed_at_utc IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS ux_account_link_generation
    ON "AccountLinkAttempts" (account_id, purpose, generation);
CREATE INDEX IF NOT EXISTS ix_account_link_retention
    ON "AccountLinkAttempts" (account_id, purpose, expires_at_utc);
CREATE INDEX IF NOT EXISTS ix_account_link_expiry_retention
    ON "AccountLinkAttempts" (expires_at_utc, token_digest);
CREATE INDEX IF NOT EXISTS ix_account_link_noncurrent_retention
    ON "AccountLinkAttempts" (account_id, purpose, created_at_utc DESC)
    WHERE NOT is_current;
"#;

const DOWN_SQL: &str = r#"DROP TABLE IF EXISTS "AccountLinkAttempts";"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn account_links_store_only_fixed_digests_and_one_terminal_result() {
        assert!(UP_SQL.contains("token_digest BYTEA PRIMARY KEY"));
        assert!(UP_SQL.contains("security_generation_digest BYTEA"));
        assert!(UP_SQL.contains("destination_digest BYTEA"));
        assert!(UP_SQL.contains("WHERE is_current AND consumed_at_utc IS NULL"));
        assert!(UP_SQL.contains("(consumed_at_utc IS NULL) = (result IS NULL)"));
        assert!(UP_SQL.contains("NOT is_current OR delivered_at_utc IS NOT NULL"));
        assert!(UP_SQL.contains("ix_account_link_expiry_retention"));
        assert!(UP_SQL.contains("ix_account_link_noncurrent_retention"));
    }
}
