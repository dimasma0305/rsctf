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
    pub(super) challenge_ids: Vec<i32>,
    pub(super) end_time_utc: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct RawOfficialConfig {
    scoring_start_round: i32,
    epoch_ticks: i32,
    cycle_ticks: i32,
    champion_cooldown_ticks: i32,
    roster_snapshot: serde_json::Value,
    hills_snapshot: serde_json::Value,
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
    pub(super) expose_port: i32,
    pub(super) allow_egress: bool,
    pub(super) checker_dir: Option<String>,
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

pub(super) async fn load_config(
    st: &SharedState,
    game_id: i32,
) -> AppResult<Option<OfficialConfig>> {
    let Some(raw) = sqlx::query_as::<_, RawOfficialConfig>(
        r#"SELECT config.scoring_start_round, config.epoch_ticks, config.cycle_ticks,
                  config.champion_cooldown_ticks,
                  config.roster_snapshot, config.hills_snapshot,
                  game.end_time_utc
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
        challenge_ids: snapshot_ids(&raw.hills_snapshot, "challengeId"),
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
    .ok_or_else(|| AppError::not_found("KotH crown cycle not found"))
}

pub(super) async fn load_hill_spec(st: &SharedState, cycle: &CycleRow) -> AppResult<HillSpec> {
    sqlx::query_as::<_, HillSpec>(
        r#"SELECT target.id AS target_id,
                  challenge.build_image_digest AS image,
                  COALESCE(challenge.memory_limit, 64) AS memory_limit,
                  COALESCE(challenge.cpu_count, 1) AS cpu_count,
                  COALESCE(challenge.expose_port, 80) AS expose_port,
                  challenge.ad_allow_egress AS allow_egress,
                  NULLIF(BTRIM(challenge.ad_checker_image), '') AS checker_dir
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
            "Crown-cycle KotH requires a platform-hosted hill with a configured image",
        )
    })
}
