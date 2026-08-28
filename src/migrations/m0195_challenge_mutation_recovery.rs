//! Atomic, replayable challenge definition mutations.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS revision BIGINT NOT NULL DEFAULT 1;
-- Fresh installations derive m0001 from the current entity, so the column may
-- already exist without the migration default when this forward step runs.
ALTER TABLE "GameChallenges" ALTER COLUMN revision SET DEFAULT 1;

CREATE TABLE IF NOT EXISTS "ChallengeCreateOperations" (
    actor_id UUID NOT NULL,
    game_id INTEGER NOT NULL,
    operation_id UUID NOT NULL,
    request_digest TEXT NOT NULL,
    challenge_id INTEGER NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ NULL,
    PRIMARY KEY (actor_id, game_id, operation_id)
);

CREATE INDEX IF NOT EXISTS ix_challengecreateoperations_actor_created
    ON "ChallengeCreateOperations" (actor_id, created_at_utc DESC);
CREATE INDEX IF NOT EXISTS ix_challengecreateoperations_retention
    ON "ChallengeCreateOperations" (created_at_utc);
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
