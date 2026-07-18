//! Adds the `AntiCheatBlocks` table (login IP/fingerprint conflict records).
//! Idempotent, runs on existing databases.
use crate::models::data::anti_cheat_block;
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        let mut stmt = schema.create_table_from_entity(anti_cheat_block::Entity);
        stmt.if_not_exists();
        manager.create_table(stmt).await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("AntiCheatBlocks"))
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
