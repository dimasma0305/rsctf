//! Revision fences for explicit A&D/KotH desired-state commands.

use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS ad_control_revision BIGINT;
UPDATE "Games"
   SET ad_control_revision = 1
 WHERE ad_control_revision IS NULL;
ALTER TABLE "Games"
    ALTER COLUMN ad_control_revision SET DEFAULT 1,
    ALTER COLUMN ad_control_revision SET NOT NULL;

ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS ad_control_revision BIGINT;
UPDATE "GameChallenges"
   SET ad_control_revision = 1
 WHERE ad_control_revision IS NULL;
ALTER TABLE "GameChallenges"
    ALTER COLUMN ad_control_revision SET DEFAULT 1,
    ALTER COLUMN ad_control_revision SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_games_ad_control_revision'
    ) THEN
        ALTER TABLE "Games"
            ADD CONSTRAINT ck_games_ad_control_revision
            CHECK (ad_control_revision BETWEEN 1 AND 9007199254740991);
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_ad_control_revision'
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_ad_control_revision
            CHECK (ad_control_revision BETWEEN 1 AND 9007199254740991);
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

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_ad_control_revision,
    DROP COLUMN IF EXISTS ad_control_revision;
ALTER TABLE "Games"
    DROP CONSTRAINT IF EXISTS ck_games_ad_control_revision,
    DROP COLUMN IF EXISTS ad_control_revision;
"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn desired_state_commands_have_independent_revision_fences() {
        assert!(UP_SQL.contains("Games\"\n    ADD COLUMN IF NOT EXISTS ad_control_revision"));
        assert!(
            UP_SQL.contains("GameChallenges\"\n    ADD COLUMN IF NOT EXISTS ad_control_revision")
        );
        assert_eq!(
            UP_SQL
                .matches("ALTER COLUMN ad_control_revision SET DEFAULT 1")
                .count(),
            2
        );
        assert_eq!(
            UP_SQL
                .matches("ALTER COLUMN ad_control_revision SET NOT NULL")
                .count(),
            2
        );
        assert!(UP_SQL.contains("BETWEEN 1 AND 9007199254740991"));
    }
}
