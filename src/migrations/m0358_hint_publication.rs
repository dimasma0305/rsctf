//! Adds an explicit organizer-controlled publication boundary for challenge hints.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS released_hint_count INTEGER;

UPDATE "GameChallenges"
   SET released_hint_count = CASE
       WHEN is_enabled AND jsonb_typeof(hints) = 'array' THEN jsonb_array_length(hints)
       ELSE 0
   END
 WHERE released_hint_count IS NULL;

ALTER TABLE "GameChallenges"
    ALTER COLUMN released_hint_count SET DEFAULT 0,
    ALTER COLUMN released_hint_count SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_released_hint_count'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_released_hint_count
            CHECK (released_hint_count >= 0);
    END IF;
END $$;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_released_hint_count;
ALTER TABLE "GameChallenges"
    DROP COLUMN IF EXISTS released_hint_count;
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
    fn live_hints_remain_published_while_staged_and_new_hints_are_drafts() {
        assert!(UP_SQL.contains("SET DEFAULT 0"));
        assert!(UP_SQL.contains("WHERE released_hint_count IS NULL"));
        assert!(UP_SQL.contains("WHEN is_enabled AND"));
        assert!(UP_SQL.contains("jsonb_array_length(hints)"));
        assert!(UP_SQL.contains("released_hint_count >= 0"));
    }
}
