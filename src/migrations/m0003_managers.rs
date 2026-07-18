//! Adds the `GameManagers` table (co-organizers) — runs on existing databases.
use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::Schema;

use crate::models::data::game_manager;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let schema = Schema::new(manager.get_database_backend());
        let mut stmt = schema.create_table_from_entity(game_manager::Entity);
        stmt.if_not_exists();
        manager.create_table(stmt).await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("GameManagers"))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
