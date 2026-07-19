//! Attack-Defense round/checker operations and King-of-the-Hill lifecycle.
//!
//! The sole official A&D standings formula lives in `services::ad::scoring`.
//! This module owns round rotation, flag/check persistence, checker execution,
//! and the crown-cycle KotH engine; it intentionally exposes no legacy
//! Attack+SLA-Defense or refresh-window implementation.

use std::collections::HashSet;

use chrono::{Duration, Utc};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};

use crate::models::data::{ad_flag, ad_round, ad_team_service, game_challenge};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};

/// Resolve the single authoritative A&D service field used by flag planting,
/// checking, and scoring. Historical/rejected rows remain stored for audit, but
/// never receive new capabilities or affect the current field size.
pub(crate) async fn active_ad_services(
    db: &DatabaseConnection,
    game_id: i32,
) -> AppResult<Vec<ad_team_service::Model>> {
    let ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT service.id
             FROM "AdTeamServices" service
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE service.game_id = $1
              AND participation.status = $2
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $3
              AND challenge."Type" = $4
            ORDER BY service.id"#,
    )
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(ad_team_service::Entity::find()
        .filter(ad_team_service::Column::Id.is_in(ids))
        .order_by_asc(ad_team_service::Column::Id)
        .all(db)
        .await?)
}

// ─────────────────────────────────────────────────────────────────────────────
// Checker verdict — enochecker3-compatible service status.
// ─────────────────────────────────────────────────────────────────────────────

/// Per-tick result of a checker run against one team's A&D service container.
/// Wire-compatible with the enochecker3 service-check contract.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AdCheckStatus {
    /// Checker succeeded — flag planted + retrieved. Full SLA credit.
    Ok = 0,
    /// Service is up but behaving incorrectly (flag mismatch, partial outage). No credit.
    Mumble = 1,
    /// Service didn't respond at all (TCP refused / timeout). No credit.
    Offline = 2,
    /// The checker itself failed. Official A&D scoring adjudicates this through
    /// prior-result carry and the field-wide first-error outage rule.
    InternalError = 3,
}

