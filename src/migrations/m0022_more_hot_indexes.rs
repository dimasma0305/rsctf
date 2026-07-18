//! Second index pass (perf-hunt, 2026-07-07). `create_table_from_entity` only
//! ever created PK/unique constraints, so these hot/growing filter+sort columns
//! were still unindexed after m0021:
//!   * `AdRounds(game_id, number)` — the table had NO non-PK index; the A&D submit
//!     + engine paths seq-scan it for the latest round (`ORDER BY number DESC
//!     LIMIT 1`) and for full-round loads (`WHERE game_id`).
//!   * `GameEvents(game_id, team_id, "Type")` — the per-open ChallengeOpened dedup
//!     scans it on every challenge view.
//!   * `AspNetUsers(normalized_email)` — login OR-branch + every email lookup.
//!   * `GameEvents(game_id, publish_time_utc)` — the ordered monitor/event feed.
//!   * `Logs(time_utc)` — the admin audit-log listing (ORDER BY time DESC).
//!
//! All `if_not_exists`, so idempotent on existing deployments.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// (index name, table, [columns]) — data-driven so up/down stay in sync.
fn indexes() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        (
            "ix_adrounds_game_number",
            "AdRounds",
            vec!["game_id", "number"],
        ),
        (
            "ix_gameevents_game_team_type",
            "GameEvents",
            vec!["game_id", "team_id", "Type"],
        ),
        (
            "ix_aspnetusers_normalized_email",
            "AspNetUsers",
            vec!["normalized_email"],
        ),
        (
            "ix_gameevents_game_publish",
            "GameEvents",
            vec!["game_id", "publish_time_utc"],
        ),
        ("ix_logs_time", "Logs", vec!["time_utc"]),
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
