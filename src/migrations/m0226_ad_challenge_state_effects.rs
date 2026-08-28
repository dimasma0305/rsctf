//! Durable post-commit reconciliation for A&D/KotH challenge desired state.

use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdChallengeStateEffects" (
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    revision BIGINT NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
    desired_enabled BOOLEAN NOT NULL,
    operation_id UUID NOT NULL UNIQUE,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 1000000),
    next_attempt_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    claim_id UUID NULL,
    claim_expires_at_utc TIMESTAMPTZ NULL,
    last_error TEXT NULL CHECK (last_error IS NULL OR OCTET_LENGTH(last_error) <= 2048),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ NULL,
    PRIMARY KEY (challenge_id, revision),
    CONSTRAINT fk_ad_challenge_state_effect_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id) ON DELETE CASCADE,
    CONSTRAINT ck_ad_challenge_state_effect_claim_pair CHECK (
        (claim_id IS NULL) = (claim_expires_at_utc IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS ix_ad_challenge_state_effect_due
    ON "AdChallengeStateEffects"(next_attempt_at_utc, challenge_id, revision)
    WHERE completed_at_utc IS NULL;

CREATE INDEX IF NOT EXISTS ix_ad_challenge_state_effect_retention
    ON "AdChallengeStateEffects"(completed_at_utc, challenge_id, revision)
    WHERE completed_at_utc IS NOT NULL;
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
        // Forward-only: state-effect rows are part of the durable operation
        // ledger and must not be destroyed by an application rollback.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn state_effects_are_idempotent_leased_and_bounded() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"AdChallengeStateEffects\""));
        assert!(UP_SQL.contains("PRIMARY KEY (challenge_id, revision)"));
        assert!(UP_SQL.contains("operation_id UUID NOT NULL UNIQUE"));
        assert!(UP_SQL.contains("claim_expires_at_utc"));
        assert!(UP_SQL.contains("OCTET_LENGTH(last_error) <= 2048"));
        assert!(UP_SQL.contains("WHERE completed_at_utc IS NULL"));
    }
}
