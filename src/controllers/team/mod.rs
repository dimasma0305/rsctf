//! Ported from RSCTF `Controllers/TeamController.cs` (+ `Repositories/TeamRepository.cs`).
//!
//! Route prefix `/api/team`. Team membership is modelled by the `team_member`
//! join table (RSCTF `Team.Members`): one row per (team, user). The roster is
//! that table, always unioned with the team captain (`team.captain_id`) so a
//! team is never captain-less in the view even if the membership row is missing.

use std::collections::BTreeSet;

use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde::Deserialize;
use std::net::SocketAddr;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::{container, game_instance, participation, team, team_member, user};
use crate::services::anti_cheat;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

mod account_lifecycle;
mod avatar;
mod invite;
mod lifecycle;
mod models;
mod profile;
mod reads;
mod revocation;
pub(crate) mod roster_policy;
mod scoreboard_invalidation;
mod signature;
#[cfg(test)]
use account_lifecycle::create_team_rows;
use account_lifecycle::ensure_player_team_creation_allowed;
pub(crate) use account_lifecycle::{create_team_rows_in, transfer_captain_locked};
pub use avatar::avatar;
pub(crate) use invite::recover_pending_invite_rotations;
pub use invite::{invite_code, update_invite_token};
pub use models::*;
pub(crate) use profile::process_profile_invalidations;
use reads::{load_team_selector, load_user_team_infos};
pub(crate) use revocation::{
    acquire_profile_mutation, acquire_roster_mutation, cleanup_deleted_team_avatar,
    invalidate_removed_membership_cache, mark_team_participations_revoked, require_team_mutable,
    revoke_participation_capabilities, revoke_team_shared_capabilities, TeamDeletionLease,
};
use revocation::{remove_membership, revoke_team_shared_capabilities_locked};
pub(crate) use roster_policy::{ensure_roster_addition_allowed, ensure_roster_change_allowed};
pub(crate) use scoreboard_invalidation::{flush_scoreboard_for_user, flush_scoreboards_for_users};
pub use signature::verify_signature;

/// Each user may captain at most this many teams. Mirrors RSCTF `MaxTeamsAllowed`.
pub(crate) const MAX_TEAMS_ALLOWED: u64 = 3;
/// Defensive roster bound; per-game limits remain authoritative for participation.
pub(crate) const MAX_TEAM_MEMBERS: u64 = 100;
pub(crate) const MAX_TEAM_NAME_CHARS: usize = 128;
pub(crate) const MAX_TEAM_BIO_CHARS: usize = 4_096;

