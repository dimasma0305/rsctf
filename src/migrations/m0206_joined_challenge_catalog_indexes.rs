//! Keep joined-challenge discovery on accepted memberships and active rows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_participations_accepted_game_id
    ON "Participations" (game_id, id, division_id)
    WHERE status = 1;
CREATE INDEX IF NOT EXISTS ix_gamechallenges_active_catalog
    ON "GameChallenges" (game_id, category, "Type", id)
    WHERE is_enabled = TRUE AND review_status = 1;
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
    fn catalog_indexes_are_partial_and_idempotent() {
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 2);
        assert!(UP_SQL.contains("WHERE status = 1"));
        assert!(UP_SQL.contains("WHERE is_enabled = TRUE AND review_status = 1"));
    }
}
