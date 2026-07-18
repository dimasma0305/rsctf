//! KotH epoch-score timeline models, cache, and read endpoint.

use std::collections::HashMap;

use axum::extract::{Path, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::MaybeUser;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

use super::board::compute_koth_board;
use super::KOTH_DETAIL_EPOCH_LIMIT;

#[derive(sqlx::FromRow)]
struct TimelineRoundRow {
    number: i32,
    start_time_utc: DateTime<Utc>,
    end_time_utc: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KothTimelinePoint {
    pub round: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    pub score: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KothTeamTimeline {
    pub participation_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub division: Option<String>,
    pub items: Vec<KothTimelinePoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KothScoreTimelineModel {
    pub latest_round: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub ends_at: Option<DateTime<Utc>>,
    pub teams: Vec<KothTeamTimeline>,
}
static KOTH_TIMELINE_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<KothScoreTimelineModel>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

/// Return the same bounded epoch score used by the leaderboard, sampled at each
/// epoch boundary. The removed additive hold-credit total is never exposed as a
/// second scoring model.
pub async fn timeline(
    State(st): State<SharedState>,
    MaybeUser(maybe): MaybeUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<KothScoreTimelineModel>> {
    let game = crate::controllers::game::load_game_cached(&st, game_id).await?;
    if game.hidden {
        return Err(AppError::not_found("Game not found"));
    }
    let is_monitor = maybe.as_ref().is_some_and(|user| user.is_monitor());
    let key = if is_monitor {
        format!("_KothTimeline_{game_id}")
    } else {
        format!("_KothTimelineFrozen_{game_id}")
    };
    if let Some(bytes) = st.cache.get(&key).await {
        if let Ok(model) = serde_json::from_slice::<KothScoreTimelineModel>(&bytes) {
            return Ok(RequestResponse::ok(model));
        }
    }
    let st_for_fill = st.clone();
    let game_for_fill = game.clone();
    let key_for_fill = key.clone();
    let model = KOTH_TIMELINE_SF
        .run(&key, move || async move {
            if let Some(bytes) = st_for_fill.cache.get(&key_for_fill).await {
                if let Ok(model) = serde_json::from_slice::<KothScoreTimelineModel>(&bytes) {
                    return Some(model);
                }
            }
            let model =
                match build_timeline_model(&st_for_fill, &game_for_fill, game_id, is_monitor).await
                {
                    Ok(model) => model,
                    Err(error) => {
                        tracing::warn!(game = game_id, %error, "KotH timeline cache fill failed");
                        return None;
                    }
                };
            let json = match serde_json::to_vec(&model) {
                Ok(json) => json,
                Err(error) => {
                    tracing::warn!(game = game_id, %error, "KotH timeline serialization failed");
                    return None;
                }
            };
            st_for_fill
                .cache
                .set(
                    &key_for_fill,
                    &json,
                    Some(std::time::Duration::from_secs(5)),
                )
                .await;
            Some(model)
        })
        .await
        .ok_or_else(|| AppError::internal("KotH timeline cache fill failed"))?;
    Ok(RequestResponse::ok(model))
}

async fn build_timeline_model(
    st: &SharedState,
    game: &crate::models::data::game::Model,
    game_id: i32,
    is_monitor: bool,
) -> AppResult<KothScoreTimelineModel> {
    let now = Utc::now();
    let event_ended = now >= game.end_time_utc;
    let mut cutoff = match game.freeze_time_utc {
        Some(freeze) if !is_monitor && now >= freeze && now < game.end_time_utc => Some(freeze),
        _ => None,
    };
    if event_ended {
        cutoff = Some(cutoff.map_or(game.end_time_utc, |value| value.min(game.end_time_utc)));
    }

    let board = compute_koth_board(st, game_id, cutoff, false).await?;
    let latest_round = board.latest_round;
    let mut wanted_rounds = Vec::with_capacity(KOTH_DETAIL_EPOCH_LIMIT + 2);
    if let Some(start_round) = board.scoring_start_round {
        wanted_rounds.push(start_round);
    }
    if latest_round > 0 {
        wanted_rounds.push(latest_round);
    }
    if let Some(score) = board.scoring.teams.values().next() {
        for epoch in score.epochs.iter().rev().take(KOTH_DETAIL_EPOCH_LIMIT) {
            let nominal_end = board
                .scoring_start_round
                .unwrap_or(1)
                .saturating_add(epoch.epoch.saturating_mul(board.epoch_ticks))
                .saturating_sub(1);
            wanted_rounds.push(nominal_end.min(latest_round));
        }
    }
    wanted_rounds.sort_unstable();
    wanted_rounds.dedup();

    let round_rows = if wanted_rounds.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, TimelineRoundRow>(
            r#"SELECT number, start_time_utc, end_time_utc
                FROM "AdRounds"
                WHERE game_id = $1 AND number = ANY($2)
                  AND ($3::timestamptz IS NULL
                       OR (NOT $4 AND start_time_utc <= $3)
                       OR ($4 AND start_time_utc < $3))"#,
        )
        .bind(game_id)
        .bind(&wanted_rounds)
        .bind(cutoff)
        .bind(event_ended)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
    };
    let round_times: HashMap<i32, TimelineRoundRow> = round_rows
        .into_iter()
        .map(|round| (round.number, round))
        .collect();
    let started_at = board
        .scoring_start_round
        .and_then(|round| round_times.get(&round))
        .map(|round| round.start_time_utc);
    let ends_at = round_times
        .get(&latest_round)
        .map(|round| round.end_time_utc)
        .or(board.current_round_ends_at);

    let mut teams = Vec::with_capacity(board.roster.len());
    for member in &board.roster {
        let mut items = Vec::new();
        if let Some(score) = board.scoring.teams.get(&member.participation_id) {
            let recent: Vec<_> = score
                .epochs
                .iter()
                .rev()
                .take(KOTH_DETAIL_EPOCH_LIMIT)
                .collect();
            for epoch in recent.into_iter().rev() {
                let nominal_end = board
                    .scoring_start_round
                    .unwrap_or(1)
                    .saturating_add(epoch.epoch.saturating_mul(board.epoch_ticks))
                    .saturating_sub(1);
                let round = nominal_end.min(latest_round);
                let Some(time) = round_times.get(&round).map(|value| value.end_time_utc) else {
                    continue;
                };
                items.push(KothTimelinePoint {
                    round,
                    time,
                    score: if epoch.cumulative_epoch_weight > 0.0 {
                        epoch.cumulative_points_numerator / epoch.cumulative_epoch_weight
                    } else {
                        0.0
                    },
                });
            }
        }
        teams.push(KothTeamTimeline {
            participation_id: member.participation_id,
            team_id: member.team_id,
            team_name: member.team_name.clone(),
            division: member.division.clone(),
            items,
        });
    }
    teams.sort_by_key(|team| team.participation_id);

    Ok(KothScoreTimelineModel {
        latest_round,
        started_at,
        ends_at,
        teams,
    })
}
