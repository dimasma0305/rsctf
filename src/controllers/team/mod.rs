//! Ported from RSCTF `Controllers/TeamController.cs` (+ `Repositories/TeamRepository.cs`).
//!
//! Route prefix `/api/team`. Team membership is modelled by the `team_member`
//! join table (RSCTF `Team.Members`): one row per (team, user). The roster is
//! that table, always unioned with the team captain (`team.captain_id`) so a
//! team is never captain-less in the view even if the membership row is missing.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::{
    ad_ssh_key, ad_team_api_token, container, game_instance, participation, team, team_member,
    user, user_participation,
};
use crate::utils::codec::random_hex;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

mod avatar;
mod lifecycle;
mod models;
pub use avatar::avatar;
use lifecycle::destroy_participation_ad_services;
pub use models::*;

/// Each user may captain at most this many teams. Mirrors RSCTF `MaxTeamsAllowed`.
const MAX_TEAMS_ALLOWED: u64 = 3;
/// Defensive roster bound; per-game limits remain authoritative for participation.
const MAX_TEAM_MEMBERS: u64 = 100;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/team", get(get_teams_info).post(create_team))
        .route(
            "/api/team/{id}",
            get(get_basic_info).put(update_team).delete(delete_team),
        )
        .route(
            "/api/team/{id}/invite",
            get(invite_code).put(update_invite_token),
        )
        .route("/api/team/accept", post(accept))
        .route("/api/team/verify", post(verify_signature))
        .route("/api/team/{id}/leave", post(leave))
        .route("/api/team/{id}/kick/{userId}", post(kick_user))
        .route("/api/team/{id}/transfer", put(transfer))
        .route("/api/team/{id}/avatar", put(avatar))
}

// --- Handlers --------------------------------------------------------------

/// `GET /api/team/{id}` — basic info for one team.
pub async fn get_basic_info(
    State(st): State<SharedState>,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let team = load_team(&st, id).await?;
    let info = to_info(&st, &team, true).await?;
    Ok(RequestResponse::ok(info))
}

/// `GET /api/team` — every team the current user captains or participates in.
pub async fn get_teams_info(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<Vec<TeamInfoModel>>> {
    let teams = user_teams(&st, user.id).await?;
    let mut out = Vec::with_capacity(teams.len());
    for team in &teams {
        out.push(to_info(&st, team, true).await?);
    }
    Ok(RequestResponse::ok(out))
}

/// `POST /api/team` — create a team; creator becomes captain.
pub async fn create_team(
    State(st): State<SharedState>,
    user: CurrentUser,
    Json(model): Json<TeamUpdateModel>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let captained = team::Entity::find()
        .filter(team::Column::CaptainId.eq(user.id))
        .count(&st.db)
        .await?;
    if captained >= MAX_TEAMS_ALLOWED {
        return Err(AppError::bad_request("Exceeded team creation limit"));
    }

    let name = model.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("Team name cannot be empty"));
    }

    let am = team::ActiveModel {
        name: Set(name),
        bio: Set(model.bio),
        avatar_hash: Set(None),
        locked: Set(false),
        invite_token: Set(random_hex(16)),
        captain_id: Set(user.id),
        ..Default::default()
    };
    let team = am.insert(&st.db).await?;

    // The creator is the captain *and* the first roster member (RSCTF
    // `CreateTeam` seeds `Team.Members` with the creator).
    let member = team_member::ActiveModel {
        team_id: Set(team.id),
        user_id: Set(user.id),
        ..Default::default()
    };
    member.insert(&st.db).await?;

    // RSCTF `Team_Created` — "Create team {name}" (TeamController, Success).
    crate::services::audit::info(
        &st.db,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Create team {}", team.name),
    )
    .await;

    let info = to_info(&st, &team, true).await?;
    Ok(RequestResponse::ok(info))
}

