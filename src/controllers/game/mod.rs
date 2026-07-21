//! Ported from RSCTF `Controllers/GameController.cs` (player-facing surface) plus
//! `Services/FlagChecker.cs` and the Game/Participation/Submission/GameInstance
//! repositories.
//!
//! Route prefix `/api/game`. Covers game listing/details, notices/events,
//! participations, scoreboard, join, the challenge view, flag SUBMISSION (judged
//! synchronously here — rsctf has no background channel worker, so the logic of
//! `GameInstanceRepository.VerifyAnswer` runs inline in `submit`), submission
//! status, and container lifecycle. Cheat / traffic-capture / writeup routes are
//! registered and return well-typed empty payloads — those belong to the
//! cheat-detection and traffic subsystems, not yet ported.

pub mod ad;
pub mod koth;

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::middlewares::rate_limiter::{limited, Policy};
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::{DateTime, Utc};
use rust_xlsxwriter::Workbook;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{CurrentUser, MaybeUser, MonitorUser};
use crate::models::data::{
    attachment, challenge_review, container, division, division_challenge_config, flag_context,
    game, game_challenge, game_event, game_instance, game_manager, game_notice, local_file,
    participation, submission, suspicion_event, team, team_member, user, user_participation,
};
use crate::utils::crypto_utils::ct_eq;
use crate::utils::enums::{
    AnswerResult, ChallengeCategory, ChallengeReviewStatus, ChallengeType, ContainerStatus,
    EventType, FileType, GamePermission, NoticeType, ParticipationStatus, ReviewRating, ScoreCurve,
    SubmissionType,
};
use crate::utils::error::{AppError, AppResult};
use crate::utils::flag_generator;
use crate::utils::shared::{ArrayResponse, MessageResponse, PageParams, RequestResponse};

/// RSCTF `Limits.MaxFlagLength`.
const MAX_FLAG_LENGTH: usize = 127;

// ---------------------------------------------------------------------------
// DTOs (inline; camelCase on the wire to match RSCTF's JSON contract).
// ---------------------------------------------------------------------------

/// RSCTF `BasicGameInfoModel`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BasicGameInfoModel {
    pub id: i32,
    pub title: String,
    pub summary: String,
    pub poster: Option<String>,
    pub limit: i32,
    pub team_count: i32,
    pub user_count: i32,
    pub average_rating: f64,
    pub review_count: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub start: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end: DateTime<Utc>,
}

impl From<&game::Model> for BasicGameInfoModel {
    fn from(g: &game::Model) -> Self {
        Self {
            id: g.id,
            title: g.title.clone(),
            summary: g.summary.clone(),
            poster: g.poster_url(),
            limit: g.team_member_count_limit,
            team_count: 0,
            user_count: 0,
            average_rating: 0.0,
            review_count: 0,
            start: g.start_time_utc,
            end: g.end_time_utc,
        }
    }
}

/// RSCTF `DivisionInfo`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DivisionInfo {
    pub id: i32,
    pub name: String,
    pub invite_code_required: bool,
}

/// One challenge as shown in the game's challenge panel.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeBrief {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub score: i32,
    pub solved: bool,
}

/// RSCTF `DetailedGameInfoModel` (with `WithParticipation`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailedGameInfoModel {
    pub id: i32,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub hidden: bool,
    pub divisions: Option<Vec<DivisionInfo>>,
    pub invite_code_required: bool,
    pub writeup_required: bool,
    pub poster: Option<String>,
    pub limit: i32,
    pub team_count: i64,
    pub division: Option<i32>,
    pub team_name: Option<String>,
    pub practice_mode: bool,
    pub allow_user_submissions: bool,
    pub status: ParticipationStatus,
    /// Category (int-string) -> challenge briefs, for accepted participants
    /// (RSCTF surfaces the challenge set on the game detail).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenges: Option<BTreeMap<String, Vec<ChallengeBrief>>>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub start: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end: DateTime<Utc>,
}

/// RSCTF `GameNotice` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameNoticeModel {
    pub id: i32,
    #[serde(rename = "type")]
    pub notice_type: NoticeType,
    pub values: Json,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
}

