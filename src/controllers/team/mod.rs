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
use crate::models::data::{container, game_instance, participation, team, team_member, user};
use crate::utils::codec::random_hex;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

mod avatar;
mod lifecycle;
mod models;
mod revocation;
mod roster_policy;
pub use avatar::avatar;
pub use models::*;
pub(crate) use revocation::{
    acquire_roster_mutation, invalidate_removed_membership_cache, mark_team_participations_revoked,
    require_team_mutable, revoke_participation_capabilities, revoke_team_shared_capabilities,
    TeamDeletionLease,
};
use revocation::{remove_membership, revoke_team_shared_capabilities_locked};
pub(crate) use roster_policy::ensure_roster_change_allowed;

/// Each user may captain at most this many teams. Mirrors RSCTF `MaxTeamsAllowed`.
pub(crate) const MAX_TEAMS_ALLOWED: u64 = 3;
/// Defensive roster bound; per-game limits remain authoritative for participation.
pub(crate) const MAX_TEAM_MEMBERS: u64 = 100;

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
    let name = model.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("Team name cannot be empty"));
    }
    let team_id = create_team_rows(st.pg(), user.id, &name, model.bio.as_deref()).await?;
    let team = load_team(&st, team_id).await?;

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

/// Atomically enforce account liveness + the captain limit, then create both
/// ownership rows. The account lock is the hand-off with admin deletion: one
/// side commits first and the other must observe either the new captaincy or
/// the durable Banned role.
pub(crate) async fn create_team_rows(
    pool: &sqlx::PgPool,
    creator_id: Uuid,
    name: &str,
    bio: Option<&str>,
) -> AppResult<i32> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let role: Option<i16> =
        sqlx::query_scalar(r#"SELECT role FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#)
            .bind(creator_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if role.is_none_or(|role| role == crate::utils::enums::Role::Banned as i16) {
        return Err(AppError::Forbidden);
    }
    let captained: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "Teams" WHERE captain_id = $1"#)
            .bind(creator_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if captained >= MAX_TEAMS_ALLOWED as i64 {
        return Err(AppError::bad_request("Exceeded team creation limit"));
    }

    let team_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams"
             (name, bio, avatar_hash, locked, invite_token, captain_id)
           VALUES ($1, $2, NULL, FALSE, $3, $4)
        RETURNING id"#,
    )
    .bind(name)
    .bind(bio)
    .bind(random_hex(16))
    .bind(creator_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(creator_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(team_id)
}

/// `PUT /api/team/{id}` — update name/bio (captain only).
pub async fn update_team(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<TeamUpdateModel>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let mut roster = acquire_roster_mutation(st.pg(), id).await?;
    require_team_mutable(roster.transaction_mut(), id).await?;
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
    roster.release().await?;

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
    let mut initial = acquire_roster_mutation(st.pg(), id).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;
    let affected_game_ids = team_game_ids(&st, team.id).await?;
    let info = to_info(&st, &team, false).await?;

    if !team.deletion_pending && team.locked && any_active_game(&st, team.id).await? {
        return Err(AppError::bad_request("Team is locked by an active game"));
    }

    mark_team_participations_revoked(initial.advisory_mut(), team.id).await?;
    // Commit the fail-closed suspension and release the per-team transaction
    // before capability teardown acquires its own game/VPN locks.
    let _roster_guard = initial.release_for_external().await?;
    let Some(deletion_lease) = TeamDeletionLease::acquire(st.pg(), &roster_key, team.id).await?
    else {
        // A cross-replica duplicate completed while this request waited for the
        // external lease. Its teardown and cascade are already authoritative.
        return Ok(RequestResponse::ok(info));
    };
    // Drop accepted-participation cache entries as soon as the suspension is
    // durable, rather than waiting for the slower container/network teardown.
    crate::controllers::game::ad::flush_team_participation_cache(&st, team.id).await;
    revoke_team_shared_capabilities(&st, team.id).await?;

    // Reap the team's live containers before the cascade drops their retry
    // identities. A failed capture fence/backend destroy aborts finalization;
    // deletion remains durably suspended and exactly retryable.
    destroy_team_containers(&st, team.id).await?;

    // Evict the scoreboard caches for every game the team was in *before* the
    // cascade drops the participation rows those game ids are read from —
    // otherwise the deleted team's row lingers on the cached board for up to
    // 7 days (RSCTF `DeleteTeam` → `FlushScoreboardsForGames`). Best-effort.
    flush_scoreboard_for_team(&st, team.id).await?;

    deletion_lease.finalize(team.id).await?;
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
    let mut roster = acquire_roster_mutation(st.pg(), id).await?;
    require_team_mutable(roster.transaction_mut(), id).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;

    let mut am: team::ActiveModel = team.into();
    am.invite_token = Set(random_hex(16));
    let team = am.update(&st.db).await?;
    roster.release().await?;
    for part in participation::Entity::find()
        .filter(participation::Column::TeamId.eq(team.id))
        .all(&st.db)
        .await?
    {
        st.byoc.disconnect_participation(&st.db, part.id).await?;
    }
    Ok(RequestResponse::ok(team.invite_code()))
}

/// `POST /api/team/accept` — join a team via its invite code (`name:id:token`).
async fn lock_live_roster_account(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> AppResult<()> {
    let role: Option<i16> =
        sqlx::query_scalar(r#"SELECT role FROM "AspNetUsers" WHERE id = $1 FOR SHARE"#)
            .bind(user_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if role.is_none_or(|role| role == crate::utils::enums::Role::Banned as i16) {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

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
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    // Retain a share lock until the membership insert commits. Account deletion
    // first takes the conflicting row lock and sets Role::Banned, so it either
    // waits for this membership (and snapshots it) or this request observes the
    // fence and cannot create a late roster entry.
    lock_live_roster_account(distributed.transaction_mut(), user.id).await?;
    let team: Option<(String, String, bool, Uuid)> = sqlx::query_as(
        r#"SELECT name, invite_token, deletion_pending, captain_id
              FROM "Teams" WHERE id = $1"#,
    )
    .bind(team_id)
    .fetch_optional(&mut **distributed.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((team_name, current_invite, deletion_pending, captain_id)) = team else {
        return Err(AppError::bad_request("Team not found"));
    };
    if deletion_pending {
        return Err(AppError::conflict("Team is being deleted"));
    }

    if current_invite != invite_token {
        return Err(AppError::bad_request("Invalid invitation for this team"));
    }
    let already_member: bool = captain_id == user.id
        || sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "TeamMembers"
                    WHERE team_id = $1 AND user_id = $2
               )"#,
        )
        .bind(team_id)
        .bind(user.id)
        .fetch_one(&mut **distributed.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if already_member {
        return Err(AppError::bad_request("Already a member of this team"));
    }
    ensure_roster_change_allowed(distributed.transaction_mut(), team_id).await?;
    let member_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint
              FROM (
                    SELECT captain_id AS user_id FROM "Teams" WHERE id = $1
                    UNION
                    SELECT user_id FROM "TeamMembers" WHERE team_id = $1
              ) roster"#,
    )
    .bind(team_id)
    .fetch_one(&mut **distributed.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if member_count >= MAX_TEAM_MEMBERS as i64 {
        return Err(AppError::bad_request("Team is full"));
    }

    // Add the caller to the roster (RSCTF `team.Members.Add(user)`).
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team_id)
        .bind(user.id)
        .execute(&mut **distributed.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    distributed.release().await?;

    // RSCTF `Team_UserJoined` — "Join Team {name}" (TeamController, Success).
    crate::services::audit::info(
        &st.db,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Join Team {team_name}"),
    )
    .await;

    // RSCTF `Accept` returns a bare `Ok()` (empty 200); the client types this as
    // `void` with no JSON parse, so emit an empty 200 rather than a `{title,status}`
    // body.
    Ok(StatusCode::OK)
}

/// `POST /api/team/{id}/leave` — leave a team. A captain must atomically transfer
/// captaincy first; other members may leave until the shared roster policy
/// freezes membership.
pub async fn leave(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<StatusCode> {
    let mut roster = acquire_roster_mutation(st.pg(), id).await?;
    require_team_mutable(roster.transaction_mut(), id).await?;
    let team = load_team(&st, id).await?;

    if team.captain_id == user.id {
        return Err(AppError::bad_request(
            "Team captain must transfer captaincy before leaving",
        ));
    }
    let members = member_ids(&st, &team).await?;
    if !members.contains(&user.id) {
        return Err(AppError::bad_request("You are not in this team"));
    }
    // Captaincy is stable now; the shared policy fences the remaining mutable
    // roster state before credential revocation and membership deletion.
    ensure_roster_change_allowed(roster.transaction_mut(), team.id).await?;

    // Keep the roster lock until every copied team credential is invalidated.
    // If external cleanup fails, membership remains intact and the same leave
    // request can be retried without creating an unauthorized credential gap.
    let (parts, koth_cache_invalidation) =
        revoke_team_shared_capabilities_locked(&st, roster.transaction_mut(), team.id).await?;
    remove_membership(roster.transaction_mut(), team.id, user.id).await?;
    roster.release().await?;
    koth_cache_invalidation.apply(st.cache.as_ref()).await;
    invalidate_removed_membership_cache(&st, user.id, &parts).await?;

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
    Ok(StatusCode::OK)
}

/// `POST /api/team/{id}/kick/{userId}` — remove a member (captain only).
pub async fn kick_user(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, target)): Path<(i32, Uuid)>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let mut roster = acquire_roster_mutation(st.pg(), id).await?;
    require_team_mutable(roster.transaction_mut(), id).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;

    ensure_roster_change_allowed(roster.transaction_mut(), team.id).await?;
    if target == team.captain_id {
        return Err(AppError::bad_request("Cannot kick the team captain"));
    }
    if !member_ids(&st, &team).await?.contains(&target) {
        return Err(AppError::bad_request("User is not in this team"));
    }
    let (parts, koth_cache_invalidation) =
        revoke_team_shared_capabilities_locked(&st, roster.transaction_mut(), team.id).await?;
    remove_membership(roster.transaction_mut(), team.id, target).await?;
    roster.release().await?;
    koth_cache_invalidation.apply(st.cache.as_ref()).await;
    invalidate_removed_membership_cache(&st, target, &parts).await?;

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
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
    transfer_captain_locked(
        distributed.transaction_mut(),
        id,
        user.id,
        model.new_captain_id,
    )
    .await?;
    distributed.release().await?;
    let team = load_team(&st, id).await?;
    let info = to_info(&st, &team, true).await?;
    Ok(RequestResponse::ok(info))
}

async fn transfer_captain_locked(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
    current_captain_id: Uuid,
    new_captain_id: Uuid,
) -> AppResult<()> {
    let team: Option<(Uuid, bool, bool)> = sqlx::query_as(
        r#"SELECT captain_id, locked, deletion_pending
              FROM "Teams" WHERE id = $1"#,
    )
    .bind(team_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((captain_id, locked, deletion_pending)) = team else {
        return Err(AppError::not_found("Team not found"));
    };
    if captain_id != current_captain_id {
        return Err(AppError::Forbidden);
    }
    if deletion_pending {
        return Err(AppError::conflict("Team is being deleted"));
    }
    if locked {
        let active_game: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1
                     FROM "Participations" participation
                     JOIN "Games" game ON game.id = participation.game_id
                    WHERE participation.team_id = $1
                      AND game.end_time_utc > clock_timestamp()
               )"#,
        )
        .bind(team_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if active_game {
            return Err(AppError::bad_request("Team is locked by an active game"));
        }
    }
    let target_is_member: bool = new_captain_id == captain_id
        || sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "TeamMembers"
                    WHERE team_id = $1 AND user_id = $2
               )"#,
        )
        .bind(team_id)
        .bind(new_captain_id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !target_is_member {
        return Err(AppError::bad_request(
            "New captain must already be a team member",
        ));
    }

    // FOR UPDATE serializes the captain limit across teams and conflicts with
    // the admin deletion fence. A pre-authenticated transfer therefore cannot
    // make an already-fenced account the new captain.
    let target_role: Option<i16> =
        sqlx::query_scalar(r#"SELECT role FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#)
            .bind(new_captain_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if target_role.is_none_or(|role| role == crate::utils::enums::Role::Banned as i16) {
        return Err(AppError::bad_request("New captain not found"));
    }
    let captained: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "Teams" WHERE captain_id = $1"#)
            .bind(new_captain_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if captained >= MAX_TEAMS_ALLOWED as i64 {
        return Err(AppError::bad_request(
            "New captain already captains too many teams",
        ));
    }
    sqlx::query(r#"UPDATE "Teams" SET captain_id = $1 WHERE id = $2"#)
        .bind(new_captain_id)
        .bind(team_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
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

/// Fail-closed teardown of every live container the team owns. Durable service,
/// instance, and container identities are cleared only after the exact backend
/// has been fenced and destroyed, so a failure remains retryable.
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

    for &participation_id in &part_ids {
        lifecycle::destroy_participation_ad_services(st, participation_id).await?;
    }

    let instances = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.is_in(part_ids))
        .all(&st.db)
        .await?;

    for inst in instances {
        let Some(cuuid) = inst.container_id else {
            continue;
        };
        if let Some(c) = container::Entity::find_by_id(cuuid).one(&st.db).await? {
            crate::controllers::game::destroy_managed_container_row(st, &c, false).await?;
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

#[cfg(test)]
#[path = "accept_tests.rs"]
mod accept_tests;

#[cfg(test)]
#[path = "account_lifecycle_tests.rs"]
mod account_lifecycle_tests;
