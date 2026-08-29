//! Bounded lookup paths for the legacy standalone exercise API.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_flagcontexts_exercise_flag_eligibility
    ON "FlagContexts"(exercise_id, flag, is_occupied, id)
    WHERE exercise_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_exercisechallenges_published_catalog
    ON "ExerciseChallenges"(publish_time_utc, id)
    WHERE is_enabled = TRUE;
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
    use super::*;

    #[test]
    fn exercise_indexes_are_forward_safe_and_bounded() {
        assert!(UP_SQL.contains("IF NOT EXISTS"));
        assert!(UP_SQL.contains("(exercise_id, flag, is_occupied, id)"));
        assert!(UP_SQL.contains("WHERE is_enabled = TRUE"));
    }
}