/// RSCTF `GameEvent` response (`FormattableDataOfEventType` + time/user/team).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameEventModel {
    #[serde(rename = "type")]
    pub event_type: crate::utils::enums::EventType,
    pub values: Json,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

/// RSCTF `TeamWithDetailedUserInfo`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamWithDetailedUserInfo {
    pub id: i32,
    pub locked: bool,
    pub captain_id: Uuid,
    pub name: Option<String>,
    pub bio: Option<String>,
    pub avatar: Option<String>,
    pub members: Vec<Json>,
}

/// RSCTF `ParticipationInfoModel` (Admin review).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationInfoModel {
    pub id: i32,
    pub team: TeamWithDetailedUserInfo,
    /// User-id GUIDs of the members registered for this participation
    /// (RSCTF `part.Members.Select(m => m.UserId)`).
    pub registered_members: Vec<Uuid>,
    pub division_id: Option<i32>,
    pub status: ParticipationStatus,
}

/// RSCTF `ChallengeItem` (a solved cell on the scoreboard).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeItem {
    pub id: i32,
    pub score: i32,
    #[serde(rename = "type")]
    pub submission_type: SubmissionType,
    pub user_name: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
}

/// RSCTF `ScoreboardItem`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreboardItem {
    pub id: i32,
    pub name: String,
    pub bio: Option<String>,
    pub division_id: Option<i32>,
    pub avatar: Option<String>,
    pub score: i64,
    pub rank: i32,
    pub division_rank: Option<i32>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub last_submission_time: DateTime<Utc>,
    pub solved_challenges: Vec<ChallengeItem>,
    pub solved_count: usize,
}

/// RSCTF `Blood`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blood {
    pub id: i32,
    pub name: String,
    pub avatar: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub submit_time_utc: Option<DateTime<Utc>>,
}

/// RSCTF `ChallengeInfo` (scoreboard column / game detail).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeInfo {
    pub id: i32,
    pub title: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub score: i32,
    pub solved: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub deadline: Option<DateTime<Utc>>,
    pub bloods: Vec<Blood>,
    pub disable_blood_bonus: bool,
}

/// RSCTF `ScoreboardModel`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreboardModel {
    #[serde(with = "crate::utils::datetime::millis")]
    pub update_time_utc: DateTime<Utc>,
    pub blood_bonus: i64,
    pub timelines: Vec<Json>,
    pub items: Vec<ScoreboardItem>,
    pub divisions: Vec<Json>,
    pub challenges: BTreeMap<String, Vec<ChallengeInfo>>,
    pub challenge_count: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub freeze: Option<DateTime<Utc>>,
    pub is_frozen_view: bool,
}

/// RSCTF `ChallengeSolverModel` — one team's solve of a single challenge, for the
/// challenge modal's solver list. Field names mirror the client's local interface
/// (`rank`/`teamName`/`teamAvatar`/`userName`/`type`/`time`/`score`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeSolverModel {
    pub rank: i32,
    pub team_name: String,
    pub team_avatar: Option<String>,
    pub user_name: Option<String>,
    #[serde(rename = "type")]
    pub submission_type: SubmissionType,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    pub score: i32,
}

/// RSCTF `GameDetailModel` (`/details`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDetailModel {
    pub challenges: BTreeMap<String, Vec<ChallengeInfo>>,
    pub challenge_count: i32,
    pub rank: Option<ScoreboardItem>,
    pub team_token: String,
    pub writeup_required: bool,
    #[serde(with = "crate::utils::datetime::millis")]
    pub writeup_deadline: DateTime<Utc>,
}

/// RSCTF `JoinedTeam`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinedTeam {
    pub id: i32,
    pub division: i32,
}

/// RSCTF `GameJoinCheckInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameJoinCheckInfoModel {
    pub joined_teams: Vec<JoinedTeam>,
    pub joinable_divisions: Vec<i32>,
}

/// RSCTF `ClientFlagContext`.
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientFlagContext {
    pub instance_entry: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub close_time: Option<DateTime<Utc>>,
    pub is_shared_instance: bool,
    pub url: Option<String>,
    pub file_size: Option<i64>,
}

