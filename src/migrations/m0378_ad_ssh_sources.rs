use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdPatchStarters" ADD COLUMN IF NOT EXISTS ssh_sources JSONB NOT NULL
    DEFAULT '[]'::jsonb CHECK (jsonb_typeof(ssh_sources)='array'
        AND jsonb_array_length(ssh_sources)<=32 AND octet_length(ssh_sources::text)<=32768);
CREATE TABLE IF NOT EXISTS "AdHostedSshSourceHistory" (
    service_id INTEGER NOT NULL REFERENCES "AdTeamServices"(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL CHECK (revision > 0),
    first_seen TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    last_seen TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    state TEXT NOT NULL CHECK (state IN ('observed','unavailable')),
    files TEXT NOT NULL CHECK (octet_length(files) <= 65536),
    PRIMARY KEY (service_id, revision)
);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
