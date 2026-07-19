use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::app_state::SharedState;
use crate::services::cache::Cache;
use crate::utils::error::{AppError, AppResult};

const LIVE_LIFECYCLE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothCooldownParticipant {
    pub participation_id: i32,
    pub team_name: String,
    pub remaining_ticks: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct KothLifecycleView {
    pub cycle_ticks: i32,
    pub claim_confirmation_ticks: i32,
    pub cycle_number: i32,
    pub cycle_tick: i32,
    pub durable_phase: String,
    pub reset_phase: String,
    pub is_scorable: bool,
    pub next_reset_ticks: Option<i32>,
    pub provisional_participation_id: Option<i32>,
    pub provisional_team_name: Option<String>,
    pub confirmation_progress: i32,
    pub cooldown_participants: Vec<KothCooldownParticipant>,
    pub old_container_id: Option<String>,
    pub replacement_container_id: Option<String>,
    pub reset_attempt: i32,
    pub readiness_failures: i32,
    pub readiness_error: Option<String>,
    pub can_retry: bool,
    pub reset_receipt_id: Option<i64>,
    pub scoring_receipt_id: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedLiveLifecycle {
    latest_round: i32,
    views: HashMap<i32, KothLifecycleView>,
}

static LIVE_LIFECYCLE_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<CachedLiveLifecycle>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

fn live_lifecycle_cache_key(game_id: i32) -> String {
    format!("kothlifecycle:{game_id}")
}

fn cached_views_for_round(
    cached: CachedLiveLifecycle,
    latest_round: i32,
) -> Option<HashMap<i32, KothLifecycleView>> {
    (cached.latest_round == latest_round).then_some(cached.views)
}

fn decode_live_lifecycle(
    bytes: &[u8],
    latest_round: i32,
) -> Option<HashMap<i32, KothLifecycleView>> {
    cached_views_for_round(serde_json::from_slice(bytes).ok()?, latest_round)
}

/// Evict the shared live lifecycle projection after a crown transition. The
/// tiered cache clears the writer's L1 immediately; existing remote L1 copies
/// expire within one second. A pre-transition fill that races the eviction can
/// repopulate L2 and extend that rare stale window to about two cache TTLs.
pub(crate) async fn invalidate_live_lifecycle_cache(cache: &dyn Cache, game_id: i32) {
    cache.remove(&live_lifecycle_cache_key(game_id)).await;
}

impl Default for KothLifecycleView {
    fn default() -> Self {
        Self {
            cycle_ticks: 3,
            claim_confirmation_ticks: 2,
            cycle_number: 0,
            cycle_tick: 0,
            durable_phase: "Uninitialized".to_string(),
            reset_phase: "Readiness".to_string(),
            is_scorable: false,
            next_reset_ticks: None,
            provisional_participation_id: None,
            provisional_team_name: None,
            confirmation_progress: 0,
            cooldown_participants: Vec::new(),
            old_container_id: None,
            replacement_container_id: None,
            reset_attempt: 0,
            readiness_failures: 0,
            readiness_error: None,
            can_retry: false,
            reset_receipt_id: None,
            scoring_receipt_id: None,
        }
    }
}

#[derive(Debug, FromRow)]
struct LifecycleRow {
    challenge_id: i32,
    cycle_ticks: i32,
    claim_confirmation_ticks: i32,
    cycle_number: Option<i32>,
    planned_start_round: Option<i32>,
    planned_end_round: Option<i32>,
    actual_start_round: Option<i32>,
    phase: Option<String>,
    provisional_participation_id: Option<i32>,
    provisional_team_name: Option<String>,
    confirmation_progress: Option<i32>,
    old_container_id: Option<String>,
    replacement_container_id: Option<String>,
    reset_attempt: Option<i32>,
    readiness_failures: Option<i32>,
    readiness_error: Option<String>,
    last_error: Option<String>,
    reset_receipt_id: Option<i64>,
    scoring_receipt_id: Option<i64>,
    cooldown_participation_ids: Vec<i32>,
    cooldown_team_names: Vec<String>,
    cooldown_remaining_ticks: Vec<i32>,
    historical_is_scorable: Option<bool>,
}

pub(super) fn wire_phase(phase: Option<&str>) -> String {
    match phase {
        None => "Readiness",
        Some("FinalizePending") => "Finalizing",
        Some("SnapshotPending") => "Snapshotting",
        Some("DestroyPending") => "Destroying",
        Some("CreatePending" | "PublishPending") => "Creating",
        Some("CapabilityPending" | "FirewallPending") => "Activating",
        Some("ReadinessPending") => "Readiness",
        Some("CooldownReleasePending") => "CooldownRelease",
        Some("Failed") => "Failed",
        Some("Ended" | "Completed") => "Ended",
        Some(_) => "Active",
    }
    .to_string()
}

fn effective_phase(
    snapshot: bool,
    durable_phase: Option<&str>,
    latest_round: i32,
    planned_start_round: Option<i32>,
    planned_end_round: Option<i32>,
    actual_start_round: Option<i32>,
) -> Option<&str> {
    if snapshot || durable_phase != Some("Active") {
        return durable_phase;
    }
    let (Some(planned_start), Some(planned_end), Some(actual_start)) =
        (planned_start_round, planned_end_round, actual_start_round)
    else {
        return Some("ReadinessPending");
    };
    if latest_round > planned_end {
        Some("FinalizePending")
    } else if latest_round < planned_start || latest_round < actual_start {
        Some("ReadinessPending")
    } else {
        durable_phase
    }
}

fn durable_phase_can_retry(phase: Option<&str>) -> bool {
    !matches!(phase, None | Some("Active" | "Completed" | "Ended"))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use std::collections::HashMap;

    use crate::services::cache::{Cache, InMemoryCache};

    use super::{
        cached_views_for_round, decode_live_lifecycle, durable_phase_can_retry, effective_phase,
        invalidate_live_lifecycle_cache, live_lifecycle_cache_key, wire_phase, CachedLiveLifecycle,
        KothLifecycleView,
    };

    #[test]
    fn cooldown_release_is_exposed_as_a_recovery_transition() {
        assert_eq!(
            wire_phase(Some("CooldownReleasePending")),
            "CooldownRelease"
        );
    }

    #[test]
    fn live_active_phase_is_scorable_only_inside_its_planned_rounds() {
        assert_eq!(
            effective_phase(false, Some("Active"), 6, Some(6), Some(8), Some(6)),
            Some("Active")
        );
        assert_eq!(
            effective_phase(false, Some("Active"), 8, Some(6), Some(8), Some(6)),
            Some("Active")
        );
        assert_eq!(
            effective_phase(false, Some("Active"), 9, Some(6), Some(8), Some(6)),
            Some("FinalizePending")
        );
        assert_eq!(
            wire_phase(effective_phase(
                false,
                Some("Active"),
                9,
                Some(6),
                Some(8),
                Some(6)
            )),
            "Finalizing"
        );
    }

    #[test]
    fn live_prestart_or_incomplete_active_cycle_is_readiness_not_scorable() {
        assert_eq!(
            effective_phase(false, Some("Active"), 5, Some(6), Some(8), Some(6)),
            Some("ReadinessPending")
        );
        assert_eq!(
            effective_phase(false, Some("Active"), 6, None, Some(8), Some(6)),
            Some("ReadinessPending")
        );
        assert_eq!(
            effective_phase(false, Some("Active"), 6, Some(6), Some(8), None),
            Some("ReadinessPending")
        );
        assert_eq!(
            effective_phase(false, Some("Active"), 6, Some(6), Some(8), Some(7)),
            Some("ReadinessPending")
        );
        assert_eq!(
            effective_phase(false, Some("Active"), 7, Some(6), Some(8), Some(7)),
            Some("Active")
        );
    }

    #[test]
    fn historical_phase_remains_bound_to_snapshot_evidence() {
        assert_eq!(
            effective_phase(true, Some("Active"), 9, Some(6), Some(8), None),
            Some("Active")
        );
    }

    #[test]
    fn projected_transition_does_not_enable_retry_for_durable_active_cycle() {
        assert_eq!(
            effective_phase(false, Some("Active"), 9, Some(6), Some(8), Some(6)),
            Some("FinalizePending")
        );
        assert!(!durable_phase_can_retry(Some("Active")));
        assert!(durable_phase_can_retry(Some("FinalizePending")));
    }

    #[test]
    fn cached_live_lifecycle_is_bound_to_the_exact_round() {
        let mut views = HashMap::new();
        views.insert(41, KothLifecycleView::default());
        let cached = CachedLiveLifecycle {
            latest_round: 7,
            views,
        };

        assert!(cached_views_for_round(cached.clone(), 7).is_some());
        assert!(cached_views_for_round(cached.clone(), 8).is_none());

        let json = serde_json::to_vec(&cached).expect("lifecycle cache serializes");
        assert!(decode_live_lifecycle(&json, 7).is_some());
        assert!(decode_live_lifecycle(&json, 8).is_none());
    }

    #[test]
    fn every_hill_in_a_game_shares_one_live_lifecycle_key() {
        assert_eq!(live_lifecycle_cache_key(17), "kothlifecycle:17");
    }

    #[tokio::test]
    async fn lifecycle_transition_evicts_the_shared_live_projection() {
        let cache = InMemoryCache::new();
        let key = live_lifecycle_cache_key(17);
        cache.set(&key, b"cached", None).await;

        invalidate_live_lifecycle_cache(&cache, 17).await;

        assert!(cache.get(&key).await.is_none());
    }
}

pub(super) async fn load_lifecycle_map(
    st: &SharedState,
    game_id: i32,
    latest_round: i32,
    snapshot_cutoff: Option<DateTime<Utc>>,
) -> AppResult<HashMap<i32, KothLifecycleView>> {
    // Historical views are keyed by an exact timestamp and must remain bound to
    // immutable evidence. The outer frozen-board cache already coalesces them,
    // so only the heavily-polled live projection belongs in this shared cache.
    if snapshot_cutoff.is_some() {
        return query_lifecycle_map(st, game_id, latest_round, snapshot_cutoff).await;
    }

    let key = live_lifecycle_cache_key(game_id);
    if let Some(bytes) = st.cache.get(&key).await {
        if let Some(views) = decode_live_lifecycle(&bytes, latest_round) {
            return Ok(views);
        }
    }

    // Include the round in the flight identity even though the stored cache key
    // is game-wide. A request carrying a newly advanced round must never follow
    // an older round's in-flight fill; the serialized round tag also rejects an
    // old fill that races and overwrites a newer one.
    let flight_key = format!("{key}:{latest_round}");
    let st = st.clone();
    let key_for_fill = key.clone();
    let cached = LIVE_LIFECYCLE_SF
        .run(&flight_key, move || async move {
            if let Some(bytes) = st.cache.get(&key_for_fill).await {
                if let Ok(cached) = serde_json::from_slice::<CachedLiveLifecycle>(&bytes) {
                    if cached.latest_round == latest_round {
                        return Some(cached);
                    }
                }
            }

            let views = match query_lifecycle_map(&st, game_id, latest_round, None).await {
                Ok(views) => views,
                Err(error) => {
                    tracing::warn!(
                        game = game_id,
                        round = latest_round,
                        %error,
                        "KotH lifecycle cache fill failed"
                    );
                    return None;
                }
            };
            let cached = CachedLiveLifecycle {
                latest_round,
                views,
            };
            match serde_json::to_vec(&cached) {
                Ok(json) => {
                    st.cache
                        .set(&key_for_fill, &json, Some(LIVE_LIFECYCLE_CACHE_TTL))
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        game = game_id,
                        round = latest_round,
                        %error,
                        "KotH lifecycle cache serialization failed"
                    );
                }
            }
            Some(cached)
        })
        .await
        .ok_or_else(|| AppError::internal("KotH lifecycle cache fill failed"))?;
    cached_views_for_round(cached, latest_round)
        .ok_or_else(|| AppError::internal("KotH lifecycle cache round changed during fill"))
}

async fn query_lifecycle_map(
    st: &SharedState,
    game_id: i32,
    latest_round: i32,
    snapshot_cutoff: Option<DateTime<Utc>>,
) -> AppResult<HashMap<i32, KothLifecycleView>> {
    let snapshot = snapshot_cutoff.is_some();
    let rows = sqlx::query_as::<_, LifecycleRow>(
        r#"SELECT challenge.id AS challenge_id,
                  game.koth_cycle_ticks AS cycle_ticks,
                  game.koth_claim_confirmation_ticks AS claim_confirmation_ticks,
                  cycle.cycle_number, cycle.planned_start_round,
                  cycle.planned_end_round,
                  cycle.actual_start_round,
                  CASE WHEN $4::timestamptz IS NULL THEN cycle.phase
                       WHEN cycle.id IS NULL THEN NULL
                       ELSE COALESCE(historical_phase.phase, 'FinalizePending') END AS phase,
                  CASE WHEN $4::timestamptz IS NOT NULL
                            THEN historical.provisional_participation_id
                       ELSE cycle.provisional_participation_id END
                    AS provisional_participation_id,
                  provisional_team.name AS provisional_team_name,
                  CASE WHEN $4::timestamptz IS NOT NULL
                            THEN historical.confirmation_progress
                       ELSE cycle.confirmation_progress END AS confirmation_progress,
                  cycle.old_container_id,
                  cycle.replacement_container_id, cycle.reset_attempt,
                  cycle.readiness_failures, cycle.readiness_error,
                  cycle.last_error,
                  receipt.reset_receipt_id, receipt.scoring_receipt_id,
                  COALESCE(cooldown.participation_ids, ARRAY[]::integer[])
                    AS cooldown_participation_ids,
                  COALESCE(cooldown.team_names, ARRAY[]::text[])
                    AS cooldown_team_names,
                  COALESCE(cooldown.remaining_ticks, ARRAY[]::integer[])
                    AS cooldown_remaining_ticks,
                  historical.is_scorable AS historical_is_scorable
             FROM "GameChallenges" challenge
             JOIN "Games" game ON game.id = challenge.game_id
             LEFT JOIN LATERAL (
               SELECT crown.id, crown.cycle_number, crown.planned_start_round,
                      crown.planned_end_round,
                      crown.actual_start_round, crown.phase,
                      crown.provisional_participation_id,
                      crown.confirmation_progress, crown.old_container_id,
                      crown.replacement_container_id, crown.reset_attempt,
                      crown.readiness_failures, crown.readiness_error,
                      crown.last_error
                 FROM "KothCrownCycles" crown
                WHERE crown.game_id = challenge.game_id
                  AND crown.challenge_id = challenge.id
                  AND ($4::timestamptz IS NULL
                       OR $3 BETWEEN crown.planned_start_round
                                         AND crown.planned_end_round)
                  AND ($4::timestamptz IS NULL OR crown.created_at <= $4)
                ORDER BY crown.cycle_number DESC LIMIT 1
             ) cycle ON TRUE
             LEFT JOIN LATERAL (
               SELECT CASE audit.phase
                        WHEN 'FinalizePending' THEN 'SnapshotPending'
                        WHEN 'SnapshotPending' THEN 'DestroyPending'
                        WHEN 'DestroyPending' THEN 'CreatePending'
                        WHEN 'CreatePending' THEN 'PublishPending'
                        WHEN 'PublishPending' THEN 'CapabilityPending'
                        WHEN 'CapabilityPending' THEN 'ReadinessPending'
                        WHEN 'ReadinessPending' THEN 'FirewallPending'
                        WHEN 'FirewallPending' THEN 'Active'
                        WHEN 'Completed' THEN 'Completed'
                        WHEN 'Ended' THEN 'Ended'
                        ELSE audit.phase
                      END AS phase
                 FROM "KothCycleAuditReceipts" audit
                WHERE audit.cycle_id = cycle.id AND audit.created_at <= $4
                ORDER BY audit.created_at DESC, audit.id DESC LIMIT 1
             ) historical_phase ON $4::timestamptz IS NOT NULL
             LEFT JOIN LATERAL (
               SELECT result.provisional_participation_id,
                      result.confirmation_streak AS confirmation_progress,
                      result.is_scorable
                 FROM "KothControlResults" result
                 JOIN "AdRounds" round
                   ON round.id = result.ad_round_id
                  AND round.game_id = result.game_id
                WHERE result.game_id = challenge.game_id
                  AND result.challenge_id = challenge.id
                  AND result.cycle_id = cycle.id
                  AND round.number <= $3
                  AND result.checked_at <= $4
                ORDER BY round.number DESC, result.id DESC LIMIT 1
             ) historical ON $4::timestamptz IS NOT NULL
             LEFT JOIN LATERAL (
               SELECT MAX(audit.id) FILTER (
                        WHERE audit.phase <> 'FinalizePending'
                      ) AS reset_receipt_id,
                      MAX(audit.id) FILTER (
                        WHERE audit.phase = 'FinalizePending'
                      ) AS scoring_receipt_id
                 FROM "KothCycleAuditReceipts" audit
                WHERE audit.cycle_id = cycle.id
             ) receipt ON TRUE
             LEFT JOIN LATERAL (
               SELECT ARRAY_AGG(active.participation_id
                                ORDER BY active.participation_id)
                        AS participation_ids,
                      ARRAY_AGG(team.name ORDER BY active.participation_id)
                        AS team_names,
                      ARRAY_AGG(
                        GREATEST(active.expires_after_round - $3 + 1, 0)::integer
                        ORDER BY active.participation_id
                      ) AS remaining_ticks
                 FROM "KothCycleCooldowns" active
                 JOIN "KothCrownCycles" active_cycle
                   ON active_cycle.id = active.cycle_id
                 JOIN "Participations" participation
                   ON participation.id = active.participation_id
                  AND participation.game_id = active_cycle.game_id
                 JOIN "Teams" team ON team.id = participation.team_id
                WHERE active_cycle.game_id = challenge.game_id
                  AND active_cycle.challenge_id = challenge.id
                  AND active.starts_round <= $3
                  AND ($4::timestamptz IS NULL OR active.created_at <= $4)
                  AND (($4::timestamptz IS NOT NULL
                       AND active.expires_after_round >= $3
                       AND active.network_enforced_at IS NOT NULL
                       AND active.network_enforced_at <= $4
                       AND (active.network_released_at IS NULL
                            OR active.network_released_at > $4))
                       OR ($4::timestamptz IS NULL
                           AND active.network_enforced = TRUE
                           AND active.network_released_at IS NULL))
             ) cooldown ON TRUE
             LEFT JOIN "Participations" provisional
               ON provisional.id = CASE
                    WHEN $4::timestamptz IS NOT NULL
                      THEN historical.provisional_participation_id
                    ELSE cycle.provisional_participation_id
                  END
              AND provisional.game_id = challenge.game_id
             LEFT JOIN "Teams" provisional_team ON provisional_team.id = provisional.team_id
            WHERE challenge.game_id = $1 AND challenge."Type" = $2"#,
    )
    .bind(game_id)
    .bind(crate::utils::enums::ChallengeType::KingOfTheHill as i16)
    .bind(latest_round)
    .bind(snapshot_cutoff)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let visible_phase = effective_phase(
                snapshot,
                row.phase.as_deref(),
                latest_round,
                row.planned_start_round,
                row.planned_end_round,
                row.actual_start_round,
            );
            let phase_is_active = visible_phase == Some("Active");
            let is_scorable = if snapshot {
                row.historical_is_scorable.unwrap_or(false)
            } else {
                phase_is_active
            };
            let cycle_tick = if phase_is_active {
                let start = if snapshot {
                    row.planned_start_round
                } else {
                    row.actual_start_round
                };
                start.map_or(0, |start| {
                    (latest_round - start + 1).clamp(1, row.cycle_ticks)
                })
            } else {
                0
            };
            let next_reset_ticks = if phase_is_active {
                row.planned_end_round
                    .map(|end| (end - latest_round + 1).max(0))
            } else {
                None
            };
            let can_retry = durable_phase_can_retry(row.phase.as_deref());
            let challenge_id = row.challenge_id;
            let durable_phase = row
                .phase
                .clone()
                .unwrap_or_else(|| "Uninitialized".to_string());
            let cooldown_participants = row
                .cooldown_participation_ids
                .into_iter()
                .zip(row.cooldown_team_names)
                .zip(row.cooldown_remaining_ticks)
                .map(
                    |((participation_id, team_name), remaining_ticks)| KothCooldownParticipant {
                        participation_id,
                        team_name,
                        remaining_ticks,
                    },
                )
                .collect();
            (
                challenge_id,
                KothLifecycleView {
                    cycle_ticks: row.cycle_ticks,
                    claim_confirmation_ticks: row.claim_confirmation_ticks,
                    cycle_number: row.cycle_number.unwrap_or(0),
                    cycle_tick,
                    durable_phase,
                    reset_phase: wire_phase(visible_phase),
                    is_scorable,
                    next_reset_ticks,
                    provisional_participation_id: row.provisional_participation_id,
                    provisional_team_name: row.provisional_team_name,
                    confirmation_progress: row.confirmation_progress.unwrap_or(0),
                    cooldown_participants,
                    old_container_id: row.old_container_id,
                    replacement_container_id: row.replacement_container_id,
                    reset_attempt: row.reset_attempt.unwrap_or(0),
                    readiness_failures: row.readiness_failures.unwrap_or(0),
                    readiness_error: row.readiness_error.or(row.last_error),
                    can_retry,
                    reset_receipt_id: row.reset_receipt_id,
                    scoring_receipt_id: row.scoring_receipt_id,
                },
            )
        })
        .collect())
}