/// Port of RSCTF `GameChallenge.UsesSharedContainer`: true when a challenge serves
/// ONE challenge-owned container to every team — a `StaticContainer` with
/// `enable_shared_container` and a valid image/port. Such a challenge never gets a
/// per-team `GameInstance`/container; the single shared container's id lives on
/// `game_challenge.shared_container_id`.
pub(crate) fn uses_shared_container(c: &game_challenge::Model) -> bool {
    c.challenge_type == ChallengeType::StaticContainer
        && c.enable_shared_container
        && crate::services::challenge_workloads::has_runtime(c)
}

/// RSCTF `ChallengeDetailModel` (player view).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeDetailModel {
    pub id: i32,
    pub title: String,
    pub content: String,
    pub category: ChallengeCategory,
    #[serde(rename = "type")]
    pub challenge_type: ChallengeType,
    pub hints: Option<Json>,
    pub score: i32,
    pub context: ClientFlagContext,
    pub limit: i32,
    pub attempts: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub deadline: Option<DateTime<Utc>>,
    pub user_rating: ReviewRating,
    pub user_comment: Option<String>,
}

/// RSCTF `ContainerInfoModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerInfoModel {
    pub id: String,
    pub status: ContainerStatus,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub expect_stop_at: DateTime<Utc>,
    pub entry: String,
}

impl From<&container::Model> for ContainerInfoModel {
    fn from(c: &container::Model) -> Self {
        Self {
            id: c.id.to_string(),
            status: c.status,
            started_at: c.started_at,
            expect_stop_at: c.expect_stop_at,
            entry: c.entry(),
        }
    }
}

/// RSCTF `Submission` (monitor feed).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionModel {
    pub answer: String,
    pub status: AnswerResult,
    #[serde(with = "crate::utils::datetime::millis")]
    pub time: DateTime<Utc>,
    pub user: Option<String>,
    pub team: Option<String>,
    pub challenge: Option<String>,
}

/// RSCTF `BasicWriteupInfoModel`.
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BasicWriteupInfoModel {
    pub submitted: bool,
    pub name: String,
    pub file_size: i64,
    pub note: String,
}

/// RSCTF `CheatReport` (cheat-detection subsystem — empty until ported).
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CheatReport {
    #[serde(with = "crate::utils::datetime::millis")]
    pub generated_at: DateTime<Utc>,
    pub ip_analysis: Vec<Json>,
    pub abnormal_solves: Vec<Json>,
    pub collusion_groups: Vec<Json>,
    pub suspicion_list: Vec<Json>,
    pub identity_overlaps: Vec<Json>,
}

/// RSCTF `CollusionCompareResult`.
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CollusionCompareResult {
    pub rsi: f64,
    pub details: Vec<Json>,
}

/// RSCTF `TeamModel` (compact team info embedded in a `ParticipationModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamModel {
    pub id: i32,
    pub name: Option<String>,
    pub avatar: Option<String>,
}

/// RSCTF `ParticipationModel` (team participation info, cheat-report side).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParticipationModel {
    pub id: i32,
    pub team: TeamModel,
    pub status: ParticipationStatus,
    pub division: Option<String>,
    pub division_id: Option<i32>,
}

/// RSCTF `CheatInfoModel` — one detected flag-sharing incident.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatInfoModel {
    /// Team that owns the (per-team dynamic) flag that was shared.
    pub owned_team: ParticipationModel,
    /// Team that submitted the other team's flag.
    pub submit_team: ParticipationModel,
    /// The offending submission.
    pub submission: SubmissionModel,
}

/// RSCTF `TrafficFlowDetail` (extends `TrafficFlowSummary`).
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TrafficFlowDetail {
    pub connection_port: i32,
    pub first_seen_utc: String,
    pub last_seen_utc: String,
    pub peer_ip: String,
    pub packets_in: i64,
    pub packets_out: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
    pub flag_hits: i64,
    pub chunks: Vec<Json>,
}

/// RSCTF `GameJoinModel`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameJoinModel {
    pub team_id: i32,
    #[serde(default)]
    pub division_id: Option<i32>,
    #[serde(default)]
    pub invite_code: Option<String>,
}

/// RSCTF `FlagSubmitModel`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagSubmitModel {
    pub flag: String,
}