/// `PUT /api/team/{id}` — update name/bio (captain only).
pub async fn update_team(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<TeamUpdateModel>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;

    let old_name = team.name.clone();
    let mut am: team::ActiveModel = team.into();
    if let Some(name) = model.name {
        let name = name.trim().to_string();
        if !name.is_empty() {
            // RSCTF `UpdateTeam` → `Team.UpdateInfo` sets the name unconditionally;
            // it does NOT enforce team-name uniqueness on rename (only the on-wire
            // invite code embeds the id, so duplicate names are harmless). Match that.
            am.name = Set(name);
        }
    }
    if let Some(bio) = model.bio {
        am.bio = Set(Some(bio));
    }
    let team = am.update(&st.db).await?;

    // RSCTF `FlushScoreboardCacheForTeam`: a rename must invalidate the scoreboard
    // caches for every game the team is in, otherwise the board keeps the old name
    // (the live board rides a 7-day sliding cache; A&D/KotH boards never auto-
    // regenerate once a game is paused/ended). Only flush on an actual name change,
    // matching the C# ordinal compare. Cache eviction is best-effort.
    if team.name != old_name {
        flush_scoreboard_for_team(&st, team.id).await?;
    }
    let info = to_info(&st, &team, true).await?;
    Ok(RequestResponse::ok(info))
}

/// `DELETE /api/team/{id}` — delete a team (captain only).
pub async fn delete_team(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let roster_key = format!("team-roster:{id}");
    let _roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;
    let affected_game_ids = team_game_ids(&st, team.id).await?;

    if team.locked && any_active_game(&st, team.id).await? {
        return Err(AppError::bad_request("Team is locked by an active game"));
    }

    mark_team_participations_revoked(&st, team.id).await?;
    revoke_team_shared_capabilities(&st, team.id).await?;

    // RSCTF `DeleteTeam`: reap the team's live containers BEFORE the cascade drops
    // the participation/instance rows the teardown keys off — otherwise A&D service
    // containers (and per-team KotH instances) would leak, running until game end
    // with no row left to reconcile them against. Best-effort: a container-daemon
    // hiccup must never block the delete, so failures are swallowed and we degrade
    // (mirrors RSCTF wrapping each destroy in a try/catch).
    destroy_team_containers(&st, team.id).await?;

    // Evict the scoreboard caches for every game the team was in *before* the
    // cascade drops the participation rows those game ids are read from —
    // otherwise the deleted team's row lingers on the cached board for up to
    // 7 days (RSCTF `DeleteTeam` → `FlushScoreboardsForGames`). Best-effort.
    flush_scoreboard_for_team(&st, team.id).await?;

    for part in participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team.id))
        .all(&st.db)
        .await?
    {
        st.byoc.disconnect_participation(&st.db, part.id).await?;
    }

    participation::Entity::delete_many()
        .filter(participation::Column::TeamId.eq(team.id))
        .exec(&st.db)
        .await?;
    user_participation::Entity::delete_many()
        .filter(user_participation::Column::TeamId.eq(team.id))
        .exec(&st.db)
        .await?;
    team_member::Entity::delete_many()
        .filter(team_member::Column::TeamId.eq(team.id))
        .exec(&st.db)
        .await?;

    let info = to_info(&st, &team, false).await?;
    team::Entity::delete_by_id(team.id).exec(&st.db).await?;
    flush_scoreboards_for_games(&st, &affected_game_ids).await;

    // RSCTF `Team_Deleted` — "Delete team {name}" (TeamController, Success).
    crate::services::audit::info(
        &st.db,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Delete team {}", team.name),
    )
    .await;

    distributed.release().await?;
    Ok(RequestResponse::ok(info))
}

/// `GET /api/team/{id}/invite` — current invite code (captain only).
pub async fn invite_code(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<String>> {
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;
    Ok(RequestResponse::ok(team.invite_code()))
}

/// `PUT /api/team/{id}/invite` — regenerate the invite token (captain only).
pub async fn update_invite_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<String>> {
    let roster_key = format!("team-roster:{id}");
    let _roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;

    let mut am: team::ActiveModel = team.into();
    am.invite_token = Set(random_hex(16));
    let team = am.update(&st.db).await?;
    for part in participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team.id))
        .all(&st.db)
        .await?
    {
        st.byoc.disconnect_participation(&st.db, part.id).await?;
    }
    distributed.release().await?;
    Ok(RequestResponse::ok(team.invite_code()))
}

