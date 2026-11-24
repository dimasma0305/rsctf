use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdPatchCandidates"
    ADD COLUMN IF NOT EXISTS manifest_digest TEXT
        CHECK (manifest_digest ~ '^sha256:[0-9a-f]{64}$'),
    ADD COLUMN IF NOT EXISTS config_digest TEXT
        CHECK ((config_digest IS NULL) = (manifest_digest IS NULL)
               AND config_digest ~ '^sha256:[0-9a-f]{64}$');
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