/// RSCTF `ChallengeReviewModel`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeReviewModel {
    #[serde(default)]
    pub rating: Option<i32>,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentQuery {
    #[serde(default)]
    pub limit: usize,
}

/// RSCTF `GameController.Events` query: `hideContainer`/`count`/`skip`/`search`.
/// Events has no `type` filter (that belongs to `Submissions`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventQuery {
    #[serde(default)]
    pub hide_container: bool,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub skip: Option<u64>,
    #[serde(default)]
    pub search: Option<String>,
}

/// RSCTF `GetChallengeSolvers` takes no paging; rsctf adds optional `count`/`skip`
/// (count omitted or 0 ⇒ the whole solver list).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolversQuery {
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub skip: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionQuery {
    #[serde(default, rename = "type")]
    pub type_filter: Option<String>,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub skip: Option<u64>,
    #[serde(default)]
    pub search: Option<String>,
}

pub fn router() -> Router<SharedState> {
    routes::router()
}

/// Player API for a stateless web replica. Process-local BYOC routes are
/// hosted only by the singleton network/control router.
pub fn web_router() -> Router<SharedState> {
    routes::web_router()
}

mod routes;

/// Build the challenge column map (category name -> [ChallengeInfo]) plus count.
fn build_challenges_map(
    list: &[game_challenge::Model],
) -> (BTreeMap<String, Vec<ChallengeInfo>>, i32) {
    let count = list.len() as i32;
    let mut map: BTreeMap<String, Vec<ChallengeInfo>> = BTreeMap::new();
    for c in list {
        // Key by the serde WIRE string ("PPC"/"AI"/"OSINT"), not Rust Debug
        // ("Ppc"/"Ai"/"Osint") — the React client uses this map key as a
        // ChallengeCategory to look up the tab/column icon+label+color, so a
        // Debug-cased key misses the map and renders a blank category.
        let key = serde_json::to_value(c.category)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        map.entry(key).or_default().push(ChallengeInfo {
            id: c.id,
            title: c.title.clone(),
            category: c.category,
            challenge_type: c.challenge_type,
            score: c.original_score,
            solved: c.accepted_count,
            deadline: c.deadline_utc,
            bloods: Vec::new(),
            disable_blood_bonus: c.disable_blood_bonus,
        });
    }
    (map, count)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The verified play context for a participant action.
struct ContextInfo {
    game: game::Model,
    participation: participation::Model,
}

/// Mirror of RSCTF `GetContextInfo`: game exists, user is an Accepted
/// participant, the game has started, and (optionally) has not ended.
async fn context_info(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
    deny_after_ended: bool,
) -> AppResult<ContextInfo> {
    let game = load_game_cached(st, game_id).await?;

    let part = find_participation(st, user.id, game_id)
        .await?
        .ok_or_else(|| AppError::bad_request("Not participating in this game"))?;

    if part.status != ParticipationStatus::Accepted {
        return Err(AppError::bad_request("Participation not accepted"));
    }
    if Utc::now() < game.start_time_utc {
        return Err(AppError::game_not_started());
    }
    if deny_after_ended && !game.practice_mode && game.end_time_utc < Utc::now() {
        return Err(AppError::game_ended());
    }

    Ok(ContextInfo {
        game,
        participation: part,
    })
}

/// Resolve a challenge permission; dangling/cross-game divisions fail closed.
pub(crate) async fn effective_permission(
    st: &SharedState,
    part: &participation::Model,
    challenge_id: i32,
) -> AppResult<GamePermission> {
    let Some(div_id) = part.division_id else {
        return Ok(GamePermission(GamePermission::ALL));
    };

    let cache_key = format!("effperm:v3:{}:{div_id}:{challenge_id}", part.game_id);
    if let Some(bytes) = st.cache.get(&cache_key).await {
        if let Ok(perm) = serde_json::from_slice::<GamePermission>(&bytes) {
            return Ok(perm);
        }
    }

    let stored: Option<i32> = sqlx::query_scalar(
        r#"SELECT COALESCE(permission.permissions, division.default_permissions)
             FROM "Divisions" division
             LEFT JOIN "DivisionChallengeConfigs" permission
               ON permission.division_id = division.id
              AND permission.challenge_id = $3
            WHERE division.id = $1 AND division.game_id = $2"#,
    )
    .bind(div_id)
    .bind(part.game_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let perm = GamePermission(stored.unwrap_or(0));

    if let Ok(json) = serde_json::to_vec(&perm) {
        st.cache
            .set(&cache_key, &json, Some(std::time::Duration::from_secs(10)))
            .await;
    }

    Ok(perm)
}

/// Batched permission resolution for the polled `/details` path.
async fn effective_permissions_batch(
    st: &SharedState,
    part: &participation::Model,
    challenge_ids: &[i32],
) -> AppResult<std::collections::HashMap<i32, GamePermission>> {
    let Some(div_id) = part.division_id else {
        return Ok(challenge_ids
            .iter()
            .map(|&id| (id, GamePermission(GamePermission::ALL)))
            .collect());
    };

    // Resolve the parent first: missing is distinct from a real zero-bit default.
    let default_key = format!("div_default:v3:{}:{div_id}", part.game_id);
    let default: Option<i32> = if let Some(bytes) = st.cache.get(&default_key).await {
        serde_json::from_slice(&bytes).unwrap_or(None)
    } else {
        let db_default: Option<i32> = sqlx::query_scalar(
            r#"SELECT default_permissions FROM "Divisions"
                WHERE id = $1 AND game_id = $2"#,
        )
        .bind(div_id)
        .bind(part.game_id)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if let Ok(json) = serde_json::to_vec(&db_default) {
            st.cache
                .set(
                    &default_key,
                    &json,
                    Some(std::time::Duration::from_secs(10)),
                )
                .await;
        }
        db_default
    };
    let Some(default) = default else {
        return Ok(challenge_ids
            .iter()
            .map(|&id| (id, GamePermission(0)))
            .collect());
    };

    let overrides_key = format!("div_overrides:v3:{}:{div_id}", part.game_id);
    let overrides: std::collections::HashMap<i32, i32> =
        if let Some(bytes) = st.cache.get(&overrides_key).await {
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            let db_overrides: std::collections::HashMap<i32, i32> =
                division_challenge_config::Entity::find()
                    .filter(division_challenge_config::Column::DivisionId.eq(div_id))
                    .all(&st.db)
                    .await?
                    .into_iter()
                    .map(|c| (c.challenge_id, c.permissions))
                    .collect();
            if let Ok(json) = serde_json::to_vec(&db_overrides) {
                st.cache
                    .set(
                        &overrides_key,
                        &json,
                        Some(std::time::Duration::from_secs(10)),
                    )
                    .await;
            }
            db_overrides
        };

    Ok(challenge_ids
        .iter()
        .map(|&id| {
            (
                id,
                GamePermission(overrides.get(&id).copied().unwrap_or(default)),
            )
        })
        .collect())
}

fn parse_answer_result(name: &str) -> Option<AnswerResult> {
    match name {
        "FlagSubmitted" => Some(AnswerResult::FlagSubmitted),
        "Accepted" => Some(AnswerResult::Accepted),
        "WrongAnswer" => Some(AnswerResult::WrongAnswer),
        "CheatDetected" => Some(AnswerResult::CheatDetected),
        "NotFound" => Some(AnswerResult::NotFound),
        _ => None,
    }
}

async fn load_game(st: &SharedState, id: i32) -> AppResult<game::Model> {
    game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))
}