/// `POST /api/team/accept` — join a team via its invite code (`name:id:token`).
pub async fn accept(
    State(st): State<SharedState>,
    user: CurrentUser,
    Json(code): Json<String>,
) -> AppResult<StatusCode> {
    // Invite code format: `{name}:{id}:{token}` where token is 32 lowercase hex.
    // Team names may themselves contain colons, so split on the *last* colon of
    // the prefix (matching RSCTF's `LastIndexOf(':')`).
    if code.len() < 34 || !code.is_char_boundary(code.len() - 32) {
        return Err(AppError::bad_request("Invalid invite code"));
    }
    let (pre_code, invite_token) = code.split_at(code.len() - 32);
    let pre_code = pre_code
        .strip_suffix(':')
        .ok_or_else(|| AppError::bad_request("Invalid invite code"))?;
    let token_ok = invite_token.len() == 32
        && invite_token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !token_ok {
        return Err(AppError::bad_request("Invalid invite code"));
    }
    let last_colon = pre_code
        .rfind(':')
        .ok_or_else(|| AppError::bad_request("Invalid invite code"))?;
    let team_id: i32 = pre_code[last_colon + 1..]
        .parse()
        .map_err(|_| AppError::bad_request("Invalid invite code"))?;

    let roster_key = format!("team-roster:{team_id}");
    let _roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    let team = team::Entity::find_by_id(team_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("Team not found"))?;

    if team.invite_token != invite_token {
        return Err(AppError::bad_request("Invalid invitation for this team"));
    }
    if team.locked && any_active_game(&st, team.id).await? {
        return Err(AppError::bad_request("Team is locked by an active game"));
    }
    let members = member_ids(&st, &team).await?;
    if members.contains(&user.id) {
        return Err(AppError::bad_request("Already a member of this team"));
    }
    if members.len() as u64 >= MAX_TEAM_MEMBERS {
        return Err(AppError::bad_request("Team is full"));
    }

    // Add the caller to the roster (RSCTF `team.Members.Add(user)`).
    let member = team_member::ActiveModel {
        team_id: Set(team.id),
        user_id: Set(user.id),
        ..Default::default()
    };
    member.insert(&st.db).await?;

    // RSCTF `Team_UserJoined` — "Join Team {name}" (TeamController, Success).
    crate::services::audit::info(
        &st.db,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Join Team {}", team.name),
    )
    .await;

    // RSCTF `Accept` returns a bare `Ok()` (empty 200); the client types this as
    // `void` with no JSON parse, so emit an empty 200 rather than a `{title,status}`
    // body.
    distributed.release().await?;
    Ok(StatusCode::OK)
}

/// `POST /api/team/{id}/leave` — leave a team. Any member may leave, the captain
/// included (RSCTF `Leave` has no captain guard); only an active-game lock blocks it.
pub async fn leave(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<StatusCode> {
    let roster_key = format!("team-roster:{id}");
    let _roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    let team = load_team(&st, id).await?;

    let members = member_ids(&st, &team).await?;
    if !members.contains(&user.id) {
        return Err(AppError::bad_request("You are not in this team"));
    }
    // RSCTF `Leave` has no captain guard: any member (captain included) may leave
    // whenever the team is not locked by an active game. It does not block or
    // reassign captaincy, so mirror that and simply drop the caller's membership.
    if team.locked && any_active_game(&st, team.id).await? {
        return Err(AppError::bad_request("Team is locked by an active game"));
    }

    remove_membership(&st, team.id, user.id).await?;

    // RSCTF `Team_UserLeft` — "Left the team {name}" (TeamController, Success).
    crate::services::audit::info(
        &st.db,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Left the team {}", team.name),
    )
    .await;

    // RSCTF `Leave` returns a bare `Ok()` (empty 200); the client types this as
    // `void` with no JSON parse, so emit an empty 200.
    distributed.release().await?;
    Ok(StatusCode::OK)
}

/// `POST /api/team/{id}/kick/{userId}` — remove a member (captain only).
pub async fn kick_user(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, target)): Path<(i32, Uuid)>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let roster_key = format!("team-roster:{id}");
    let _roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;

    if team.locked && any_active_game(&st, team.id).await? {
        return Err(AppError::bad_request("Team is locked by an active game"));
    }
    if target == team.captain_id {
        return Err(AppError::bad_request("Cannot kick the team captain"));
    }
    if !member_ids(&st, &team).await?.contains(&target) {
        return Err(AppError::bad_request("User is not in this team"));
    }

    remove_membership(&st, team.id, target).await?;

    // RSCTF `Team_MemberRemoved` — "Kick {kicked} from Team {name}" (TeamController,
    // Success). Resolve the kicked user's name for the message (best-effort read).
    let kicked_name = user::Entity::find_by_id(target)
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .and_then(|u| u.user_name)
        .unwrap_or_else(|| "null".to_string());
    crate::services::audit::info(
        &st.db,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Kick {} from Team {}", kicked_name, team.name),
    )
    .await;

    let info = to_info(&st, &team, true).await?;
    distributed.release().await?;
    Ok(RequestResponse::ok(info))
}

