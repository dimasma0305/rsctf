use chrono::{DateTime, Utc};
use sqlx::FromRow;

use crate::app_state::SharedState;
use crate::utils::enums::{ChallengeBuildStatus, ChallengeReviewStatus, ChallengeType};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug)]
pub(crate) struct OfficialConfig {
    pub(super) scoring_start_round: i32,
    pub(super) epoch_ticks: i32,
    pub(super) cycle_ticks: i32,
    pub(super) champion_cooldown_ticks: i32,
    pub(super) roster: Vec<i32>,
    pub(super) hills: Vec<OfficialHill>,
    pub(super) start_time_utc: DateTime<Utc>,
    pub(super) end_time_utc: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HillLifecycle {
    ScheduledCrown,
    PersistentArena,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct OfficialHill {
    pub(super) challenge_id: i32,
    pub(super) lifecycle: HillLifecycle,
}

impl OfficialConfig {
    /// A persistent arena needs a finite round fence for the existing evidence
    /// foreign keys, but it must not inherit the Boot2Root reset cadence. The
    /// shortest valid configured round is 30 seconds, so the complete event
    /// duration is a conservative upper bound even when the scheduler
    /// reanchors after downtime. Re-reading the live event deadline lets an
    /// organizer extend an event without creating a scheduled arena reset.
    pub(super) fn persistent_end_round(&self) -> i32 {
        const MINIMUM_ROUND_SECONDS: i64 = 30;

        let event_seconds = self
            .end_time_utc
            .signed_duration_since(self.start_time_utc)
            .num_seconds()
            .max(0);
        let maximum_rounds =
            event_seconds.saturating_add(MINIMUM_ROUND_SECONDS - 1) / MINIMUM_ROUND_SECONDS + 1;
        self.scoring_start_round
            .saturating_add(i32::try_from(maximum_rounds).unwrap_or(i32::MAX))
    }
}

#[derive(Debug, FromRow)]
struct RawOfficialConfig {
    scoring_start_round: i32,
    epoch_ticks: i32,
    cycle_ticks: i32,
    champion_cooldown_ticks: i32,
    roster_snapshot: serde_json::Value,
    hills_snapshot: serde_json::Value,
    start_time_utc: DateTime<Utc>,
    end_time_utc: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct CycleRow {
    pub(super) id: i64,
    pub(super) game_id: i32,
    pub(super) challenge_id: i32,
    pub(super) cycle_number: i32,
    pub(super) phase: String,
    pub(super) planned_start_round: i32,
    pub(super) old_container_id: Option<String>,
    pub(super) replacement_container_id: Option<String>,
    pub(super) replacement_host: Option<String>,
    pub(super) replacement_port: Option<i32>,
    pub(super) expected_image: String,
    pub(super) reset_attempt: i32,
    pub(super) readiness_attempt: i32,
}

#[derive(Debug, FromRow)]
pub(super) struct HillSpec {
    pub(super) target_id: i32,
    pub(super) image: String,
    pub(super) memory_limit: i32,
    pub(super) cpu_count: i32,
    pub(super) storage_limit: i32,
    pub(super) expose_port: i32,
    pub(super) allow_egress: bool,
    pub(super) checker_dir: Option<String>,
    pub(super) runtime_flag: Option<String>,
}

pub(super) fn snapshot_ids(snapshot: &serde_json::Value, object_key: &str) -> Vec<i32> {
    snapshot
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_i64().or_else(|| value.get(object_key)?.as_i64()))
        .filter_map(|value| i32::try_from(value).ok())
        .collect()
}

fn snapshot_hills(snapshot: &serde_json::Value) -> AppResult<Vec<OfficialHill>> {
    snapshot
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| {
            let challenge_id = value
                .as_i64()
                .or_else(|| value.get("challengeId")?.as_i64())
                .and_then(|value| i32::try_from(value).ok())?;
            let claim_source = value
                .get("claimSource")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Marker");
            Some((challenge_id, claim_source))
        })
        .map(|(challenge_id, claim_source)| {
            let lifecycle = match claim_source {
                "Marker" => HillLifecycle::ScheduledCrown,
                "Api" => HillLifecycle::PersistentArena,
                source => {
                    return Err(AppError::internal(format!(
                        "unsupported snapshotted KotH claim source {source:?}"
                    )))
                }
            };
            Ok(OfficialHill {
                challenge_id,
                lifecycle,
            })
        })
        .collect()
}

pub(super) async fn load_config(
    st: &SharedState,
    game_id: i32,
) -> AppResult<Option<OfficialConfig>> {
    let Some(raw) = sqlx::query_as::<_, RawOfficialConfig>(
        r#"SELECT config.scoring_start_round, config.epoch_ticks, config.cycle_ticks,
                  config.champion_cooldown_ticks,
                  config.roster_snapshot, config.hills_snapshot,
                  game.start_time_utc, game.end_time_utc
             FROM "KothOfficialConfigs" config
             JOIN "Games" game ON game.id = config.game_id
            WHERE config.game_id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(OfficialConfig {
        scoring_start_round: raw.scoring_start_round,
        epoch_ticks: raw.epoch_ticks,
        cycle_ticks: raw.cycle_ticks,
        champion_cooldown_ticks: raw.champion_cooldown_ticks,
        roster: snapshot_ids(&raw.roster_snapshot, "participationId"),
        hills: snapshot_hills(&raw.hills_snapshot)?,
        start_time_utc: raw.start_time_utc,
        end_time_utc: raw.end_time_utc,
    }))
}

pub(super) async fn load_cycle(st: &SharedState, cycle_id: i64) -> AppResult<CycleRow> {
    sqlx::query_as::<_, CycleRow>(
        r#"SELECT id, game_id, challenge_id, cycle_number, phase,
                  planned_start_round, old_container_id,
                  replacement_container_id, replacement_host,
                  replacement_port, expected_image, reset_attempt,
                  readiness_attempt
             FROM "KothCrownCycles" WHERE id = $1"#,
    )
    .bind(cycle_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH runtime lifecycle not found"))
}

pub(super) async fn load_hill_spec(st: &SharedState, cycle: &CycleRow) -> AppResult<HillSpec> {
    sqlx::query_as::<_, HillSpec>(
        r#"SELECT target.id AS target_id,
                  challenge.build_image_digest AS image,
                  COALESCE(challenge.memory_limit, 64) AS memory_limit,
                  COALESCE(challenge.cpu_count, 1) AS cpu_count,
                  COALESCE(challenge.storage_limit, 512) AS storage_limit,
                  COALESCE(challenge.expose_port, 80) AS expose_port,
                  challenge.ad_allow_egress AS allow_egress,
                  NULLIF(BTRIM(challenge.ad_checker_image), '') AS checker_dir,
                  (SELECT flag.flag
                     FROM "FlagContexts" flag
                    WHERE flag.challenge_id = challenge.id
                    ORDER BY flag.id
                    LIMIT 1) AS runtime_flag
             FROM "GameChallenges" challenge
             JOIN "KothTargets" target
               ON target.game_id = challenge.game_id
              AND target.challenge_id = challenge.id
            WHERE challenge.game_id = $1 AND challenge.id = $2
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $3
              AND challenge."Type" = $4
              AND challenge.build_status = $5
              AND NULLIF(BTRIM(challenge.build_image_digest), '') IS NOT NULL"#,
    )
    .bind(cycle.game_id)
    .bind(cycle.challenge_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .bind(ChallengeBuildStatus::Success as i16)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| {
        AppError::bad_request(
            "Managed KotH requires a platform-hosted hill with a configured image",
        )
    })
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;

    use super::{snapshot_hills, HillLifecycle, OfficialConfig, OfficialHill};

    #[test]
    fn frozen_claim_source_selects_only_the_required_lifecycle() {
        assert_eq!(
            snapshot_hills(&json!([
                {"challengeId": 9, "claimSource": "Api"},
                {"challengeId": 10, "claimSource": "Marker"},
                11
            ]))
            .unwrap(),
            vec![
                OfficialHill {
                    challenge_id: 9,
                    lifecycle: HillLifecycle::PersistentArena,
                },
                OfficialHill {
                    challenge_id: 10,
                    lifecycle: HillLifecycle::ScheduledCrown,
                },
                OfficialHill {
                    challenge_id: 11,
                    lifecycle: HillLifecycle::ScheduledCrown,
                },
            ]
        );
        assert!(snapshot_hills(&json!([{
            "challengeId": 12,
            "claimSource": "Unknown"
        }]))
        .is_err());
    }

    #[test]
    fn persistent_arena_round_fence_spans_the_complete_event() {
        let start = Utc.with_ymd_and_hms(2026, 8, 12, 0, 0, 0).unwrap();
        let config = OfficialConfig {
            scoring_start_round: 7,
            epoch_ticks: 24,
            cycle_ticks: 12,
            champion_cooldown_ticks: 1,
            roster: vec![1, 2],
            hills: Vec::new(),
            start_time_utc: start,
            end_time_utc: start + Duration::hours(6),
        };

        assert_eq!(config.persistent_end_round(), 728);
    }
}
