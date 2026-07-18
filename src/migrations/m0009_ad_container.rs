//! Adds `AdTeamServices.container_id` + `last_reset_at` (platform-launched A&D
//! service containers + self-reset cooldown). Idempotent, runs on existing DBs.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("AdTeamServices"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("container_id")).text().null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("AdTeamServices"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("last_reset_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for col in ["container_id", "last_reset_at"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("AdTeamServices"))
                        .drop_column(Alias::new(col))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
