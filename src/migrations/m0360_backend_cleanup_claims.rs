//! Bounded, independently retried end-of-event cleanup.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdTeamServices"
    ADD COLUMN IF NOT EXISTS cleanup_next_at TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    ADD COLUMN IF NOT EXISTS cleanup_lease_token UUID,
    ADD COLUMN IF NOT EXISTS cleanup_lease_until TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS cleanup_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE "KothTargets"
    ADD COLUMN IF NOT EXISTS cleanup_next_at TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    ADD COLUMN IF NOT EXISTS cleanup_lease_token UUID,
    ADD COLUMN IF NOT EXISTS cleanup_lease_until TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS cleanup_attempts INTEGER NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS ix_ad_services_cleanup_due
    ON "AdTeamServices" (cleanup_next_at, id)
    WHERE container_id IS NOT NULL OR host <> '';
CREATE INDEX IF NOT EXISTS ix_koth_targets_cleanup_due
    ON "KothTargets" (cleanup_next_at, id)
    WHERE container_id IS NOT NULL OR host <> '';
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
