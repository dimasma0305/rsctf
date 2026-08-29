//! Bound account-stat aggregation to the caller's accepted solve projection.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_submissions_user_accepted_game_challenge
    ON "Submissions" (user_id, game_id, challenge_id, id)
    WHERE user_id IS NOT NULL AND status = 1;
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
    use super::UP_SQL;

    #[test]
    fn accepted_account_projection_is_partial_and_idempotent() {
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS"));
        assert!(UP_SQL.contains("(user_id, game_id, challenge_id, id)"));
        assert!(UP_SQL.contains("WHERE user_id IS NOT NULL AND status = 1"));
        assert!(!UP_SQL.contains("answer"));
    }
}
