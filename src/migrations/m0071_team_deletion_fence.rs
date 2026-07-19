//! Add a durable cross-replica fence for multi-stage team deletion.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE "Teams"
                   ADD COLUMN IF NOT EXISTS deletion_pending BOOLEAN;
                   UPDATE "Teams"
                      SET deletion_pending = FALSE
                    WHERE deletion_pending IS NULL;
                   ALTER TABLE "Teams"
                     ALTER COLUMN deletion_pending SET DEFAULT FALSE,
                     ALTER COLUMN deletion_pending SET NOT NULL;"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"ALTER TABLE "Teams" DROP COLUMN IF EXISTS deletion_pending;"#)
            .await?;
        Ok(())
    }
}
