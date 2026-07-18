//! Indexes for the KotH mint table added in m0023. The per-tick checker preloads
//! the window's token→participation map by `round_number` (and the token endpoint
//! looks a team's token up by `(participation_id, round_number)`), so without these
//! every checker tick + token fetch seq-scans `KothTokens`, which grows one row per
//! participation per refresh window for the whole game. Idempotent.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

fn indexes() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        // Checker: WHERE round_number = anchor (the token-map preload each tick).
        (
            "ix_kothtokens_round_number",
            "KothTokens",
            vec!["round_number"],
        ),
        // Token endpoint: WHERE participation_id AND round_number = anchor.
        (
            "ix_kothtokens_part_round",
            "KothTokens",
            vec!["participation_id", "round_number"],
        ),
    ]
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, table, cols) in indexes() {
            let mut idx = Index::create();
            idx.if_not_exists().name(name).table(Alias::new(table));
            for c in cols {
                idx.col(Alias::new(c));
            }
            manager.create_index(idx.to_owned()).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, table, _) in indexes() {
            manager
                .drop_index(Index::drop().name(name).table(Alias::new(table)).to_owned())
                .await?;
        }
        Ok(())
    }
}
