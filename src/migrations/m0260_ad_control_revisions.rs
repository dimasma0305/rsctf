//! Revision fences for explicit A&D/KotH desired-state commands.

use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "Games"
    ADD COLUMN IF NOT EXISTS ad_control_revision BIGINT NOT NULL DEFAULT 1;

ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS ad_control_revision BIGINT NOT NULL DEFAULT 1;

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

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0260_ad_control_revisions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Production migrations are forward-only; retain revision fences.
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
        assert!(UP_SQL.contains("BETWEEN 1 AND 9007199254740991"));
    }
}
