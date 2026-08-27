//! Ordered access path for bounded public post feeds.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_posts_feed_order
    ON "Posts" (is_pinned DESC, update_time_utc DESC, id DESC);
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_posts_feed_order;
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
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DOWN_SQL, UP_SQL};

    #[test]
    fn post_feed_index_is_idempotent_and_matches_the_wire_order() {
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS ix_posts_feed_order"));
        assert!(UP_SQL.contains("is_pinned DESC, update_time_utc DESC, id DESC"));
        assert!(DOWN_SQL.contains("DROP INDEX IF EXISTS ix_posts_feed_order"));
    }
}