/// `PUT /api/team/{id}/transfer` — hand captaincy to another user (captain only).
pub async fn transfer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<TeamTransferModel>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let roster_key = format!("team-roster:{id}");
    let _roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;

    if team.locked && any_active_game(&st, team.id).await? {
        return Err(AppError::bad_request("Team is locked by an active game"));
    }
    if !member_ids(&st, &team)
        .await?
        .contains(&model.new_captain_id)
    {
        return Err(AppError::bad_request(
            "New captain must already be a team member",
        ));
    }

    let new_captain = user::Entity::find_by_id(model.new_captain_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("New captain not found"))?;

    // Keep the per-user captaincy cap after the roster eligibility check.
    let captained = team::Entity::find()
        .filter(team::Column::CaptainId.eq(new_captain.id))
        .count(&st.db)
        .await?;
    if captained >= MAX_TEAMS_ALLOWED {
        return Err(AppError::bad_request(
            "New captain already captains too many teams",
        ));
    }

    let mut am: team::ActiveModel = team.into();
    am.captain_id = Set(new_captain.id);
    let team = am.update(&st.db).await?;

    let info = to_info(&st, &team, true).await?;
    distributed.release().await?;
    Ok(RequestResponse::ok(info))
}

/// `POST /api/team/verify` — verify a team signature. Mirrors RSCTF
/// `TeamController.VerifySignature` / `CryptoUtils.VerifySignature`:
///
/// * `publicKey` is the game's Ed25519 public key, standard-Base64 encoded
///   (must decode to exactly 32 bytes).
/// * `teamToken` is `<id>:<signature>` where `<signature>` is the standard-Base64
///   Ed25519 signature over the UTF-8 bytes of `RSCTF_TEAM_{id}`.
///
/// Returns void 200 when the signature is valid, 400 on malformed input, and 401
/// when the signature does not verify.
pub async fn verify_signature(
    State(_st): State<SharedState>,
    Json(model): Json<SignatureVerifyModel>,
) -> AppResult<StatusCode> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    // Public key: Base64 → 32 raw bytes.
    let pk_bytes = crate::utils::codec::base64_decode(&model.public_key)
        .ok_or_else(|| AppError::bad_request("Invalid signature"))?;
    let pk_arr: [u8; 32] = pk_bytes
        .as_slice()
        .try_into()
        .map_err(|_| AppError::bad_request("Invalid signature"))?;

    // Team token: `<id>:<signature>` (split on the first colon).
    let pos = model
        .team_token
        .find(':')
        .ok_or_else(|| AppError::bad_request("Invalid signature"))?;
    let (id_str, rest) = model.team_token.split_at(pos);
    let sign = &rest[1..];
    let team_id: i32 = id_str
        .parse()
        .map_err(|_| AppError::bad_request("Invalid signature"))?;
    if sign.is_empty() {
        return Err(AppError::bad_request("Invalid signature"));
    }

    // Data that was signed: `RSCTF_TEAM_{id}`.
    let data = format!("RSCTF_TEAM_{team_id}");

    // Beyond this point a malformed key/signature is treated as a failed
    // verification (401), never a 500 — RSCTF surfaces the same Unauthorized.
    let verified = (|| {
        let verifying_key = VerifyingKey::from_bytes(&pk_arr).ok()?;
        let sign_bytes = crate::utils::codec::base64_decode(sign)?;
        let sign_arr: [u8; 64] = sign_bytes.as_slice().try_into().ok()?;
        let signature = Signature::from_bytes(&sign_arr);
        Some(verifying_key.verify(data.as_bytes(), &signature).is_ok())
    })()
    .unwrap_or(false);

    if verified {
        // RSCTF `VerifySignature` returns a bare `Ok()` (empty 200); the client
        // types the success case as `void`, so emit an empty 200.
        Ok(StatusCode::OK)
    } else {
        Err(AppError::Unauthorized)
    }
}

// --- Helpers ---------------------------------------------------------------

async fn load_team(st: &SharedState, id: i32) -> AppResult<team::Model> {
    team::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Team not found"))
}

