//! Repair and constrain legacy per-challenge writable-layer limits.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
UPDATE "GameChallenges"
   SET storage_limit = 512
 WHERE storage_limit IS NOT NULL
   AND (storage_limit < 1 OR storage_limit > 1048576);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_storage_limit'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_storage_limit
            CHECK (
                storage_limit IS NULL
                OR storage_limit BETWEEN 1 AND 1048576
            );
    END IF;
END $$;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_storage_limit;
"#;

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
    use super::*;

    #[test]
    fn migration_repairs_rows_before_adding_the_bound() {
        assert!(UP_SQL.contains("SET storage_limit = 512"));
        assert!(UP_SQL.contains("storage_limit BETWEEN 1 AND 1048576"));
    }
}
