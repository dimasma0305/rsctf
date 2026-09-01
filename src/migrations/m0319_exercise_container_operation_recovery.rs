use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "ExerciseContainerOperations"
    ADD COLUMN IF NOT EXISTS runtime_started BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS backend_id TEXT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_exercise_container_operation_backend_id'
           AND conrelid = '"ExerciseContainerOperations"'::regclass
    ) THEN
        ALTER TABLE "ExerciseContainerOperations"
            ADD CONSTRAINT ck_exercise_container_operation_backend_id
            CHECK (backend_id IS NULL
                   OR octet_length(backend_id) BETWEEN 1 AND 512);
    END IF;
END $$;
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
    fn exercise_operations_distinguish_prelaunch_from_ambiguous_work() {
        assert!(UP_SQL.contains("runtime_started BOOLEAN NOT NULL DEFAULT FALSE"));
        assert!(UP_SQL.contains("backend_id TEXT NULL"));
        assert!(UP_SQL.contains("conrelid = '\"ExerciseContainerOperations\"'::regclass"));
    }
}