impl AdCheckStatus {
    /// Decode a stored `ad_check_result.status` / `ad_team_service.status` numeric
    /// (see the `i16` columns in `models::data::ad`). Unknown values degrade to
    /// `InternalError`, which is adjudicated as infrastructure rather than a
    /// fabricated service verdict.
    pub fn from_i16(v: i16) -> AdCheckStatus {
        match v {
            0 => AdCheckStatus::Ok,
            1 => AdCheckStatus::Mumble,
            2 => AdCheckStatus::Offline,
            _ => AdCheckStatus::InternalError,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Operational checker constants + env-overridable scheduler config.
// ─────────────────────────────────────────────────────────────────────────────

/// Full credit — service up + correct this tick.
pub const SLA_CREDIT_OK: f64 = 1.0;
/// Half credit — Ok this tick but down/mumble the previous tick (recovering).
pub const SLA_CREDIT_RECOVERING: f64 = 0.5;
/// No credit — Mumble / Offline / InternalError.
pub const SLA_CREDIT_NONE: f64 = 0.0;
/// Runtime, env-overridable scheduler knobs.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AdScoringConfig {
    /// Round/tick length in seconds (default 60).
    pub tick_seconds: i64,
    /// Warmup before round 1 bootstraps (default 1800).
    pub warmup_seconds: i64,
}

impl Default for AdScoringConfig {
    fn default() -> Self {
        AdScoringConfig {
            tick_seconds: 60,
            warmup_seconds: 1800,
        }
    }
}

impl AdScoringConfig {
    /// Load per-deployment overrides from the environment. Each var is optional;
    /// an unset or unparseable value keeps the RSCTF default.
    pub fn from_env() -> Self {
        fn i(key: &str, default: i64) -> i64 {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        let d = AdScoringConfig::default();
        AdScoringConfig {
            tick_seconds: i("RSCTF_AD_TICK_SECONDS", d.tick_seconds),
            warmup_seconds: i("RSCTF_AD_WARMUP_SECONDS", d.warmup_seconds),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// In-memory values used to plan round rotation and KotH ticks.
// ─────────────────────────────────────────────────────────────────────────────

/// One A&D round (tick).
#[derive(Clone, Debug, PartialEq)]
pub struct Round {
    pub id: i64,
    pub game_id: i32,
    /// 1-based round number, monotonic per game.
    pub number: i32,
    /// Round start (UTC epoch seconds — chrono is available but epoch keeps the
    /// pure math dependency-free and trivially testable).
    pub started_at: i64,
    /// Round end = `started_at + tick_seconds`.
    pub ends_at: i64,
}

/// A team's instance of one A&D/KotH challenge — the container the checker
/// probes and the flag is planted into.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamService {
    pub id: i64,
    /// The team's participation id (scoring identity).
    pub participation_id: i32,
    pub challenge_id: i32,
    /// Live container id, if any — `None` means nothing to plant into this tick.
    pub container_id: Option<String>,
}

mod checker;
mod flag_delivery;
mod koth_auth;
pub(crate) mod koth_cycle;
mod koth_marker;
mod persistence;
mod reducers;
mod round_completion;
mod rounds;
pub mod sandbox;
mod service_reset;

pub use checker::*;
pub(crate) use flag_delivery::{
    record_flag_delivery_outcomes, FlagDeliveryKind, FlagDeliveryOutcome,
};
pub(crate) use koth_auth::{
    acquire_game_lock as acquire_ad_game_lock, clear_challenge_control, game_lock_key,
    revoke_koth_capabilities,
};
pub(crate) use persistence::{complete_missing_koth_results, finalize_ended_round_checks};
pub use reducers::*;
pub use round_completion::abandon_process_round_finishes;
pub(crate) use round_completion::{
    abandon_round_finish, claim_round_finish, complete_round_finish, expire_overdue_round_finish,
    lock_owned_round_finish, prepared_round_snapshot, RoundFinishDisposition, RoundFinishLease,
};
pub use rounds::*;
pub(crate) use service_reset::{prepare_service_reset, publish_service_reset};

// ─────────────────────────────────────────────────────────────────────────────
// Unit tests for operational checker credit and round preparation.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn tick_credit_recovering_semantics() {
        use AdCheckStatus::*;
        assert!(close(tick_credit(Ok, None), 1.0));
        assert!(close(tick_credit(Ok, Some(Ok)), 1.0));
        assert!(close(tick_credit(Ok, Some(Offline)), 0.5)); // recovering
        assert!(close(tick_credit(Ok, Some(Mumble)), 0.5)); // recovering
        assert!(close(tick_credit(Ok, Some(InternalError)), 1.0)); // infra fault isn't a "down" tick
        assert!(close(tick_credit(Mumble, Some(Ok)), 0.0));
        assert!(close(tick_credit(Offline, None), 0.0));
        assert!(close(tick_credit(InternalError, Some(Ok)), 0.0));
    }

    #[test]
    fn stored_credit_is_unscaled_completion_data() {
        use AdCheckStatus::*;
        assert!(close(stored_tick_credit(Ok, Some(Ok)), 1.0));
        assert!(close(stored_tick_credit(Ok, Some(Offline)), 0.5));
        assert!(close(stored_tick_credit(Offline, None), 0.0));
    }

    #[test]
    fn needs_advance_warmup_then_expiry() {
        let cfg = AdScoringConfig::default(); // warmup 1800, tick 60
        let start = 1_000_000;
        // no round yet, before warmup end
        assert!(!needs_advance(start + 100, start, None, &cfg));
        // no round yet, after warmup end → bootstrap
        assert!(needs_advance(start + 1800, start, None, &cfg));
        // round live, not yet expired
        assert!(!needs_advance(
            start + 2000,
            start,
            Some(start + 2060),
            &cfg
        ));
        // round expired
        assert!(needs_advance(start + 2100, start, Some(start + 2060), &cfg));
    }

    #[test]
    fn plan_round_plants_flags() {
        let cfg = AdScoringConfig::default();
        let services = vec![
            TeamService {
                id: 1,
                participation_id: 10,
                challenge_id: 1,
                container_id: Some("c1".into()),
            },
            TeamService {
                id: 2,
                participation_id: 11,
                challenge_id: 1,
                container_id: Some("c2".into()),
            },
            // no container → no plant
            TeamService {
                id: 3,
                participation_id: 12,
                challenge_id: 1,
                container_id: None,
            },
        ];
        let mut n = 0;
        let plan = plan_round(7, 0, 1_000, &services, &cfg, || {
            n += 1;
            format!("flag{{det{n}}}")
        });
        assert_eq!(plan.round.number, 1);
        assert_eq!(plan.round.ends_at, 1_000 + 60);
        assert_eq!(plan.flags.len(), 2); // only the two live containers
        assert_eq!(plan.flags[0].value, "flag{det1}");
        let plan2 = plan_round(7, 1, 2_000, &services, &cfg, || "flag{x}".into());
        assert_eq!(plan2.round.number, 2);
    }

    #[test]
    fn format_flag_is_url_safe_and_wrapped() {
        let bytes = [0xFBu8, 0xEF, 0xBE]; // exercises the +/ → _- mapping
        let f = format_flag(&bytes);
        assert!(f.starts_with("flag{"));
        assert!(f.ends_with('}'));
        let inner = &f[5..f.len() - 1];
        assert!(inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        assert!(!inner.contains('+') && !inner.contains('/') && !inner.contains('='));
    }

    #[test]
    fn env_config_falls_back_to_defaults() {
        // Without overrides, scheduler defaults stay stable.
        let d = AdScoringConfig::default();
        let e = AdScoringConfig::from_env();
        assert_eq!(e.tick_seconds, d.tick_seconds);
        assert_eq!(e.warmup_seconds, d.warmup_seconds);
    }
}
