//! Normalize unsupported container network modes and constrain future writes.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
UPDATE "GameChallenges"
   SET network_mode = 0
 WHERE network_mode IS NOT NULL
   AND (
       network_mode NOT IN (0, 32)
       OR ("Type" IN (4, 5) AND network_mode = 32)
   );

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_network_mode'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_network_mode
            CHECK (
                network_mode IS NULL
                OR (
                    network_mode IN (0, 32)
                    AND NOT ("Type" IN (4, 5) AND network_mode = 32)
                )
            );
    END IF;
END $$;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_network_mode;
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
    fn migration_repairs_unsupported_modes_before_constraining_them() {
        assert!(UP_SQL.contains("network_mode NOT IN (0, 32)"));
        assert!(UP_SQL.contains("\"Type\" IN (4, 5)"));
        assert!(UP_SQL.contains("ck_game_challenges_network_mode"));
    }
}
