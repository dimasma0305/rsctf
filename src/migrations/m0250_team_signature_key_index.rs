//! Anchor public team-signature verification without scanning the game catalog.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_games_public_key_signature_lookup
    ON "Games" (public_key, id)
    INCLUDE (start_time_utc, end_time_utc);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only production migration. Retaining the lookup index is safe
        // for older binaries and avoids a table scan after a rollback.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn lookup_index_is_idempotent_and_covers_the_live_window() {
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS"));
        assert!(UP_SQL.contains("ON \"Games\" (public_key, id)"));
        assert!(UP_SQL.contains("INCLUDE (start_time_utc, end_time_utc)"));
    }
}
