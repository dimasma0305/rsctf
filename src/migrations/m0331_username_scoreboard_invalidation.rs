//! User-first lookup for post-commit solver-name scoreboard invalidation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_submissions_accepted_user_game
    ON "Submissions" (user_id, game_id)
    WHERE status = 1;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_submissions_accepted_user_game;
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
    use super::UP_SQL;

    #[test]
    fn accepted_solver_lookup_is_user_first_and_partial() {
        assert!(UP_SQL.contains("(user_id, game_id)"));
        assert!(UP_SQL.contains("WHERE status = 1"));
    }
}
