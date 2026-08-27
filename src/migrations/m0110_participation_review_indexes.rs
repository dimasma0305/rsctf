//! Cover bounded participation review filters and per-row registration counts.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_participations_review_filter
    ON "Participations" (game_id, status, division_id, team_id, id);

CREATE INDEX IF NOT EXISTS ix_userparticipations_review_count
    ON "UserParticipations" (game_id, participation_id, team_id, user_id);
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_userparticipations_review_count;
DROP INDEX IF EXISTS ix_participations_review_filter;
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
    fn review_indexes_are_idempotent_and_cover_filter_and_registration_lookups() {
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 2);
        assert!(
            UP_SQL.contains("ON \"Participations\" (game_id, status, division_id, team_id, id)")
        );
        assert!(UP_SQL
            .contains("ON \"UserParticipations\" (game_id, participation_id, team_id, user_id)"));
    }
}
