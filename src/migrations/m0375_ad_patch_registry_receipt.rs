use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "AdPatchCandidates" ADD COLUMN IF NOT EXISTS registry_image_ref TEXT
    CHECK (registry_image_ref IS NULL OR (
        manifest_digest IS NOT NULL AND config_digest IS NOT NULL
        AND octet_length(registry_image_ref) BETWEEN 73 AND 512
        AND right(registry_image_ref, length('/service-' || service_id || '@' || manifest_digest))
            = '/service-' || service_id || '@' || manifest_digest
    ));
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
