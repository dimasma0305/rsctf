//! Organizer-selected original ZIP for hosted A&D patch revisions.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdPatchStarters" (
    challenge_id INTEGER PRIMARY KEY REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    tree_id TEXT NOT NULL CHECK (tree_id ~ '^[0-9a-f]{64}$'),
    archive BYTEA NOT NULL CHECK (octet_length(archive) BETWEEN 1 AND 75497472),
    archive_bytes INTEGER GENERATED ALWAYS AS (octet_length(archive)) STORED
);
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
