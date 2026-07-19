//! Restore KotH crown-cycle defaults on both upgraded and pristine databases.

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
                r#"ALTER TABLE "Games"
                     ALTER COLUMN koth_epoch_ticks SET DEFAULT 12,
                     ALTER COLUMN koth_cycle_ticks SET DEFAULT 3,
                     ALTER COLUMN koth_champion_cooldown_ticks SET DEFAULT 1,
                     ALTER COLUMN koth_claim_confirmation_ticks SET DEFAULT 2;"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // These defaults existed before this corrective migration. Removing
        // them on downgrade would make the schema less safe than it started.
        Ok(())
    }
}
