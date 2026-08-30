//! Replay-safe, bounded game clone operations.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "GameCloneOperations" (
    operation_id UUID PRIMARY KEY,
    source_game_id INTEGER NOT NULL REFERENCES "Games" (id) ON DELETE CASCADE,
    requested_by UUID NOT NULL REFERENCES "AspNetUsers" (id) ON DELETE CASCADE,
    request_digest VARCHAR(64) NOT NULL,
    source_revision VARCHAR(64) NOT NULL,
    destination_game_id INTEGER REFERENCES "Games" (id) ON DELETE SET NULL,
    status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
    error_message VARCHAR(512),
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ,
    CONSTRAINT ck_gamecloneoperations_terminal CHECK (
        (status = 1 AND completed_at_utc IS NOT NULL)
        OR status <> 1
    )
);

CREATE INDEX IF NOT EXISTS ix_gamecloneoperations_source_created
    ON "GameCloneOperations" (source_game_id, created_at_utc DESC);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: operation identity and replay results are durable state.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_intent_and_result_are_durable() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("request_digest VARCHAR(64) NOT NULL"));
        assert!(UP_SQL.contains("source_revision VARCHAR(64) NOT NULL"));
        assert!(UP_SQL.contains("destination_game_id INTEGER"));
    }
}