pub(crate) fn validate_team_profile(name: Option<&str>, bio: Option<&str>) -> AppResult<()> {
    if let Some(name) = name {
        let length = name.chars().count();
        if !(1..=MAX_TEAM_NAME_CHARS).contains(&length) {
            return Err(AppError::bad_request(format!(
                "Team name must be between 1 and {MAX_TEAM_NAME_CHARS} characters"
            )));
        }
    }
    if bio.is_some_and(|bio| bio.chars().count() > MAX_TEAM_BIO_CHARS) {
        return Err(AppError::bad_request(format!(
            "Team bio cannot exceed {MAX_TEAM_BIO_CHARS} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn team_profile_bounds_are_character_based() {
        assert!(validate_team_profile(Some(&"界".repeat(128)), Some(&"x".repeat(4_096))).is_ok());
        assert!(validate_team_profile(Some(&"x".repeat(129)), None).is_err());
        assert!(validate_team_profile(Some(""), None).is_err());
        assert!(validate_team_profile(None, Some(&"x".repeat(4_097))).is_err());
    }
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/team", get(get_teams_info).post(create_team))
        .route("/api/team/selector", get(get_team_selector))
        .route(
            "/api/team/{id}",
            get(get_basic_info).put(update_team).delete(delete_team),
        )
        .route(
            "/api/team/{id}/invite",
            get(invite_code).put(update_invite_token),
        )
        .route("/api/team/accept", post(accept))
        .route(
            "/api/team/verify",
            crate::middlewares::rate_limiter::limited(
                crate::middlewares::rate_limiter::Policy::TeamSignatureGlobal,
                crate::middlewares::rate_limiter::limited(
                    crate::middlewares::rate_limiter::Policy::TeamSignatureSource,
                    post(verify_signature),
                ),
            )
            .layer(DefaultBodyLimit::max(signature::BODY_LIMIT_BYTES)),
        )
        .route("/api/team/{id}/leave", post(leave))
        .route("/api/team/{id}/kick/{userId}", post(kick_user))
        .route("/api/team/{id}/transfer", put(transfer))
        .route(
            "/api/team/{id}/avatar",
            put(avatar).layer(DefaultBodyLimit::max(
                crate::utils::upload::IMAGE_BODY_BYTES,
            )),
        )
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
    Ok(RequestResponse::ok(
        load_user_team_infos(st.pg(), user.id).await?,
    ))
}

/// `GET /api/team/selector` — compact, bounded team choices for event joins.
pub async fn get_team_selector(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<Vec<TeamSelectorInfoModel>>> {
    Ok(RequestResponse::ok(
        load_team_selector(st.pg(), user.id).await?,
    ))
}

/// `POST /api/team` — create a team; creator becomes captain.
pub async fn create_team(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Json(model): Json<TeamUpdateModel>,
) -> AppResult<RequestResponse<TeamInfoModel>> {
    let name = model.name.unwrap_or_default().trim().to_string();
    validate_team_profile(Some(&name), model.bio.as_deref())?;
    let operation_id = crate::controllers::edit::control_jobs::operation_id(&headers)?;
    let fingerprint = crate::services::mutation_operations::fingerprint(
        "team-create",
        &(&name, model.bio.as_deref()),
    )?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let replay = crate::services::mutation_operations::claim(
        &mut transaction,
        user.id,
        "team-create",
        "account",
        operation_id,
        fingerprint,
    )
    .await?;
    let (team_id, created) = if let Some(replay) = replay {
        let id = replay
            .result_id
            .parse::<i32>()
            .map_err(|_| AppError::internal("invalid retained team result identity"))?;
        (id, false)
    } else {
        ensure_player_team_creation_allowed(&mut transaction).await?;
        let id = create_team_rows_in(
            &mut transaction,
            user.id,
            &user.security_stamp,
            &name,
            model.bio.as_deref(),
        )
        .await?;
        crate::services::mutation_operations::complete(
            &mut transaction,
            user.id,
            "team-create",
            "account",
            operation_id,
            &id.to_string(),
            None,
        )
        .await?;
        (id, true)
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let team = load_team(&st, team_id).await?;

    // RSCTF `Team_Created` — "Create team {name}" (TeamController, Success).
    if created {
        crate::services::audit::info(
            &st,
            "TeamController",
            Some(user.name.clone()),
            None,
            format!("Create team {}", team.name),
        )
        .await;
    }

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
    let mut roster = acquire_profile_mutation(st.pg(), id).await?;
    require_team_mutable(roster.transaction_mut(), id).await?;
    let info = profile::update_locked(roster.transaction_mut(), id, user.id, model).await?;
    roster.release().await?;
    let effects_state = st.clone();
    tokio::spawn(async move {
        if let Err(error) = profile::process_profile_invalidations(&effects_state).await {
            tracing::warn!(%error, "team profile invalidation failed");
        }
    });
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

    let avatar_hash = deletion_lease.finalize(team.id).await?;
    cleanup_deleted_team_avatar(&st, avatar_hash).await;
    flush_scoreboards_for_games(&st, &affected_game_ids).await;

    // RSCTF `Team_Deleted` — "Delete team {name}" (TeamController, Success).
    crate::services::audit::info(
        &st,
        "TeamController",
        Some(user.name.clone()),
        None,
        format!("Delete team {}", team.name),
    )
    .await;

    Ok(RequestResponse::ok(info))
}

/// `POST /api/team/accept` — join a team via its invite code (`name:id:token`).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TeamAcceptModel {
    Legacy(String),
    Identity(TeamAcceptIdentityModel),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamAcceptIdentityModel {
    pub code: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub fingerprint_proof: Option<String>,
}

async fn lock_live_roster_account(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    expected_security_stamp: &str,
) -> AppResult<()> {
    let account = sqlx::query_as::<_, (bool, i16, Option<String>)>(
        r#"SELECT email_confirmed, role, security_stamp
             FROM "AspNetUsers"
            WHERE id = $1
            FOR SHARE"#,
    )
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if account.is_none_or(|(confirmed, role, stamp)| {
        !confirmed
            || role == crate::utils::enums::Role::Banned as i16
            || stamp.as_deref() != Some(expected_security_stamp)
    }) {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

pub async fn accept(
    State(st): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    user: CurrentUser,
    Json(model): Json<TeamAcceptModel>,
) -> AppResult<StatusCode> {
    let (code, submitted_fingerprint, submitted_proof, legacy_request) = match model {
        TeamAcceptModel::Legacy(code) => (code, None, None, true),
        TeamAcceptModel::Identity(model) => (
            model.code,
            model.fingerprint,
            model.fingerprint_proof,
            false,
        ),
    };
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
    let preflight_policy = anti_cheat::load_policy_flags(st.pg()).await?;
    if legacy_request && preflight_policy.fingerprint_required() {
        return Err(AppError::bad_request(
            "A fresh browser fingerprint proof is required to join a team.",
        ));
    }
    let fingerprint = anti_cheat::validate_fingerprint_submission(
        &st,
        preflight_policy,
        submitted_fingerprint.as_deref(),
        submitted_proof.as_deref(),
    )
    .await?;
    let current_ip = anti_cheat::client_ip(&headers, Some(peer.ip()));

    let roster_key = format!("team-roster:{team_id}");
    let _roster_guard = crate::utils::single_flight::coalesce(&roster_key).await;
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &roster_key).await?;
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

    // Registration happens before a user belongs to a team, so per-team
    // uniqueness must be re-evaluated in the same serialized transaction as
    // this roster insert. Otherwise register-then-join bypasses the policy.
    let identity_admission = admit_team_member_with_roster_fence(
        distributed.transaction_mut(),
        st.config.as_ref(),
        user.id,
        Some(&user.name),
        team_id,
        current_ip.as_deref(),
        fingerprint.as_deref(),
    )
    .await?;
    if identity_admission == anti_cheat::AdmissionOutcome::Blocked {
        // Commit the rejected-attempt audit, but never insert the membership.
        distributed.release().await?;
        return Err(AppError::Coded {
            http: StatusCode::FORBIDDEN,
            code: 403,
            title: anti_cheat::block_message().to_string(),
        });
    }

    // Identity locks precede the account row lock. Retain this share lock until
    // the membership insert commits so deletion either snapshots the new
    // membership or this request observes the durable banned fence.
    lock_live_roster_account(distributed.transaction_mut(), user.id, &user.security_stamp).await?;

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
        &st,
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

#[allow(clippy::too_many_arguments)]
async fn admit_team_member_with_roster_fence(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    config: &crate::models::internal::configs::AppConfig,
    user_id: Uuid,
    user_name: Option<&str>,
    team_id: i32,
    current_ip: Option<&str>,
    fingerprint: Option<&str>,
) -> AppResult<anti_cheat::AdmissionOutcome> {
    let outcome = anti_cheat::admit_team_member_in_transaction(
        transaction,
        config,
        user_id,
        user_name,
        team_id,
        current_ip,
        fingerprint,
    )
    .await?;
    // Identity policy/user/value locks must precede the roster's Game fences.
    // Game registration uses the same roster -> identity -> Games order; a
    // queued exclusive policy update can otherwise complete a three-way cycle.
    ensure_roster_addition_allowed(transaction, team_id).await?;
    Ok(outcome)
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
        &st,
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
        &st,
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
        &user.security_stamp,
        model.new_captain_id,
    )
    .await?;
    distributed.release().await?;
    let team = load_team(&st, id).await?;
    let info = to_info(&st, &team, true).await?;
    Ok(RequestResponse::ok(info))
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
        profile_revision: team.profile_revision,
        members,
    })
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
            format!("_ScoreBoardWireV2_{game_id}"),
            format!("_ScoreBoardWireV2Frozen_{game_id}"),
            format!("_KothScoreBoard_{game_id}"),
            format!("_KothScoreBoardFrozen_{game_id}"),
            format!("_KothScoreBoardWireV2_{game_id}"),
            format!("_KothScoreBoardWireV2Frozen_{game_id}"),
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
