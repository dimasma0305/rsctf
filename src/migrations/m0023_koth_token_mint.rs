//! Extends `KothTokens` into RSCTF's per-participation **mint** table (the record
//! that makes per-tick KotH king election possible). Adds:
//!   * `round_number` — the refresh-window **anchor round** the token is valid for
//!     (the checker matches `/koth/king` against tokens minted for the current
//!     window's anchor).
//!   * `ad_round_id` — the round the token was minted in.
//!
//! and makes `target_id` nullable: a minted token is **game-wide** (one token
//! arbitrates every hill the team wrote it into), not tied to a single target.
//!
//! Legacy capture-log rows (from the removed capture flow) keep their `target_id`
//! and leave the new columns NULL; the checker filters on `round_number`, so they
//! are simply ignored rather than migrated. Idempotent — safe on existing DBs.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("KothTokens"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("round_number")).integer().null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("KothTokens"))
                    .add_column_if_not_exists(
                        ColumnDef::new(Alias::new("ad_round_id")).integer().null(),
                    )
                    .to_owned(),
            )
            .await?;
        // A minted token is game-wide, so it carries no target — drop the NOT NULL.
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("KothTokens"))
                    .modify_column(ColumnDef::new(Alias::new("target_id")).integer().null())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for col in ["round_number", "ad_round_id"] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("KothTokens"))
                        .drop_column(Alias::new(col))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
