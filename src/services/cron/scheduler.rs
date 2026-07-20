//! Latency-sensitive A&D/KotH round scheduler.
//!
//! Maintenance, Docker reaping, and VPN reconciliation deliberately live on a
//! separate supervisor. This driver polls every five seconds and handles games
//! concurrently; per-game advisory locks and durable finish leases remain the
//! authority for retries and multi-replica safety.

use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use futures::StreamExt;
use sea_orm::EntityTrait;

use super::round_finish::{drive_round_pipeline, PipelineDrive};
use super::{RoundSchedulerScope, ADVANCE_BUDGET_SECS};
use crate::app_state::SharedState;
use crate::models::data::game;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
use crate::utils::error::AppResult;

const DEFAULT_WARMUP_SECONDS: i64 = 1_800;
const DEFAULT_GAME_CONCURRENCY: usize = 4;

#[derive(Clone, Copy, Debug, sqlx::FromRow)]
struct ActiveGame {
    id: i32,
    start_time_utc: chrono::DateTime<Utc>,
    end_time_utc: chrono::DateTime<Utc>,
    ad_warmup_seconds: Option<i32>,
    ad_min_grace_period_seconds: Option<i32>,
    ad_scoring_paused: bool,
    network_bound: bool,
}

#[derive(Clone, Copy, Debug, sqlx::FromRow)]
struct LatestRound {
    id: i32,
    number: i32,
    end_time_utc: chrono::DateTime<Utc>,
    pipeline_complete: bool,
}

impl LatestRound {
    fn cursor(self) -> crate::services::ad_engine::RoundCursor {
        crate::services::ad_engine::RoundCursor {
            id: self.id,
            number: self.number,
        }
    }
}

fn game_concurrency() -> usize {
    std::env::var("RSCTF_AD_GAME_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=16).contains(value))
        .unwrap_or(DEFAULT_GAME_CONCURRENCY)
}

fn scope_accepts(scope: RoundSchedulerScope, network_bound: bool) -> bool {
    match scope {
        RoundSchedulerScope::All => true,
        RoundSchedulerScope::ManagedOnly => !network_bound,
        RoundSchedulerScope::NetworkBoundOnly => network_bound,
    }
}

fn required_network_bound(scope: RoundSchedulerScope) -> Option<bool> {
    match scope {
        RoundSchedulerScope::All => None,
        RoundSchedulerScope::ManagedOnly => Some(false),
        RoundSchedulerScope::NetworkBoundOnly => Some(true),
    }
}

fn minimum_round_runway_seconds(grace_seconds: Option<i32>) -> i64 {
    i64::from(
        grace_seconds
            .unwrap_or(crate::services::ad_engine::DEFAULT_CHECKER_GRACE_SECONDS)
            .clamp(1, 60),
    ) + i64::try_from(
        crate::services::ad_engine::FLAG_DELIVERY_PUBLICATION_RESERVE_SECONDS
            + crate::services::ad_engine::CHECKER_MINIMUM_RUNWAY_SECONDS
            + crate::services::ad_engine::CHECKER_SCHEDULER_OUTER_MARGIN_SECONDS,
    )
    .unwrap_or(i64::MAX)
}

fn has_minimum_round_runway(
    event_end: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
    grace_seconds: Option<i32>,
) -> bool {
    event_end.signed_duration_since(now)
        >= Duration::seconds(minimum_round_runway_seconds(grace_seconds))
}

/// Advance every due game without allowing one slow game's checker pipeline to
/// delay the rest of the event field.
pub(super) async fn advance_ad_rounds(
    state: &SharedState,
    scope: RoundSchedulerScope,
) -> AppResult<u64> {
    let now = Utc::now();
    let games = sqlx::query_as::<_, ActiveGame>(
        r#"SELECT game.id, game.start_time_utc, game.end_time_utc,
                  game.ad_warmup_seconds, game.ad_min_grace_period_seconds,
                  game.ad_scoring_paused,
                  EXISTS (
                    SELECT 1
                      FROM "GameChallenges" challenge
                     WHERE challenge.game_id = game.id
                       AND challenge.is_enabled = TRUE
                       AND challenge.review_status = $2
                       AND challenge."Type" = $3
                       AND challenge.ad_self_hosted = TRUE
                  ) AS network_bound
             FROM "Games" game
            WHERE game.start_time_utc <= $1 AND game.end_time_utc > $1"#,
    )
    .bind(now)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_all(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let games = games
        .into_iter()
        .filter(|game| scope_accepts(scope, game.network_bound));
    let mut work = futures::stream::iter(games)
        .map(|game| async move {
            let game_id = game.id;
            match advance_game(state, game, scope).await {
                Ok(advanced) => advanced,
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "cron: game round driver failed");
                    false
                }
            }
        })
        .buffer_unordered(game_concurrency());
    let mut advanced = 0u64;
    while let Some(did_advance) = work.next().await {
        advanced += u64::from(did_advance);
    }
    Ok(advanced)
}

