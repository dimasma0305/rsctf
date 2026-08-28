use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "PlayerContainerOperations" (
    operation_id UUID PRIMARY KEY,
    scope_key TEXT NOT NULL,
    actor_user_id UUID NOT NULL,
    game_id INTEGER NOT NULL,
    participation_id INTEGER NULL,
    challenge_id INTEGER NOT NULL,
    intent VARCHAR(16) NOT NULL CHECK (intent IN ('Create', 'Delete', 'Extend')),
    publication_id UUID NOT NULL,
    state VARCHAR(16) NOT NULL CHECK (state IN ('Running', 'Succeeded', 'Failed')),
    result JSONB NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_expires_at_utc TIMESTAMPTZ NOT NULL,
    CHECK ((state = 'Succeeded') = (result IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_player_container_operation_active_scope
    ON "PlayerContainerOperations" (scope_key)
    WHERE state = 'Running';
CREATE INDEX IF NOT EXISTS ix_player_container_operation_expiry
    ON "PlayerContainerOperations" (lease_expires_at_utc, operation_id)
    WHERE state = 'Running';
CREATE INDEX IF NOT EXISTS ix_player_container_operation_result_expiry
    ON "PlayerContainerOperations" (updated_at_utc, operation_id)
    WHERE state <> 'Running';
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
    fn one_active_intent_per_scope_has_recoverable_identity() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("publication_id UUID NOT NULL"));
        assert!(UP_SQL.contains("ux_player_container_operation_active_scope"));
        assert!(UP_SQL.contains("WHERE state = 'Running'"));
        assert!(UP_SQL.contains("(state = 'Succeeded') = (result IS NOT NULL)"));
    }
}
