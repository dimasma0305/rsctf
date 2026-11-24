use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AdSharedPatchChanges" (
    service_id INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    changes JSONB NOT NULL CHECK (jsonb_typeof(changes)='array' AND jsonb_array_length(changes) BETWEEN 1 AND 32),
    published BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY(service_id,revision),
    FOREIGN KEY(service_id,revision) REFERENCES "AdPatchRevisions"(service_id,revision) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS ix_ad_shared_patch_published ON "AdSharedPatchChanges"(service_id,revision) WHERE published;
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
