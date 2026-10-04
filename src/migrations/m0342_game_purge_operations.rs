//! Durable, replay-safe authorization for explicitly enabled event-history purges.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "GamePurgeOperations" (
    operation_id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL,
    actor_user_id UUID NOT NULL,
    request_digest VARCHAR(64) NOT NULL
        CHECK (request_digest ~ '^[0-9a-f]{64}$'),
    expected_configuration_revision BIGINT NOT NULL
        CHECK (expected_configuration_revision >= 0),
    confirmation_title TEXT NOT NULL,
    status SMALLINT NOT NULL DEFAULT 0 CHECK (status IN (0, 1)),
    result JSONB,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ,
    CONSTRAINT ck_gamepurgeoperations_terminal CHECK (
        (status = 0 AND result IS NULL AND completed_at_utc IS NULL)
        OR (status = 1 AND result IS NOT NULL AND completed_at_utc IS NOT NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_gamepurgeoperations_active_game
    ON "GamePurgeOperations" (game_id)
    WHERE status = 0;
CREATE INDEX IF NOT EXISTS ix_gamepurgeoperations_retention
    ON "GamePurgeOperations" (completed_at_utc, operation_id)
    WHERE status = 1;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: deployed operation state must remain replayable across
        // ordinary binary rollbacks for the configured retention window.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn purge_intent_survives_game_deletion_and_has_one_active_owner() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(!UP_SQL.contains("REFERENCES \"Games\""));
        assert!(UP_SQL.contains("ux_gamepurgeoperations_active_game"));
        assert!(UP_SQL.contains("WHERE status = 0"));
        assert!(UP_SQL.contains("ck_gamepurgeoperations_terminal"));
        assert!(UP_SQL.contains("ix_gamepurgeoperations_retention"));
    }
}
