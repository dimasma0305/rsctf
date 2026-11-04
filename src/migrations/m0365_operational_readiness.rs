//! Preserve scoring exclusions and distinguish expected import fences from failures.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "KothOfficialConfigs"
  ADD COLUMN IF NOT EXISTS excluded_hills_snapshot JSONB NOT NULL DEFAULT '[]'::jsonb;
ALTER TABLE "RepoBindings"
  ADD COLUMN IF NOT EXISTS scan_outcome TEXT NOT NULL DEFAULT 'NoChange',
  ADD COLUMN IF NOT EXISTS blocked_commit_sha TEXT,
  ADD COLUMN IF NOT EXISTS scan_blocked_game_ids INTEGER[] NOT NULL DEFAULT '{}';
ALTER TABLE "RepoBindingScans"
  ADD COLUMN IF NOT EXISTS outcome TEXT NOT NULL DEFAULT 'Success';
-- Initial SeaORM migrations construct tables from the current entity models.
-- Apply defaults even when a fresh install already created these columns.
ALTER TABLE "RepoBindings"
  ALTER COLUMN scan_outcome SET DEFAULT 'NoChange',
  ALTER COLUMN scan_blocked_game_ids SET DEFAULT '{}';
ALTER TABLE "RepoBindingScans" ALTER COLUMN outcome SET DEFAULT 'Success';
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
