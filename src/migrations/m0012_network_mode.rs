//! Adds `GameChallenges.network_mode` (container network mode; RSCTF
//! `Challenge.NetworkMode`, nullable, default `Open` = 0). Idempotent, runs on
//! existing DBs — existing rows default to `Open`.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("GameChallenges"))
                    .add_column_if_not_exists(
                        // Stored as SmallInteger (the `NetworkMode` db_enum's i16 repr);
                        // nullable to match the `Option<NetworkMode>` field, default 0 = Open.
                        ColumnDef::new(Alias::new("network_mode"))
                            .small_integer()
                            .null()
                            .default(0),
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
                    .table(Alias::new("GameChallenges"))
                    .drop_column(Alias::new("network_mode"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
