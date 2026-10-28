//! Index durable teardown fences, including crash recovery after destruction.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_ad_services_cleanup_pending
    ON "AdTeamServices" (game_id, cleanup_next_at, id)
    WHERE cleanup_lease_token IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_koth_targets_cleanup_pending
    ON "KothTargets" (game_id, cleanup_next_at, id)
    WHERE cleanup_lease_token IS NOT NULL;
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
