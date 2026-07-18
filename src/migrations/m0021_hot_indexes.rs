//! Adds indexes on the hot filter columns of tables that grow without bound
//! (submissions, A&D flags/checks/attacks, KotH control results, game events).
//! `create_table_from_entity` only ever created PK/unique constraints, so these
//! columns were unindexed — every scoreboard poll seq-scanned `Submissions`, every
//! A&D flag submit seq-scanned `AdFlags`, etc. All indexes are `if_not_exists` so
//! the migration is idempotent and safe to re-run on existing deployments.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

// (index name, table, [columns]) — kept as data so `up`/`down` stay in sync.
fn indexes() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        // Scoreboard: WHERE game_id AND status=Accepted, scanned on every poll.
        (
            "ix_submissions_game_status",
            "Submissions",
            vec!["game_id", "status"],
        ),
        // Submit attempt-count / solved-set / review gate: WHERE participation_id AND challenge_id.
        (
            "ix_submissions_part_challenge",
            "Submissions",
            vec!["participation_id", "challenge_id"],
        ),
        // A&D submit: WHERE flag = ? (up to 100 lookups per request, hottest live path).
        ("ix_adflags_flag", "AdFlags", vec!["flag"]),
        // Flag plant/lookup per round+service.
        (
            "ix_adflags_round_service",
            "AdFlags",
            vec!["round_id", "team_service_id"],
        ),
        // Checker bookkeeping: previous/existing check per (service, round), each tick.
        (
            "ix_adcheckresults_service_round",
            "AdCheckResults",
            vec!["team_service_id", "round_id"],
        ),
        // KotH per-hill control result upsert per (game, challenge, round), each tick.
        (
            "ix_kothcontrol_game_challenge_round",
            "KothControlResults",
            vec!["game_id", "challenge_id", "ad_round_id"],
        ),
        // A&D attack lookups by flag + victim service.
        ("ix_adattacks_flag", "AdAttacks", vec!["flag_id"]),
        (
            "ix_adattacks_victim",
            "AdAttacks",
            vec!["victim_team_service_id"],
        ),
        // Game-event scans (challenge-opened checks, monitor feed) filter by game.
        ("ix_gameevents_game", "GameEvents", vec!["game_id"]),
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