fn require_captain(team: &team::Model, user: &CurrentUser) -> AppResult<()> {
    if team.captain_id != user.id {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// Distinct member ids for a team: captain plus everyone on the roster join table.
async fn member_ids(st: &SharedState, team: &team::Model) -> AppResult<BTreeSet<Uuid>> {
    let rows = team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team.id))
        .all(&st.db)
        .await?;
    let mut ids: BTreeSet<Uuid> = rows.into_iter().map(|r| r.user_id).collect();
    ids.insert(team.captain_id);
    Ok(ids)
}

/// Build the roster view for a team.
async fn roster(st: &SharedState, team: &team::Model) -> AppResult<Vec<TeamUserInfoModel>> {
    let ids: Vec<Uuid> = member_ids(st, team).await?.into_iter().collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let users = user::Entity::find()
        .filter(user::Column::Id.is_in(ids))
        .all(&st.db)
        .await?;
    Ok(users
        .into_iter()
        .map(|m| TeamUserInfoModel {
            captain: m.id == team.captain_id,
            avatar: m.avatar_url(),
            id: m.id,
            user_name: m.user_name,
            bio: Some(m.bio),
            real_name: m.real_name,
            student_number: m.std_number,
        })
        .collect())
}

async fn to_info(
    st: &SharedState,
    team: &team::Model,
    include_members: bool,
) -> AppResult<TeamInfoModel> {
    let members = if include_members {
        Some(roster(st, team).await?)
    } else {
        None
    };
    Ok(TeamInfoModel {
        id: team.id,
        name: team.name.clone(),
        bio: team.bio.clone(),
        avatar: team.avatar_url(),
        locked: team.locked,
        members,
    })
}

