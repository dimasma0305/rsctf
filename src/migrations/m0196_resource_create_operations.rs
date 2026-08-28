//! Bounded exact-replay ledger for team, game, and post creation.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "ResourceCreateOperations" (
    actor_id UUID NOT NULL,
    resource_kind TEXT NOT NULL,
    scope_id INTEGER NOT NULL DEFAULT 0,
    operation_id UUID NOT NULL,
    request_digest TEXT NOT NULL,
    result_id TEXT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    completed_at_utc TIMESTAMPTZ NULL,
    PRIMARY KEY (actor_id, resource_kind, scope_id, operation_id),
    CONSTRAINT ck_resourcecreateoperations_kind
        CHECK (resource_kind IN ('team', 'game', 'post')),
    CONSTRAINT ck_resourcecreateoperations_digest
        CHECK (char_length(request_digest) = 64)
);

CREATE INDEX IF NOT EXISTS ix_resourcecreateoperations_actor_kind_created
    ON "ResourceCreateOperations" (actor_id, resource_kind, created_at_utc DESC);
CREATE INDEX IF NOT EXISTS ix_resourcecreateoperations_retention
    ON "ResourceCreateOperations" (created_at_utc);
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
