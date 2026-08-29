//! Retry-safe identity and response recovery for signed KotH observations.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "KothApiObservationOperations" (
    challenge_id INTEGER NOT NULL,
    game_id INTEGER NOT NULL,
    request_digest BYTEA NOT NULL,
    context_hash CHAR(64) NOT NULL,
    lease_token UUID NOT NULL,
    lease_expires_at TIMESTAMPTZ NOT NULL,
    response JSONB NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at TIMESTAMPTZ NULL,
    expires_at TIMESTAMPTZ NOT NULL
        DEFAULT (clock_timestamp() + interval '10 minutes'),
    PRIMARY KEY (challenge_id, request_digest),
    CONSTRAINT fk_koth_observation_operations_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_observation_operations_digest
        CHECK (OCTET_LENGTH(request_digest) = 32),
    CONSTRAINT ck_koth_observation_operations_context
        CHECK (context_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT ck_koth_observation_operations_completion
        CHECK ((response IS NULL AND completed_at IS NULL)
            OR (response IS NOT NULL AND completed_at IS NOT NULL
                AND jsonb_typeof(response) = 'object')),
    CONSTRAINT ck_koth_observation_operations_expiry
        CHECK (expires_at > created_at)
);

CREATE INDEX IF NOT EXISTS ix_koth_observation_operations_expiry
    ON "KothApiObservationOperations"(expires_at);
CREATE INDEX IF NOT EXISTS ix_koth_observation_operations_scope
    ON "KothApiObservationOperations"(game_id, challenge_id, created_at DESC);
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
    fn observation_retry_records_are_bounded_and_scope_unique() {
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, request_digest)"));
        assert!(UP_SQL.contains("response JSONB NULL"));
        assert!(UP_SQL.contains("lease_expires_at TIMESTAMPTZ NOT NULL"));
        assert!(UP_SQL.contains("interval '10 minutes'"));
        assert!(UP_SQL.contains("ON DELETE CASCADE"));
    }
}
