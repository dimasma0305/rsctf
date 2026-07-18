//! Adds `AdCheckResults.sla_credit` — the field-scaled SLA credit frozen at
//! check-land time (weighted by THAT round's field size, `sqrt(teams)`), so a
//! team joining later can't retroactively rescale historical SLA. Nullable:
//! legacy rows written before this column stay NULL and the scoreboard falls
//! back to a live recompute for them. Idempotent, runs on existing DBs.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("AdCheckResults"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("sla_credit")).double().null(),
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
                    .table(Alias::new("AdCheckResults"))
                    .drop_column(Alias::new("sla_credit"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
