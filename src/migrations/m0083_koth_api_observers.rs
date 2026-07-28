//! Challenge-scoped, signed KotH observer input and its current exact-context claim.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "KothApiObservers" (
    challenge_id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL,
    hmac_secret TEXT NOT NULL,
    secret_hint VARCHAR(16) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_used_at TIMESTAMPTZ NULL,
    CONSTRAINT fk_koth_api_observers_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_api_observers_secret
        CHECK (
            OCTET_LENGTH(hmac_secret) BETWEEN 48 AND 128
            AND hmac_secret LIKE 'koth_api_%'
            AND BTRIM(secret_hint) <> ''
        )
);

CREATE INDEX IF NOT EXISTS ix_koth_api_observers_game
    ON "KothApiObservers"(game_id, challenge_id);

CREATE TABLE IF NOT EXISTS "KothApiObservations" (
    target_id INTEGER PRIMARY KEY,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    cycle_id BIGINT NOT NULL,
    reset_attempt INTEGER NOT NULL,
    container_id TEXT NOT NULL,
    token_id INTEGER NULL,
    context_hash CHAR(64) NOT NULL,
    request_timestamp_ms BIGINT NOT NULL,
    accepted_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT fk_koth_api_observations_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_observations_target
        FOREIGN KEY (target_id, challenge_id)
        REFERENCES "KothTargets"(id, challenge_id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_observations_cycle
        FOREIGN KEY (cycle_id, challenge_id)
        REFERENCES "KothCrownCycles"(id, challenge_id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_api_observations_token
        FOREIGN KEY (token_id, cycle_id)
        REFERENCES "KothTokens"(id, cycle_id)
        ON DELETE SET NULL (token_id),
    CONSTRAINT ck_koth_api_observations_attempt
        CHECK (reset_attempt >= 0),
    CONSTRAINT ck_koth_api_observations_container
        CHECK (BTRIM(container_id) <> ''),
    CONSTRAINT ck_koth_api_observations_context
        CHECK (context_hash ~ '^[0-9a-f]{64}$')
);

CREATE INDEX IF NOT EXISTS ix_koth_api_observations_cycle
    ON "KothApiObservations"(cycle_id, reset_attempt, target_id);

CREATE TABLE IF NOT EXISTS "KothApiRequestReplays" (
    request_hash BYTEA PRIMARY KEY,
    challenge_id INTEGER NOT NULL
        REFERENCES "KothApiObservers"(challenge_id)
        ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT ck_koth_api_request_replays_hash
        CHECK (OCTET_LENGTH(request_hash) = 32)
);

CREATE INDEX IF NOT EXISTS ix_koth_api_request_replays_expiry
    ON "KothApiRequestReplays"(expires_at);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "KothApiRequestReplays";
DROP TABLE IF EXISTS "KothApiObservations";
DROP TABLE IF EXISTS "KothApiObservers";
"#;

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
    fn observer_input_is_exact_context_bounded_and_replay_protected() {
        assert!(UP_SQL.contains("REFERENCES \"GameChallenges\"(game_id, id)"));
        assert!(UP_SQL.contains("REFERENCES \"KothTargets\"(id, challenge_id)"));
        assert!(UP_SQL.contains("REFERENCES \"KothCrownCycles\"(id, challenge_id)"));
        assert!(UP_SQL.contains("REFERENCES \"KothTokens\"(id, cycle_id)"));
        assert!(UP_SQL.contains("request_timestamp_ms BIGINT NOT NULL"));
        assert!(UP_SQL.contains("request_hash BYTEA PRIMARY KEY"));
        assert!(UP_SQL.contains("ix_koth_api_request_replays_expiry"));
    }
}
