//! Indexes for bounded monitor event/submission paging and normalized search.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
-- pg_trgm is a trusted PostgreSQL extension. It keeps literal contains-searches
-- indexed without changing the established monitor search semantics.
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;

CREATE INDEX IF NOT EXISTS ix_gameevents_monitor_page
    ON "GameEvents" (game_id, publish_time_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_gameevents_game_user
    ON "GameEvents" (game_id, user_id)
    WHERE user_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_submissions_monitor_status_page
    ON "Submissions" (game_id, status, submit_time_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_submissions_game_team
    ON "Submissions" (game_id, team_id);
CREATE INDEX IF NOT EXISTS ix_submissions_game_user
    ON "Submissions" (game_id, user_id)
    WHERE user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_submissions_game_challenge
    ON "Submissions" (game_id, challenge_id);

CREATE INDEX IF NOT EXISTS ix_teams_monitor_name_trgm
    ON "Teams" USING GIN (LOWER(name) gin_trgm_ops);
CREATE INDEX IF NOT EXISTS ix_users_monitor_name_trgm
    ON "AspNetUsers" USING GIN (LOWER(user_name) gin_trgm_ops)
    WHERE user_name IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_challenges_monitor_title_trgm
    ON "GameChallenges" USING GIN (LOWER(title) gin_trgm_ops);
CREATE INDEX IF NOT EXISTS ix_submissions_monitor_answer_trgm
    ON "Submissions" USING GIST (LOWER(answer) gist_trgm_ops(siglen=64));
CREATE INDEX IF NOT EXISTS ix_gameevents_monitor_values_trgm
    ON "GameEvents" USING GIST (LOWER(values::text) gist_trgm_ops(siglen=64));
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_gameevents_monitor_values_trgm;
DROP INDEX IF EXISTS ix_submissions_monitor_answer_trgm;
DROP INDEX IF EXISTS ix_challenges_monitor_title_trgm;
DROP INDEX IF EXISTS ix_users_monitor_name_trgm;
DROP INDEX IF EXISTS ix_teams_monitor_name_trgm;
DROP INDEX IF EXISTS ix_submissions_game_challenge;
DROP INDEX IF EXISTS ix_submissions_game_user;
DROP INDEX IF EXISTS ix_submissions_game_team;
DROP INDEX IF EXISTS ix_submissions_monitor_status_page;
DROP INDEX IF EXISTS ix_gameevents_game_user;
DROP INDEX IF EXISTS ix_gameevents_monitor_page;
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
    fn migration_is_forward_idempotent_and_keeps_extension_on_down() {
        assert!(UP_SQL.contains("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public"));
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 11);
        assert!(UP_SQL.contains("(game_id, publish_time_utc DESC, id DESC)"));
        assert!(UP_SQL.contains("(game_id, status, submit_time_utc DESC, id DESC)"));
        assert!(UP_SQL.contains("gin_trgm_ops"));
        assert!(UP_SQL.contains("gist_trgm_ops(siglen=64)"));
        assert!(!DOWN_SQL.contains("DROP EXTENSION"));
    }
}
