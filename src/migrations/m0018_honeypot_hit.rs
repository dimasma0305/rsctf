//! Adds `HoneypotHits` — one row per hit on a honeypot bait route
//! (`controllers::honeypot`), the ground truth the `HoneypotChain` detector
//! aggregates. Ported from RSCTF's honeypot-hit persistence.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("HoneypotHits"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("game_id")).integer().null())
                    .col(
                        ColumnDef::new(Alias::new("participation_id"))
                            .integer()
                            .null(),
                    )
                    .col(ColumnDef::new(Alias::new("user_id")).uuid().null())
                    .col(ColumnDef::new(Alias::new("bait")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("remote_ip"))
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .col(ColumnDef::new(Alias::new("user_agent")).text().null())
                    .col(
                        ColumnDef::new(Alias::new("hit_at_utc"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("HoneypotHits"))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
