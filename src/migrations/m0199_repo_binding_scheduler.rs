//! Durable repo-scan leases and coalesced push-back work.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS scan_lease_token UUID NULL;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS scan_lease_until TIMESTAMPTZ NULL;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS scan_started_at_utc TIMESTAMPTZ NULL;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS consecutive_scan_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE "RepoBindings" ALTER COLUMN consecutive_scan_failures SET DEFAULT 0;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS push_lease_token UUID NULL;
ALTER TABLE "RepoBindings" ADD COLUMN IF NOT EXISTS push_lease_until TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS ix_repobindings_due_scan
    ON "RepoBindings" (next_scan_utc, id)
    WHERE status = 0;

CREATE TABLE IF NOT EXISTS "RepoBindingPushJobs" (
    binding_id INTEGER NOT NULL REFERENCES "RepoBindings" (id) ON DELETE CASCADE,
    challenge_id INTEGER NOT NULL REFERENCES "GameChallenges" (id) ON DELETE CASCADE,
    game_id INTEGER NOT NULL REFERENCES "Games" (id) ON DELETE CASCADE,
    requested_revision BIGINT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (binding_id, challenge_id)
);

CREATE INDEX IF NOT EXISTS ix_repobindingpushjobs_claim
    ON "RepoBindingPushJobs" (updated_at_utc, binding_id, challenge_id);
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
