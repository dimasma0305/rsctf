//! Preserve organizer schedule edits and the reconciliation boundary they reopen.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "GameScheduleChanges" (
    operation_id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games"(id) ON DELETE CASCADE,
    actor_user_id UUID NOT NULL,
    configuration_revision BIGINT NOT NULL,
    previous_start TIMESTAMPTZ NOT NULL,
    previous_end TIMESTAMPTZ NOT NULL,
    requested_start TIMESTAMPTZ NOT NULL,
    requested_end TIMESTAMPTZ NOT NULL,
    previous_evidence_closed_at TIMESTAMPTZ,
    previous_sealed_at TIMESTAMPTZ,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (game_id, configuration_revision),
    CHECK (requested_end > requested_start)
);
CREATE INDEX IF NOT EXISTS ix_game_schedule_changes_history
    ON "GameScheduleChanges" (game_id, changed_at);
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
