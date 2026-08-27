//! Index the four capped temporal-edge candidates behind the public recent-games poll.
//!
//! Opposite edge directions need separate indexes because `id ASC` is the stable
//! tie-breaker in both directions; a backward scan would reverse that tie order.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_games_visible_ended_edge
    ON "Games" (end_time_utc DESC, id ASC)
    INCLUDE (start_time_utc)
    WHERE hidden = FALSE;

CREATE INDEX IF NOT EXISTS ix_games_visible_upcoming_edge
    ON "Games" (start_time_utc ASC, id ASC)
    INCLUDE (end_time_utc)
    WHERE hidden = FALSE;

CREATE INDEX IF NOT EXISTS ix_games_visible_active_start_edge
    ON "Games" (start_time_utc DESC, id ASC)
    INCLUDE (end_time_utc)
    WHERE hidden = FALSE;

CREATE INDEX IF NOT EXISTS ix_games_visible_active_end_edge
    ON "Games" (end_time_utc ASC, id ASC)
    INCLUDE (start_time_utc)
    WHERE hidden = FALSE;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_games_visible_active_end_edge;
DROP INDEX IF EXISTS ix_games_visible_active_start_edge;
DROP INDEX IF EXISTS ix_games_visible_upcoming_edge;
DROP INDEX IF EXISTS ix_games_visible_ended_edge;
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
    fn indexes_cover_every_exact_edge_direction_and_remain_partial() {
        for fragment in [
            "end_time_utc DESC, id ASC",
            "start_time_utc ASC, id ASC",
            "start_time_utc DESC, id ASC",
            "end_time_utc ASC, id ASC",
        ] {
            assert!(UP_SQL.contains(fragment), "missing edge index: {fragment}");
        }
        assert_eq!(UP_SQL.matches("WHERE hidden = FALSE").count(), 4);
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 4);
    }
}
