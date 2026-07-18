//! Ported from RSCTF `Controllers/AdAdminController.cs` — the organizer / game-admin
//! Attack & Defense operator surface.
//!
//! # Attack & Defense — operator/admin surface
//!
//! Admin-only counterpart to [`crate::controllers::ad_game`]. Where the player
//! controller *reads* A&D state and submits captured flags, this controller lets
//! a game organizer inspect and operate the engine:
//!   * **Service inventory** (`GET .../Services`) — enumerate every (team,
//!     challenge) A&D service instance registered for the game, joined to the
//!     team and challenge names, with its last checker/SLA verdict and address.
//!   * **Service registration** (`POST .../Services`) — register / upsert a
//!     team's service host:port so the checker knows where to probe.
//!   * **Round inspection** (`GET .../Rounds`) — the full round/tick timeline.
//!   * **Round advance** (`POST .../Round/Advance`) — retained as a typed API
//!     endpoint but rejects manual writes; only the automatic checker pipeline
//!     may create official scored rounds.
//!
//! Round / flag / attack / check state is persisted in the AD entity tables
//! (`ad_round`, `ad_team_service`, `ad_flag`, `ad_check_result`). Official score
//! aggregation lives in [`crate::services::ad::scoring`], and the automatic
//! pipeline executes each configured checker before publishing its evidence.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Router;
use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::models::data::{ad_round, ad_team_service, game, game_challenge, participation, team};
use crate::services::ad_engine::AdCheckStatus;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

// ---------------------------------------------------------------------------
// DTOs (inline; camelCase on the wire to match RSCTF's JSON contract).
// ---------------------------------------------------------------------------

/// One (team, challenge) service instance in the admin service inventory.
/// Port of the per-cell slice of RSCTF `AdTeamCellModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdServiceModel {
    pub ad_team_service_id: i32,
    pub participation_id: i32,
    pub team_name: String,
    pub challenge_id: i32,
    pub challenge_title: String,
    pub host: String,
    pub port: i32,
    /// Latest checker/SLA verdict for this service: "Ok" / "Mumble" / "Offline"
    /// / "InternalError" (the [`AdCheckStatus`] label of the stored numeric).
    pub check_status: String,
}

/// One round (tick) in the game's A&D timeline. Port of RSCTF `AdRound`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdRoundModel {
    pub id: i32,
    pub number: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub start_time_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end_time_utc: DateTime<Utc>,
    pub finalized: bool,
}

/// Legacy response shape retained for the disabled manual-advance endpoint.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdRoundAdvanceResult {
    pub round: i32,
    pub flags_planted: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub ends_at: DateTime<Utc>,
    /// `game.ad_flag_lifetime_ticks` — a flag stays live for this many ticks, so
    /// a capture scores only while the flag's round is within `lifetime` of the
    /// live round. The submit path rejects an expired capture.
    pub flag_lifetime_ticks: i32,
}