/// Every team the user captains or is a roster member of, ordered by id.
async fn user_teams(st: &SharedState, user_id: Uuid) -> AppResult<Vec<team::Model>> {
    let member_team_ids: Vec<i32> = team_member::Entity::find()
        .filter(team_member::Column::UserId.eq(user_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|r| r.team_id)
        .collect();

    let mut cond = team::Column::CaptainId.eq(user_id);
    if !member_team_ids.is_empty() {
        cond = cond.or(team::Column::Id.is_in(member_team_ids));
    }
    let teams = team::Entity::find()
        .filter(cond)
        .order_by_asc(team::Column::Id)
        .all(&st.db)
        .await?;
    Ok(teams)
}

/// Revoke every participation-shared A&D capability for one participation.
pub(crate) async fn revoke_participation_capabilities(
    st: &SharedState,
    participation_id: i32,
) -> AppResult<()> {
    // BYOC tokens are derived from the team invite secret. Rotate it so a bundle
    // rejected once cannot silently become valid again if the participation is
    // later re-accepted. This intentionally invalidates BYOC bundles for every
    // participation of the team until the remaining players download fresh ones.
    let team_id = participation::Entity::find_by_id(participation_id)
        .one(&st.db)
        .await?
        .map(|part| part.team_id);
    let team_parts = if let Some(team_id) = team_id {
        participation::Entity::find()
            .filter(participation::Column::TeamId.eq(team_id))
            .all(&st.db)
            .await?
    } else {
        Vec::new()
    };
    let mut errors = Vec::new();
    if let Some(team_id) = team_id {
        match team::Entity::find_by_id(team_id).one(&st.db).await {
            Ok(Some(team)) => {
                let mut am: team::ActiveModel = team.into();
                am.invite_token = Set(random_hex(16));
                if let Err(error) = am.update(&st.db).await {
                    errors.push(format!("rotate team secret: {error}"));
                }
            }
            Ok(None) => {}
            Err(error) => errors.push(format!("load team secret: {error}")),
        }
    }
    if let Err(error) = ad_team_api_token::Entity::delete_many()
        .filter(ad_team_api_token::Column::ParticipationId.eq(participation_id))
        .exec(&st.db)
        .await
    {
        errors.push(format!("revoke API token: {error}"));
    }
    if let Err(error) = ad_ssh_key::Entity::delete_many()
        .filter(ad_ssh_key::Column::ParticipationId.eq(participation_id))
        .exec(&st.db)
        .await
    {
        errors.push(format!("revoke SSH key: {error}"));
    }
    if let Err(error) =
        crate::services::ad_vpn::revoke_peers_for_participations(&st.db, &[participation_id]).await
    {
        errors.push(format!("revoke VPN peer: {error}"));
    }
    if let Err(error) = destroy_participation_ad_services(st, participation_id).await {
        errors.push(format!("destroy A&D service: {error}"));
    }
    if team_parts.is_empty() {
        if let Err(error) = st
            .byoc
            .disconnect_participation(&st.db, participation_id)
            .await
        {
            errors.push(format!("revoke BYOC tunnel: {error}"));
        }
    } else {
        for part in team_parts {
            if let Err(error) = st.byoc.disconnect_participation(&st.db, part.id).await {
                errors.push(format!("revoke BYOC tunnel: {error}"));
            }
        }
    }
    if let Err(error) = crate::services::ad_engine::revoke_koth_capabilities(
        &st.db,
        st.cache.as_ref(),
        &[participation_id],
    )
    .await
    {
        errors.push(format!("revoke KotH capability: {error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::internal(errors.join("; ")))
    }
}

/// Revoke credentials copied by any member of a team. These credentials are
/// participation-shared in this schema, so member removal/ban must revoke them
/// for the whole team and require the remaining roster to mint fresh material.
pub(crate) async fn revoke_team_shared_capabilities(
    st: &SharedState,
    team_id: i32,
) -> AppResult<Vec<participation::Model>> {
    let parts = participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team_id))
        .all(&st.db)
        .await?;
    let part_ids: Vec<i32> = parts.iter().map(|part| part.id).collect();
    let mut errors = Vec::new();
    match team::Entity::find_by_id(team_id).one(&st.db).await {
        Ok(Some(team)) => {
            let mut am: team::ActiveModel = team.into();
            am.invite_token = Set(random_hex(16));
            if let Err(error) = am.update(&st.db).await {
                errors.push(format!("rotate team secret: {error}"));
            }
        }
        Ok(None) => {}
        Err(error) => errors.push(format!("load team secret: {error}")),
    }
    if !part_ids.is_empty() {
        if let Err(error) = ad_team_api_token::Entity::delete_many()
            .filter(ad_team_api_token::Column::ParticipationId.is_in(part_ids.clone()))
            .exec(&st.db)
            .await
        {
            errors.push(format!("revoke API tokens: {error}"));
        }
        if let Err(error) = ad_ssh_key::Entity::delete_many()
            .filter(ad_ssh_key::Column::ParticipationId.is_in(part_ids.clone()))
            .exec(&st.db)
            .await
        {
            errors.push(format!("revoke SSH keys: {error}"));
        }
        if let Err(error) =
            crate::services::ad_vpn::revoke_peers_for_participations(&st.db, &part_ids).await
        {
            errors.push(format!("revoke VPN peers: {error}"));
        }
    }
    for part in &parts {
        if let Err(error) = st.byoc.disconnect_participation(&st.db, part.id).await {
            errors.push(format!("revoke BYOC tunnel: {error}"));
        }
    }
    if !part_ids.is_empty() {
        if let Err(error) = crate::services::ad_engine::revoke_koth_capabilities(
            &st.db,
            st.cache.as_ref(),
            &part_ids,
        )
        .await
        {
            errors.push(format!("revoke KotH capabilities: {error}"));
        }
    }
    if !errors.is_empty() {
        return Err(AppError::internal(errors.join("; ")));
    }
    Ok(parts)
}

/// Establish a durable fail-closed gate before team deletion starts teardown.
pub(crate) async fn mark_team_participations_revoked(
    st: &SharedState,
    team_id: i32,
) -> AppResult<()> {
    let game_ids: std::collections::BTreeSet<i32> = participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|participation| participation.game_id)
        .collect();
    let mut scoring_controls = Vec::new();
    for game_id in game_ids {
        let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
        if crate::controllers::edit::ad_epoch_scoring_started_locked(
            &mut **control.transaction_mut(),
            game_id,
        )
        .await?
        {
            return Err(AppError::bad_request(
                "A team cannot be deleted after A&D epoch scoring has started.",
            ));
        }
        scoring_controls.push(control);
    }

    sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE team_id = $2"#)
        .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
        .bind(team_id)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    for control in scoring_controls.into_iter().rev() {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

/// Drop a user from a team's roster: delete the `team_member` row and, mirroring
/// RSCTF `RemoveUserParticipations`, any per-game participation rows they hold
/// for this team.
async fn remove_membership(st: &SharedState, team_id: i32, user_id: Uuid) -> AppResult<()> {
    let parts = revoke_team_shared_capabilities(st, team_id).await?;

    team_member::Entity::delete_many()
        .filter(team_member::Column::TeamId.eq(team_id))
        .filter(team_member::Column::UserId.eq(user_id))
        .exec(&st.db)
        .await?;
    user_participation::Entity::delete_many()
        .filter(user_participation::Column::TeamId.eq(team_id))
        .filter(user_participation::Column::UserId.eq(user_id))
        .exec(&st.db)
        .await?;

    for part in parts {
        st.cache
            .remove(&crate::controllers::game::ad::participation_cache_key(
                user_id,
                part.game_id,
            ))
            .await;
        st.byoc.disconnect_participation(&st.db, part.id).await?;
    }
    Ok(())
}

/// Distinct ids of the games the team has (or had) a participation in.
pub(crate) async fn team_game_ids(st: &SharedState, team_id: i32) -> AppResult<Vec<i32>> {
    let mut ids: Vec<i32> = participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|p| p.game_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Evict the live scoreboard renderings for every game the team is in — RSCTF
/// `FlushScoreboardsForGames`. We drop the full key family (standard + frozen,
/// A&D + KotH) per game unconditionally: removing an absent key is a no-op, and
/// this keeps us in step with `edit::flush_scoreboard`. Best-effort by design —
/// the cache is a soft dependency, so a miss never fails the request.
pub(crate) async fn flush_scoreboard_for_team(st: &SharedState, team_id: i32) -> AppResult<()> {
    let game_ids = team_game_ids(st, team_id).await?;
    flush_scoreboards_for_games(st, &game_ids).await;
    Ok(())
}

pub(crate) async fn flush_scoreboards_for_games(st: &SharedState, game_ids: &[i32]) {
    for &game_id in game_ids {
        for key in [
            format!("_ScoreBoard_{game_id}"),
            format!("_ScoreBoardFrozen_{game_id}"),
            format!("_KothScoreBoard_{game_id}"),
            format!("_KothScoreBoardFrozen_{game_id}"),
            format!("_KothTimeline_{game_id}"),
            format!("_KothTimelineFrozen_{game_id}"),
        ] {
            st.cache.remove(&key).await;
        }
        crate::controllers::game::ad::hard_invalidate_ad_scoreboard(st, game_id).await;
    }
}

/// Best-effort teardown of every live container the team owns. Walks the team's
/// participations → game instances → container rows, destroys the backing
/// container via `st.containers` (a no-op when Docker is absent) and drops the
/// bookkeeping row. Failures at any step are swallowed so a container-daemon
/// hiccup can never wedge a team delete (RSCTF wraps each destroy in try/catch).
pub(crate) async fn destroy_team_containers(st: &SharedState, team_id: i32) -> AppResult<()> {
    let part_ids: Vec<i32> = participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|part| part.id)
        .collect();
    if part_ids.is_empty() {
        return Ok(());
    }

    let ad_backend_ids =
        crate::services::ad_vpn::deactivate_participation_services(&st.db, &part_ids).await?;
    for backend_id in ad_backend_ids {
        crate::services::traffic::stop_container_capture(&st, &backend_id).await?;
        let _ = st.containers.destroy(&backend_id).await;
    }

    let instances = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.is_in(part_ids))
        .all(&st.db)
        .await?;

    for inst in instances {
        let Some(cuuid) = inst.container_id else {
            continue;
        };
        if let Ok(Some(c)) = container::Entity::find_by_id(cuuid).one(&st.db).await {
            // Destroy the backing container first, then remove the row. Both are
            // best-effort; the surrounding delete continues regardless.
            let _ = st.containers.destroy(&c.container_id).await;
            let _ = container::Entity::delete_by_id(cuuid).exec(&st.db).await;
        }
    }
    Ok(())
}

/// Whether the team is currently registered for a game that has not yet ended.
async fn any_active_game(st: &SharedState, team_id: i32) -> AppResult<bool> {
    let game_ids: Vec<i32> = participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|p| p.game_id)
        .collect();
    if game_ids.is_empty() {
        return Ok(false);
    }
    let now = Utc::now();
    let active = crate::models::data::game::Entity::find()
        .filter(crate::models::data::game::Column::Id.is_in(game_ids))
        .filter(crate::models::data::game::Column::EndTimeUtc.gt(now))
        .count(&st.db)
        .await?;
    Ok(active > 0)
}
