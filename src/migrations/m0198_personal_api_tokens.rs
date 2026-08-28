//! Make managed personal/admin API tokens a real, fenced credential class.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "ApiTokens"
    ADD COLUMN IF NOT EXISTS audience TEXT NOT NULL DEFAULT 'admin_api';
ALTER TABLE "ApiTokens" ALTER COLUMN audience SET DEFAULT 'admin_api';
ALTER TABLE "ApiTokens"
    ADD COLUMN IF NOT EXISTS security_stamp_hash TEXT NULL;

-- Legacy secrets had no grammar or security-stamp fence and never authenticated.
-- Retire them explicitly; replacement secrets are revealed only on a new create.
UPDATE "ApiTokens"
   SET is_revoked = TRUE
 WHERE security_stamp_hash IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS ux_apitokens_token_hash
    ON "ApiTokens" (token_hash)
    WHERE is_revoked = FALSE;
CREATE INDEX IF NOT EXISTS ix_apitokens_creator_created
    ON "ApiTokens" (creator_id, created_at DESC);
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