async fn advance_game(
    state: &SharedState,
    schedule: ActiveGame,
    scope: RoundSchedulerScope,
) -> AppResult<bool> {
    if schedule.ad_scoring_paused {
        return Ok(false);
    }

    let (has_engine_challenge, network_bound): (bool, bool) = sqlx::query_as(
        r#"SELECT
             EXISTS(
               SELECT 1 FROM "GameChallenges"
                WHERE game_id = $1 AND is_enabled = TRUE
                  AND review_status = $2 AND "Type" IN ($3,$4)
             ),
             EXISTS(
               SELECT 1 FROM "GameChallenges"
                WHERE game_id = $1 AND is_enabled = TRUE
                  AND review_status = $2 AND "Type" = $3
                  AND ad_self_hosted = TRUE
             )"#,
    )
    .bind(schedule.id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_one(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    if !has_engine_challenge || !scope_accepts(scope, network_bound) {
        return Ok(false);
    }

    let latest = sqlx::query_as::<_, LatestRound>(
        r#"SELECT id, number, end_time_utc,
                  pipeline_completed_at IS NOT NULL AS pipeline_complete
             FROM "AdRounds" WHERE game_id = $1
            ORDER BY number DESC, id DESC LIMIT 1"#,
    )
    .bind(schedule.id)
    .fetch_optional(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let mut game_model = None;

    let mut advanced = false;
    // A committed round may be unfinished after a crash. Recover it before
    // considering its successor; the durable lease rejects duplicate owners.
    // Once its authoritative deadline passes, fence remaining writers and turn
    // missing evidence into platform voids so checker latency cannot stretch
    // every later round.
    if let Some(current) = &latest {
        if !current.pipeline_complete {
            let game = load_game_model(state, schedule.id).await?;
            game_model = Some(game);
            match drive_round_pipeline(
                state,
                game_model.as_ref().expect("game model was loaded"),
                current.id,
                None,
                pipeline_budget(current.end_time_utc),
            )
            .await
            {
                Ok(PipelineDrive::Complete) => {}
                Ok(PipelineDrive::InFlight) => return Ok(advanced),
                Ok(PipelineDrive::Finished | PipelineDrive::Expired) => advanced = true,
                Err(error) => {
                    tracing::warn!(
                        game = schedule.id,
                        round = current.number,
                        %error,
                        "cron: prepared-round recovery failed"
                    );
                    return Ok(advanced);
                }
            }
        }
    }

    let now = Utc::now();
    let warmup = schedule
        .ad_warmup_seconds
        .map(i64::from)
        .unwrap_or(DEFAULT_WARMUP_SECONDS);
    let due = latest.as_ref().map_or(
        now >= schedule.start_time_utc + Duration::seconds(warmup),
        |round| round.end_time_utc <= now,
    );
    if !due {
        return Ok(advanced);
    }
    if !has_minimum_round_runway(
        schedule.end_time_utc,
        now,
        schedule.ad_min_grace_period_seconds,
    ) {
        // A clipped terminal pseudo-round would make every participant's flag
        // and checker sample platform-owned. Stop cleanly instead; event-end
        // settlement closes the last real round.
        return Ok(advanced);
    }

    // Readiness work can include a slow container-runtime call. Keep that delay
    // outside the persisted scoring window so a healthy service still receives
    // a truthful tick after the platform becomes ready. Preparation revalidates
    // every service identity while holding the game lock.
    let game = match game_model {
        Some(game) => game,
        None => load_game_model(state, schedule.id).await?,
    };
    let repair_failures = match crate::controllers::edit::ensure_ad_containers(
        state, &game, None, false, false,
    )
    .await
    {
        Ok((_, failures)) => failures as usize,
        Err(error) => {
            tracing::warn!(
                game = schedule.id,
                %error,
                "cron: managed A&D service readiness failed before round preparation"
            );
            1
        }
    };
    if repair_failures > 0 {
        tracing::warn!(
            game = schedule.id,
            failed = repair_failures,
            "cron: managed A&D service readiness was incomplete before round preparation"
        );
    }
    let now = Utc::now();
    if !has_minimum_round_runway(
        schedule.end_time_utc,
        now,
        schedule.ad_min_grace_period_seconds,
    ) {
        // Do not create a terminal pseudo-round when readiness consumed the
        // remaining event window. The failed work is platform downtime, not a
        // participant sample.
        return Ok(advanced);
    }

    let prepared = match crate::services::ad_engine::prepare_round(
        &state.db,
        schedule.id,
        latest.map(LatestRound::cursor),
        required_network_bound(scope),
        now,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::warn!(game = schedule.id, %error, "cron: round preparation failed");
            return Ok(false);
        }
    };
    state
        .cache
        .remove(&format!("latestround:{}", schedule.id))
        .await;
    let round_id = prepared.id;
    let round_number = prepared.number;
    let budget = pipeline_budget(prepared.ends_at);
    match drive_round_pipeline(state, &game, round_id, Some(prepared), budget).await {
        Ok(PipelineDrive::Finished | PipelineDrive::Expired) => Ok(true),
        Ok(PipelineDrive::Complete | PipelineDrive::InFlight) => Ok(advanced),
        Err(error) => {
            tracing::warn!(
                game = game.id,
                round = round_number,
                %error,
                "cron: round finishing failed"
            );
            Ok(advanced)
        }
    }
}

/// Container reconciliation still consumes the established complete game
/// entity. Load it only for a due/recovery mutation; the five-second polling
/// path above remains a compact raw-sqlx projection.
async fn load_game_model(state: &SharedState, game_id: i32) -> AppResult<game::Model> {
    game::Entity::find_by_id(game_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| crate::utils::error::AppError::not_found("Game not found"))
}

/// Leave a one-second persistence margin before the authoritative deadline.
/// The engine's configured 30..600 second tick remains the outer bound; the
/// global cap protects unusual long-tick events from a wedged checker.
fn pipeline_budget(deadline: chrono::DateTime<Utc>) -> StdDuration {
    let remaining = deadline
        .signed_duration_since(Utc::now())
        .to_std()
        .unwrap_or(StdDuration::from_secs(1));
    remaining
        .saturating_sub(StdDuration::from_secs(1))
        .max(StdDuration::from_secs(1))
        .min(StdDuration::from_secs(ADVANCE_BUDGET_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_scopes_partition_network_bound_games_without_overlap() {
        assert!(scope_accepts(RoundSchedulerScope::All, false));
        assert!(scope_accepts(RoundSchedulerScope::All, true));

        assert!(scope_accepts(RoundSchedulerScope::ManagedOnly, false));
        assert!(!scope_accepts(RoundSchedulerScope::ManagedOnly, true));

        assert!(!scope_accepts(RoundSchedulerScope::NetworkBoundOnly, false));
        assert!(scope_accepts(RoundSchedulerScope::NetworkBoundOnly, true));

        assert_eq!(required_network_bound(RoundSchedulerScope::All), None);
        assert_eq!(
            required_network_bound(RoundSchedulerScope::ManagedOnly),
            Some(false)
        );
        assert_eq!(
            required_network_bound(RoundSchedulerScope::NetworkBoundOnly),
            Some(true)
        );
    }

    #[test]
    fn game_concurrency_default_is_bounded() {
        let value = game_concurrency();
        assert!((1..=16).contains(&value));
    }

    #[test]
    fn terminal_round_runway_matches_the_validated_tick_reserve() {
        assert_eq!(minimum_round_runway_seconds(None), 15);
        assert_eq!(minimum_round_runway_seconds(Some(18)), 30);
    }

    #[test]
    fn readiness_delay_is_rechecked_before_a_max_grace_round_is_created() {
        let before_readiness = Utc::now();
        let event_end = before_readiness + Duration::seconds(72);
        assert!(has_minimum_round_runway(
            event_end,
            before_readiness,
            Some(60)
        ));
        assert!(!has_minimum_round_runway(
            event_end,
            before_readiness + Duration::seconds(1),
            Some(60)
        ));
    }

    #[test]
    fn pipeline_budget_keeps_a_deadline_margin_and_global_cap() {
        let short = pipeline_budget(Utc::now() + Duration::seconds(10));
        assert!((StdDuration::from_secs(8)..=StdDuration::from_secs(9)).contains(&short));
        let long = pipeline_budget(Utc::now() + Duration::seconds(600));
        assert_eq!(long, StdDuration::from_secs(ADVANCE_BUDGET_SECS));
    }
}