/// Load a challenge through the player-visible boundary. Player actions must not
/// expose disabled or unapproved challenge material even when its numeric id is
/// guessed directly.
pub(crate) async fn load_playable_challenge(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<game_challenge::Model> {
    game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))
}

/// Load a challenge scoped to its game without applying player visibility. This
/// is used only for cleanup paths that must remain available after a challenge is
/// disabled, while still rejecting cross-game ids.
pub(crate) async fn load_scoped_challenge(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<game_challenge::Model> {
    game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))
}

/// One-second, read-only game-row cache for hot board endpoints. Mutation paths
/// use [`load_game`]; explicit invalidation handles organizer timing edits.
static GAME_ROW_CACHE: std::sync::LazyLock<
    std::sync::RwLock<HashMap<i32, (game::Model, std::time::Instant)>>,
> = std::sync::LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

/// Coalesce concurrent reloads at the one-second TTL boundary, including
/// negative and failed lookups so an invalid id or database outage cannot
/// trigger a follower recompute herd.
static GAME_SF: std::sync::LazyLock<crate::utils::single_flight::SingleFlight<GameLoadFlight>> =
    std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

const GAME_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) async fn load_game_cached(st: &SharedState, id: i32) -> AppResult<game::Model> {
    let generation = game_row_cache_generation(id);
    if let Ok(cache) = GAME_ROW_CACHE.read() {
        if let Some((g, at)) = cache.get(&id) {
            if at.elapsed() < GAME_CACHE_TTL {
                return Ok(g.clone());
            }
        }
    }
    // Miss: single-flight the DB load. Preserve the public 404 contract while
    // broadcasting transient failures as one generic 500.
    let st2 = st.clone();
    let coalesced = GAME_SF
        .run(&format!("game:{id}:v{generation}"), move || async move {
            if let Ok(cache) = GAME_ROW_CACHE.read() {
                if let Some((g, at)) = cache.get(&id) {
                    if at.elapsed() < GAME_CACHE_TTL {
                        return GameLoadFlight::Found(g.clone());
                    }
                }
            }
            match load_game(&st2, id).await {
                Ok(g) => {
                    cache_game_row_if_current(id, g.clone(), generation);
                    GameLoadFlight::Found(g)
                }
                Err(AppError::NotFound(_)) => GameLoadFlight::NotFound,
                Err(error) => {
                    tracing::warn!(game = id, %error, "game cache fill failed");
                    GameLoadFlight::Failed
                }
            }
        })
        .await;
    match coalesced {
        GameLoadFlight::Found(g) => Ok(g),
        GameLoadFlight::NotFound => Err(AppError::not_found("Game not found")),
        GameLoadFlight::Failed => Err(AppError::internal("game cache fill failed")),
    }
}

