//! Bound the mutation-driven account statistics, current-user team lists, and
//! joined-event challenge catalog to their selective ownership predicates.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_submissions_user_accepted_stats
    ON "Submissions" (user_id, game_id, challenge_id)
    WHERE user_id IS NOT NULL AND status = 1;

CREATE INDEX IF NOT EXISTS ix_firstsolves_submission_stats
    ON "FirstSolves" (submission_id);

CREATE INDEX IF NOT EXISTS ix_teams_captain_active
    ON "Teams" (captain_id, id)
    WHERE deletion_pending = FALSE;

CREATE INDEX IF NOT EXISTS ix_gamechallenges_player_catalog
    ON "GameChallenges" (game_id, id)
    WHERE is_enabled = TRUE AND deletion_pending = FALSE AND review_status = 0;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_gamechallenges_player_catalog;
DROP INDEX IF EXISTS ix_teams_captain_active;
DROP INDEX IF EXISTS ix_firstsolves_submission_stats;
DROP INDEX IF EXISTS ix_submissions_user_accepted_stats;
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
    fn player_read_indexes_are_idempotent_and_partial() {
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 4);
        assert!(UP_SQL.contains("user_id, game_id, challenge_id"));
        assert!(UP_SQL.contains("WHERE user_id IS NOT NULL AND status = 1"));
        assert!(UP_SQL.contains("ix_firstsolves_submission_stats"));
        assert!(UP_SQL.contains("ON \"FirstSolves\" (submission_id)"));
        assert!(UP_SQL.contains("WHERE deletion_pending = FALSE"));
        assert!(UP_SQL.contains(
            "WHERE is_enabled = TRUE AND deletion_pending = FALSE AND review_status = 0"
        ));
    }
}