/// Body of `POST .../Services` — register / upsert a team's service address.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdServiceRegisterModel {
    pub participation_id: i32,
    pub challenge_id: i32,
    pub host: String,
    pub port: i32,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router() -> Router<SharedState> {
    Router::new()
        // Full per-(team,challenge) service inventory + register/upsert an address.
        .route(
            "/api/ad/admin/{game_id}/Services",
            get(services).post(register_service),
        )
        // The round/tick timeline.
        .route("/api/ad/admin/{game_id}/Rounds", get(rounds))
        // Retained for client compatibility; the handler rejects manual writes.
        .route("/api/ad/admin/{game_id}/Round/Advance", post(advance_round))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/ad/admin/{game_id}/Services` — the operator's A&D service inventory.
///
/// Ports the row/cell assembly of `AdAdminController.State`: list every
/// `AdTeamService` for the game, resolve each one's team name (via its
/// participation → team FK) and challenge title, and surface its stored checker
/// status.
pub async fn services(
    State(st): State<SharedState>,
    AdminUser(_user): AdminUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<Vec<AdServiceModel>>> {
    let _game = load_game(&st, game_id).await?;

    let rows = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::GameId.eq(game_id))
        .order_by_asc(ad_team_service::Column::ParticipationId)
        .order_by_asc(ad_team_service::Column::ChallengeId)
        .all(&st.db)
        .await?;

    let team_names =
        team_name_by_participation(&st, rows.iter().map(|r| r.participation_id)).await?;
    let challenge_titles = challenge_title_map(&st, rows.iter().map(|r| r.challenge_id)).await?;

    let data = rows
        .into_iter()
        .map(|r| AdServiceModel {
            team_name: team_names
                .get(&r.participation_id)
                .cloned()
                .unwrap_or_default(),
            challenge_title: challenge_titles
                .get(&r.challenge_id)
                .cloned()
                .unwrap_or_default(),
            check_status: check_status_label(r.status).to_string(),
            ad_team_service_id: r.id,
            participation_id: r.participation_id,
            challenge_id: r.challenge_id,
            host: r.host,
            port: r.port,
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

/// `POST /api/ad/admin/{game_id}/Services` — register or upsert a team's service
/// host:port so the checker knows where to probe. Keyed by (game, participation,
/// challenge); a repeat call updates the address in place.
pub async fn register_service(
    State(st): State<SharedState>,
    AdminUser(_user): AdminUser,
    Path(game_id): Path<i32>,
    axum::Json(model): axum::Json<AdServiceRegisterModel>,
) -> AppResult<RequestResponse<AdServiceModel>> {
    let _game = load_game(&st, game_id).await?;

    if model.host.trim().is_empty() {
        return Err(AppError::bad_request("A service host is required"));
    }
    if model.port <= 0 || model.port > 65535 {
        return Err(AppError::bad_request("A valid service port is required"));
    }

    let lock_key = format!(
        "ad-service:{}:{}",
        model.participation_id, model.challenge_id
    );
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;

    // Revalidate the full live pair after taking the same lock as provisioning
    // and teardown; an admin request must not republish a revoked endpoint.
    let part = participation::Entity::find()
        .filter(participation::Column::Id.eq(model.participation_id))
        .filter(participation::Column::GameId.eq(game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("Unknown accepted participation for this game"))?;
    let challenge = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(model.challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("Unknown active A&D challenge for this game"))?;

    // Upsert by (game, participation, challenge).
    let existing = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::GameId.eq(game_id))
        .filter(ad_team_service::Column::ParticipationId.eq(model.participation_id))
        .filter(ad_team_service::Column::ChallengeId.eq(model.challenge_id))
        .one(&st.db)
        .await?;

    let saved = match existing {
        Some(row) => {
            let mut am: ad_team_service::ActiveModel = row.into();
            am.host = Set(model.host.clone());
            am.port = Set(model.port);
            am.update(&st.db).await?
        }
        None => {
            ad_team_service::ActiveModel {
                game_id: Set(game_id),
                participation_id: Set(model.participation_id),
                challenge_id: Set(model.challenge_id),
                host: Set(model.host.clone()),
                port: Set(model.port),
                // Not yet probed — the checker executor sets this on the first tick.
                status: Set(AdCheckStatus::InternalError as i16),
                ..Default::default()
            }
            .insert(&st.db)
            .await?
        }
    };
    crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
    distributed.release().await?;

    let team_name = team::Entity::find_by_id(part.team_id)
        .one(&st.db)
        .await?
        .map(|t| t.name)
        .unwrap_or_default();

    Ok(RequestResponse::ok(AdServiceModel {
        ad_team_service_id: saved.id,
        participation_id: saved.participation_id,
        team_name,
        challenge_id: saved.challenge_id,
        challenge_title: challenge.title,
        host: saved.host,
        port: saved.port,
        check_status: check_status_label(saved.status).to_string(),
    }))
}

/// `GET /api/ad/admin/{game_id}/Rounds` — the round/tick timeline (newest first).
pub async fn rounds(
    State(st): State<SharedState>,
    AdminUser(_user): AdminUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<Vec<AdRoundModel>>> {
    let _game = load_game(&st, game_id).await?;

    let data = ad_round::Entity::find()
        .filter(ad_round::Column::GameId.eq(game_id))
        .order_by_desc(ad_round::Column::Number)
        .all(&st.db)
        .await?
        .into_iter()
        .map(|r| AdRoundModel {
            id: r.id,
            number: r.number,
            start_time_utc: r.start_time_utc,
            end_time_utc: r.end_time_utc,
            finalized: r.finalized,
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

/// `POST /api/ad/admin/{game_id}/Round/Advance` — intentionally disabled.
/// Official rounds must pass through the automatic flag-delivery and checker
/// pipeline so no scored round can exist without its complete evidence path.
pub async fn advance_round(
    State(_st): State<SharedState>,
    AdminUser(_user): AdminUser,
    Path(_game_id): Path<i32>,
) -> AppResult<RequestResponse<AdRoundAdvanceResult>> {
    Err(AppError::bad_request(
        "Manual round advance is disabled; official rounds are created only by the automatic flag-delivery and checker pipeline.",
    ))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn load_game(st: &SharedState, id: i32) -> AppResult<game::Model> {
    game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))
}

/// Human label for a stored [`AdCheckStatus`] numeric.
fn check_status_label(status: i16) -> &'static str {
    match status {
        s if s == AdCheckStatus::Ok as i16 => "Ok",
        s if s == AdCheckStatus::Mumble as i16 => "Mumble",
        s if s == AdCheckStatus::Offline as i16 => "Offline",
        _ => "InternalError",
    }
}

/// Map each participation id to its team's display name, via the
/// participation → team FK (two batched lookups, mirroring game.rs helpers).
async fn team_name_by_participation(
    st: &SharedState,
    participation_ids: impl Iterator<Item = i32>,
) -> AppResult<HashMap<i32, String>> {
    let ids: Vec<i32> = dedup(participation_ids);
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let parts = participation::Entity::find()
        .filter(participation::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;

    let team_ids: Vec<i32> = dedup(parts.iter().map(|p| p.team_id));
    let team_names: HashMap<i32, String> = if team_ids.is_empty() {
        HashMap::new()
    } else {
        team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| (t.id, t.name))
            .collect()
    };

    Ok(parts
        .into_iter()
        .map(|p| {
            let name = team_names.get(&p.team_id).cloned().unwrap_or_default();
            (p.id, name)
        })
        .collect())
}

/// Map each challenge id to its title.
async fn challenge_title_map(
    st: &SharedState,
    challenge_ids: impl Iterator<Item = i32>,
) -> AppResult<HashMap<i32, String>> {
    let ids: Vec<i32> = dedup(challenge_ids);
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.is_in(ids))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|c| (c.id, c.title))
        .collect())
}

fn dedup(ids: impl Iterator<Item = i32>) -> Vec<i32> {
    let mut seen = std::collections::HashSet::new();
    ids.filter(|id| seen.insert(*id)).collect()
}