/// Resolve a user's participation in a game via the UserParticipations link.
///
/// Reuse the accepted-only A&D participation cache for jeopardy `/details` and
/// submit; participation mutations explicitly invalidate its five-second TTL.
async fn find_participation(
    st: &SharedState,
    user_id: Uuid,
    game_id: i32,
) -> AppResult<Option<participation::Model>> {
    let key = crate::controllers::game::ad::participation_cache_key(user_id, game_id);
    if let Some(bytes) = st.cache.get(&key).await {
        if let Ok(p) = serde_json::from_slice::<participation::Model>(&bytes) {
            if p.status == ParticipationStatus::Accepted {
                return Ok(Some(p));
            }
        }
    }
    let Some(link) = user_participation::Entity::find_by_id((user_id, game_id))
        .one(&st.db)
        .await?
    else {
        return Ok(None);
    };
    let part = participation::Entity::find_by_id(link.participation_id)
        .one(&st.db)
        .await?;
    // Only cache an accepted participation (matches `resolve_participation`), so the cache
    // can never weaken a status gate — a pending/removed one is always read fresh.
    if let Some(ref p) = part {
        if p.status == ParticipationStatus::Accepted {
            if let Ok(j) = serde_json::to_vec(p) {
                st.cache
                    .set(&key, &j, Some(std::time::Duration::from_secs(5)))
                    .await;
            }
        }
    }
    Ok(part)
}

/// Per-team scoreboard token: `{teamId}:Ed25519(privateKey, "RSCTF_TEAM_{teamId}")`.
fn participation_token(g: &game::Model, team_id: i32) -> AppResult<String> {
    let signature =
        crate::utils::crypto_utils::game_sign(&g.private_key, &format!("RSCTF_TEAM_{team_id}"))?;
    Ok(format!("{team_id}:{signature}"))
}

mod cheat;
mod containers;
mod lookups;
mod membership;
mod play;
mod scoreboard;
mod scoreboard_board;
mod submit;
mod traffic;
mod writeup;

pub use cheat::*;
pub use containers::*;
use lookups::*;
pub use play::*;
pub use scoreboard::*;
pub(crate) use scoreboard_board::*;
pub use submit::*;
pub use traffic::*;
pub use writeup::*;
