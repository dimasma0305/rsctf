use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "AnonymousImageCleanupClaims" (
    installation_scope TEXT NOT NULL,
    canonical_ref TEXT NOT NULL,
    claim_token UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (installation_scope, canonical_ref)
);
CREATE INDEX IF NOT EXISTS ix_anonymous_image_cleanup_expiry
    ON "AnonymousImageCleanupClaims" (installation_scope, expires_at);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP TABLE IF EXISTS "AnonymousImageCleanupClaims""#)
            .await?;
        Ok(())
    }
}
