//! Short-lived, lifecycle-bound credentials for KotH targets that report their
//! own bounded Leaderboard evidence.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "KothTargetReporters" (
    cycle_id BIGINT PRIMARY KEY,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    reset_attempt INTEGER NOT NULL,
    hmac_secret TEXT NOT NULL,
    issued_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ NULL,
    CONSTRAINT fk_koth_target_reporters_cycle
        FOREIGN KEY (cycle_id, challenge_id)
        REFERENCES "KothCrownCycles"(id, challenge_id)
        ON DELETE CASCADE,
    CONSTRAINT fk_koth_target_reporters_challenge
        FOREIGN KEY (game_id, challenge_id)
        REFERENCES "GameChallenges"(game_id, id)
        ON DELETE CASCADE,
    CONSTRAINT ck_koth_target_reporters_attempt
        CHECK (reset_attempt >= 0),
    CONSTRAINT ck_koth_target_reporters_secret
        CHECK (
            OCTET_LENGTH(hmac_secret) BETWEEN 48 AND 128
            AND hmac_secret LIKE 'koth_target_%'
        )
);

CREATE INDEX IF NOT EXISTS ix_koth_target_reporters_scope
    ON "KothTargetReporters"(game_id, challenge_id, cycle_id, reset_attempt);
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "KothTargetReporters";
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
    fn reporter_credentials_are_cycle_scoped_bounded_and_cascaded() {
        assert!(UP_SQL.contains("cycle_id BIGINT PRIMARY KEY"));
        assert!(UP_SQL.contains("FOREIGN KEY (cycle_id, challenge_id)"));
        assert!(UP_SQL.contains("REFERENCES \"KothCrownCycles\"(id, challenge_id)"));
        assert!(UP_SQL.contains("REFERENCES \"GameChallenges\"(game_id, id)"));
        assert!(UP_SQL.contains("reset_attempt >= 0"));
        assert!(UP_SQL.contains("hmac_secret LIKE 'koth_target_%'"));
        assert!(UP_SQL.contains("expires_at TIMESTAMPTZ NOT NULL"));
    }
}
