//! Durable, cross-replica ownership for legacy exercise container mutations.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ExerciseContainerOperations" (
    operation_id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
    exercise_id INTEGER NOT NULL
        REFERENCES "ExerciseChallenges"(id) ON DELETE CASCADE,
    intent VARCHAR(8) NOT NULL CHECK (intent IN ('Create', 'Delete')),
    publication_id UUID NOT NULL,
    state VARCHAR(16) NOT NULL CHECK (state IN ('Running', 'Succeeded', 'Failed')),
    result JSONB NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_expires_at_utc TIMESTAMPTZ NOT NULL,
    CHECK ((state = 'Succeeded') = (result IS NOT NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_exercise_container_operation_active_user
    ON "ExerciseContainerOperations" (user_id)
    WHERE state = 'Running';
CREATE INDEX IF NOT EXISTS ix_exercise_container_operation_expiry
    ON "ExerciseContainerOperations" (lease_expires_at_utc, operation_id)
    WHERE state = 'Running';
CREATE INDEX IF NOT EXISTS ix_exercise_container_operation_retention
    ON "ExerciseContainerOperations" (updated_at_utc, operation_id)
    WHERE state <> 'Running';
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
    fn exact_operations_have_one_bounded_user_owner() {
        assert!(UP_SQL.contains("operation_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("ux_exercise_container_operation_active_user"));
        assert!(UP_SQL.contains("WHERE state = 'Running'"));
        assert!(UP_SQL.contains("publication_id UUID NOT NULL"));
        assert!(UP_SQL.contains("(state = 'Succeeded') = (result IS NOT NULL)"));
    }
}
