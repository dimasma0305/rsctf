//! Official epoch-settled scoring for Attack-Defense evidence.
//!
//! PostgreSQL qualifies and bounds the historical evidence; the formula and
//! cross-service aggregation remain pure Rust functions.

mod aggregate;
mod board;
mod evidence;
mod formula;
mod rollup;
mod service_rollup;

pub use aggregate::{
    aggregate_team_epoch, average_equal_epochs, average_weighted_epochs, score_team_epoch,
    TeamEpochScore,
};
pub use board::{
    build_ad_scoreboard, AdEpochScore, AdEvidenceStatus, AdScoreboard, AdScoreboardChallenge,
    AdServiceScore, AdTeamScore,
};
pub use formula::{
    score_epoch_service, EpochServiceEvidence, EpochServiceScore, ScoringError, BALANCE_WEIGHT,
    DEFENSE_WEIGHT, MAX_SERVICE_WEIGHT, MIN_SERVICE_WEIGHT, OFFENSE_WEIGHT, RARITY_COEFFICIENT,
};
pub(crate) use rollup::{
    ensure_epoch_rollups as refresh_epoch_rollups, invalidate_rollups_for_end_change,
    invalidate_rollups_from_round, lock_epoch_rollups,
};
