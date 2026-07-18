//! Adds `KothTargets.container_id` (platform-launched shared hill container).
//! Idempotent, runs on existing DBs.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("KothTargets"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("container_id")).text().null(),
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
                    .table(Alias::new("KothTargets"))
                    .drop_column(Alias::new("container_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
