//! Durable exact-result ledger for ordinary challenge-definition updates.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ChallengeUpdateOperations" (
    operation_id UUID PRIMARY KEY,
    actor_id UUID NOT NULL,
    game_id INTEGER NOT NULL,
    challenge_id INTEGER NOT NULL,
    request_digest TEXT NOT NULL,
    expected_revision BIGINT NOT NULL,
    result_revision BIGINT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ NULL,
    CONSTRAINT ck_challengeupdateoperations_result CHECK (
        (result_revision IS NULL) = (completed_at_utc IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS ix_challengeupdateoperations_actor_created
    ON "ChallengeUpdateOperations" (actor_id, created_at_utc DESC);
CREATE INDEX IF NOT EXISTS ix_challengeupdateoperations_scope
    ON "ChallengeUpdateOperations" (actor_id, game_id, challenge_id, operation_id);
CREATE INDEX IF NOT EXISTS ix_challengeupdateoperations_retention
    ON "ChallengeUpdateOperations" (created_at_utc);
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

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn operation_identity_is_global_and_result_is_terminal() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("ck_challengeupdateoperations_result"));
        assert!(UP_SQL.contains("actor_id, game_id, challenge_id, operation_id"));
    }
}
