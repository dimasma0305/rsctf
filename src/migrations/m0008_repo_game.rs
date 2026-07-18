//! Adds `RepoBindings.game_id` (the event a repo syncs into). Idempotent ALTER,
//! runs on existing databases that already have the RepoBindings table.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("RepoBindings"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("game_id")).integer().null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("RepoBindings"))
                    .drop_column(Alias::new("game_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
