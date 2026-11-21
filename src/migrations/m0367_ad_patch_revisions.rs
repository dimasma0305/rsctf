use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdPatchSourceUsage" (
    service_id INTEGER PRIMARY KEY REFERENCES "AdTeamServices"(id) ON DELETE CASCADE,
    used_bytes BIGINT NOT NULL CHECK (used_bytes BETWEEN 0 AND 536870912)
);
CREATE TABLE IF NOT EXISTS "AdPatchSourceBlobs" (
    service_id INTEGER NOT NULL REFERENCES "AdTeamServices"(id) ON DELETE CASCADE,
    oid TEXT NOT NULL CHECK (oid ~ '^[0-9a-f]{64}$'),
    deflated BOOLEAN NOT NULL,
    raw_size INTEGER NOT NULL CHECK (raw_size BETWEEN 0 AND 67108864),
    payload BYTEA NOT NULL CHECK (octet_length(payload) <= 75497472),
    PRIMARY KEY (service_id, oid)
);
CREATE TABLE IF NOT EXISTS "AdPatchSourceTrees" (
    service_id INTEGER NOT NULL REFERENCES "AdTeamServices"(id) ON DELETE CASCADE,
    tree_id TEXT NOT NULL CHECK (tree_id ~ '^[0-9a-f]{64}$'),
    manifest TEXT NOT NULL,
    PRIMARY KEY (service_id, tree_id)
);
CREATE TABLE IF NOT EXISTS "AdPatchRevisions" (
    service_id INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 4096),
    tree_id TEXT NOT NULL,
    parent_id TEXT NOT NULL CHECK (parent_id ~ '^[0-9a-f]{64}$'),
    commit_id TEXT NOT NULL CHECK (commit_id ~ '^[0-9a-f]{64}$'),
    actor_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (service_id, revision),
    FOREIGN KEY (service_id, tree_id) REFERENCES "AdPatchSourceTrees"(service_id, tree_id) ON DELETE CASCADE
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
