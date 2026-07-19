//! Player-facing Attack & Defense endpoints, ported from RSCTF's
//! `Controllers/GameController.cs` A&D surface. All routes live under the
//! `/api/Game/{id}/Ad/...` prefix (mixed-case, matching the documented frontend
//! contract — axum matches paths case-sensitively).
//!
//! # Attack & Defense engine — flow overview
//!
//! RSCTF's A&D subsystem drives a live game where each accepted team runs its
//! own copy of every enabled A&D challenge inside a per-team container/pod, and
//! teams simultaneously attack every other team's copy while defending their own.
//!
//! The engine is round-based (a "tick"): on each advance a fresh random flag is
//! planted into every (team, challenge) container. Flags remain submittable for
//! the configured lifetime window (five ticks by default). An SLA checker probes
//! each service per round and records a verdict.
//! Attackers submit flags stolen from other teams; the official scoreboard
//! aggregates qualified flag, defense, and SLA evidence into fixed epochs.
//!
//! Round/flag/attack/check state is persisted in and read back from the DB; the
//! official scoring math lives in `services/ad/scoring/`. Team API tokens
//! (headless submit) and SSH-key management are modeled by the corresponding
//! A&D tables and player endpoints.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use rand::TryRng;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{CurrentUser, MaybeUser};
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::models::data::{
    ad_ssh_key, ad_team_api_token, ad_team_service, game, game_challenge, participation,
};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{MessageResponse, RequestResponse};

// ---------------------------------------------------------------------------
// Router — paths match Api.ts verbatim (mixed case, case-sensitive).
// ---------------------------------------------------------------------------

fn common_router() -> Router<SharedState> {
    Router::new()
        .route("/api/Game/{id}/Ad/Scoreboard", get(scoreboard))
        .route(
            "/api/Game/{id}/Ad/Services/{adTeamServiceId}/Reset",
            post(reset_service),
        )
        .route(
            "/api/Game/{id}/Ad/Services/{adTeamServiceId}/Snapshot",
            limited(Policy::Container, get(download_snapshot)),
        )
        .route(
            "/api/Game/{id}/Ad/Ssh/Key",
            get(get_ssh_key).post(upload_ssh_key).delete(delete_ssh_key),
        )
        .route("/api/Game/{id}/Ad/Ssh/Key/Generate", post(generate_ssh_key))
        .route("/api/Game/{id}/Ad/State", get(state))
        .route("/api/Game/{id}/Ad/Submit", post(submit))
        .route("/api/Game/{id}/Ad/Targets", get(targets))
        // Lowercase alias the KotH panel calls (KothChallengePanel.tsx hits
        // `/api/game/{id}/ad/targets`; axum matches case-sensitively so the
        // capital route above doesn't cover it) — same handler, same `{id}` param.
        .route("/api/game/{id}/ad/targets", get(targets))
        // KotH game-level surface. The capital `.../Ad/Koth/...` aliases live in
        // THIS router (not koth's lowercase router) so they share the
        // `/api/Game/{id}/Ad/...` prefix + `{id}` param name with the routes above
        // and never trip matchit's param-name conflict check at merge time.
        //
        // Scoreboard/Timeline: capital aliases the KotH arena polls
        // (Attack.tsx: `/api/Game/{id}/Ad/Koth/Scoreboard|Timeline`), pointing at
        // the SAME koth handlers the lowercase routes serve.
        .route(
            "/api/Game/{id}/Ad/Koth/Scoreboard",
            get(crate::controllers::game::koth::scoreboard),
        )
        .route(
            "/api/Game/{id}/Ad/Koth/Timeline",
            get(crate::controllers::game::koth::timeline),
        )
        // Game-level control-token + all-hills endpoints (RSCTF AdGameController
        // `Koth/Token` / `Koth/Hills`), documented by KothGuideModal for scripted
        // play. Dual auth (Bearer ad_... or session), like Submit.
        .route(
            "/api/Game/{id}/Ad/Koth/Token",
            get(crate::controllers::game::koth::koth_token_all),
        )
        .route(
            "/api/Game/{id}/Ad/Koth/Hills",
            get(crate::controllers::game::koth::koth_hills),
        )
        .route(
            "/api/Game/{id}/Ad/Token",
            get(get_token).post(rotate_token).delete(revoke_token),
        )
        .route("/api/Game/{id}/Ad/Vpn/Config", get(download_vpn_config))
        .route(
            "/api/Game/{id}/Ad/Byoc/Setup/{challengeId}",
            get(byoc_setup),
        )
        .route(
            "/api/Game/{id}/Ad/Byoc/Compose/{challengeId}",
            get(byoc_compose),
        )
}

/// Complete monolithic A&D surface, including process-local BYOC endpoints.
pub fn router() -> Router<SharedState> {
    common_router().merge(stateful_router())
}

/// Stateless A&D surface for horizontally-scaled web replicas. Stateful BYOC
/// agent/image connections must reach the singleton network service and are
/// intentionally absent here so a proxy mistake fails closed.
pub fn web_router() -> Router<SharedState> {
    common_router()
}

/// Minimal API surface hosted by the privileged singleton network owner.
///
/// Ordinary account/admin/game traffic belongs on horizontally-scaled `web`
/// replicas. The network process exposes only endpoints that consume its
/// process-local BYOC tunnel registry; keeping the historical aliases here
/// preserves already-downloaded agents when the reverse proxy routes them to
/// this service.
pub fn stateful_router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/Game/{id}/Ad/Byoc/Agent/{participationId}/{challengeId}/{token}",
            get(byoc_agent),
        )
        .route(
            "/api/Game/{id}/Ad/Byoc/Image/{participationId}/{challengeId}/{token}",
            get(byoc_image),
        )
        .route(
            "/api/stateful/Game/{id}/Ad/Byoc/Agent/{participationId}/{challengeId}/{token}",
            get(byoc_agent),
        )
        .route(
            "/api/stateful/Game/{id}/Ad/Byoc/Image/{participationId}/{challengeId}/{token}",
            get(byoc_image),
        )
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Map a persisted `i16` checker verdict to its RSCTF string label.
fn status_str(v: i16) -> String {
    match v {
        0 => "Ok",
        1 => "Mumble",
        2 => "Offline",
        _ => "InternalError",
    }
    .to_string()
}

/// Resolve the caller's accepted participation in a game via the
/// UserParticipations link (mirrors RSCTF's `ResolveUserParticipationAsync`).
/// Cache key for a user's resolved participation in a game. Invalidate
/// (`cache::remove`) wherever a participation is created/deleted or its status changes —
/// join/leave/accept/reject/ban — so a revoked player loses access at once, not on TTL.
pub(crate) fn participation_cache_key(user_id: uuid::Uuid, game_id: i32) -> String {
    format!("_Part_{user_id}_{game_id}")
}

/// Nullable participation columns preserve the distinction between an absent
/// membership link (`fetch_optional` returns `None`) and an orphaned link (the
/// `LEFT JOIN` returns a row whose participation id is `None`). The latter is a
/// 404 in the existing API contract, while the former is a 400.
#[derive(Debug, sqlx::FromRow)]
struct ResolvedParticipationRow {
    participation_id: Option<i32>,
    participation_status: Option<i16>,
    participation_token: Option<String>,
    participation_writeup_id: Option<i32>,
    participation_game_id: Option<i32>,
    participation_team_id: Option<i32>,
    participation_division_id: Option<i32>,
    participation_suspicion_score: Option<i32>,
}

const RESOLVE_PARTICIPATION_SQL: &str = r#"
SELECT participation.id AS participation_id,
       participation.status AS participation_status,
       participation.token AS participation_token,
       participation.writeup_id AS participation_writeup_id,
       participation.game_id AS participation_game_id,
       participation.team_id AS participation_team_id,
       participation.division_id AS participation_division_id,
       participation.suspicion_score AS participation_suspicion_score
  FROM "UserParticipations" membership
  LEFT JOIN "Participations" participation
    ON participation.id = membership.participation_id
 WHERE membership.user_id = $1
   AND membership.game_id = $2
"#;

fn missing_participation_column(column: &str) -> AppError {
    AppError::internal(format!(
        "resolved participation row is missing required column {column}"
    ))
}

fn map_resolved_participation(
    row: Option<ResolvedParticipationRow>,
) -> AppResult<participation::Model> {
    let row = row.ok_or_else(|| AppError::bad_request("Not participating in this game"))?;
    let id = row
        .participation_id
        .ok_or_else(|| AppError::not_found("Participation not found"))?;
    let status_value = row
        .participation_status
        .ok_or_else(|| missing_participation_column("status"))?;
    let status = <ParticipationStatus as sea_orm::ActiveEnum>::try_from_value(&status_value)
        .map_err(|error| AppError::internal(error.to_string()))?;
    let part = participation::Model {
        id,
        status,
        token: row
            .participation_token
            .ok_or_else(|| missing_participation_column("token"))?,
        writeup_id: row.participation_writeup_id,
        game_id: row
            .participation_game_id
            .ok_or_else(|| missing_participation_column("game_id"))?,
        team_id: row
            .participation_team_id
            .ok_or_else(|| missing_participation_column("team_id"))?,
        division_id: row.participation_division_id,
        suspicion_score: row
            .participation_suspicion_score
            .ok_or_else(|| missing_participation_column("suspicion_score"))?,
    };
    if part.status != ParticipationStatus::Accepted {
        return Err(AppError::bad_request("Participation not accepted"));
    }
    Ok(part)
}

async fn load_resolved_participation<'e, E>(
    executor: E,
    user_id: uuid::Uuid,
    game_id: i32,
) -> AppResult<participation::Model>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row = sqlx::query_as::<_, ResolvedParticipationRow>(RESOLVE_PARTICIPATION_SQL)
        .bind(user_id)
        .bind(game_id)
        .fetch_optional(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    map_resolved_participation(row)
}

/// Resolve (and gate on `Accepted`) the caller's participation. This ran two DB queries
/// (`user_participation` → `participation`) on EVERY authenticated A&D/KotH poll — the
/// dominant per-request DB cost across the whole polled surface. Cached per (user, game)
/// behind a short TTL + single-flight (the client batches several auth polls at once, so
/// coalescing collapses a cold batch to one resolve).
///
/// Safety: only an **Accepted** participation is ever cached, and a cache hit re-checks
/// the status, so the gate can never be *weakened* by the cache. The 5 s TTL bounds
/// staleness — a ban/leave/removal takes effect within 5 s even if an invalidation site
/// is missed; the known mutation points also flush the key for immediate effect.
pub(crate) async fn resolve_participation(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
) -> AppResult<participation::Model> {
    let key = participation_cache_key(user.id, game_id);
    if let Some(p) = cached_accepted_participation(st, &key).await {
        return Ok(p);
    }
    let _flight = crate::utils::single_flight::coalesce(&key).await;
    if let Some(p) = cached_accepted_participation(st, &key).await {
        return Ok(p);
    }
    let part = load_resolved_participation(st.pg(), user.id, game_id).await?;
    if let Ok(j) = serde_json::to_vec(&part) {
        st.cache
            .set(&key, &j, Some(std::time::Duration::from_secs(5)))
            .await;
    }
    Ok(part)
}

/// A cached participation, but ONLY if it is still `Accepted` — never lets the cache
/// weaken `resolve_participation`'s gate.
async fn cached_accepted_participation(
    st: &SharedState,
    key: &str,
) -> Option<participation::Model> {
    let bytes = st.cache.get(key).await?;
    let part = serde_json::from_slice::<participation::Model>(&bytes).ok()?;
    (part.status == ParticipationStatus::Accepted).then_some(part)
}

/// Flush the `resolve_participation` cache for every member of a participation — a team
/// shares one participation across its members, so a status change / removal must clear
/// them all. Best-effort (removing an absent key is a no-op).
pub(crate) async fn flush_participation_cache(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
) {
    if let Ok(uids) = sqlx::query_scalar::<_, uuid::Uuid>(
        r#"SELECT user_id FROM "UserParticipations" WHERE participation_id = $1"#,
    )
    .bind(participation_id)
    .fetch_all(st.pg())
    .await
    {
        for uid in uids {
            st.cache
                .remove(&participation_cache_key(uid, game_id))
                .await;
        }
    }
}

/// Flush the `resolve_participation` cache for every (user, game) a team participates in —
/// for a whole-team removal (disband / admin delete) that can span games.
pub(crate) async fn flush_team_participation_cache(st: &SharedState, team_id: i32) {
    if let Ok(rows) = sqlx::query_as::<_, (uuid::Uuid, i32)>(
        r#"SELECT user_id, game_id FROM "UserParticipations" WHERE team_id = $1"#,
    )
    .bind(team_id)
    .fetch_all(st.pg())
    .await
    {
        for (uid, gid) in rows {
            st.cache.remove(&participation_cache_key(uid, gid)).await;
        }
    }
}

/// Resolve the flag submitter for a dual-auth A&D endpoint: an `Authorization:
/// Bearer ad_...` team API token wins (scripted exploits), else the interactive
/// session's participation. Mirrors RSCTF's
/// `ResolveTeamApiTokenAsync ?? ResolveUserParticipationAsync`.
pub(crate) async fn resolve_ad_attacker(
    st: &SharedState,
    headers: &HeaderMap,
    verified: Option<&crate::services::ad::api_token::VerifiedTeamToken>,
    rejected: Option<&crate::services::ad::api_token::RejectedTeamToken>,
    maybe_user: MaybeUser,
    game_id: i32,
) -> AppResult<participation::Model> {
    reject_prechecked_team_token(rejected)?;
    if let Some(part) = resolve_team_api_token(st, headers, verified, game_id).await? {
        return Ok(part);
    }
    if let Some(user) = maybe_user.0 {
        return resolve_participation(st, &user, game_id).await;
    }
    Err(AppError::Unauthorized)
}

fn reject_prechecked_team_token(
    rejected: Option<&crate::services::ad::api_token::RejectedTeamToken>,
) -> AppResult<()> {
    if rejected.is_some() {
        Err(AppError::Unauthorized)
    } else {
        Ok(())
    }
}

/// Fill `buf` with cryptographically-random bytes from the OS CSPRNG.
fn fill_random(buf: &mut [u8]) {
    rand::rngs::SysRng
        .try_fill_bytes(buf)
        .expect("operating-system CSPRNG unavailable");
}

// ---------------------------------------------------------------------------
// Submodules — split by cohesive responsibility (routes/handlers unchanged).
// ---------------------------------------------------------------------------

mod byoc;
mod byoc_authorization;
mod scoreboard;
mod scoreboard_encoding;
mod ssh;
mod state_tail;
mod submit;
mod targets;
mod token;
mod vpn;

pub use byoc::*;
pub use scoreboard::*;
pub use ssh::*;
pub use submit::*;
pub use targets::*;
pub use token::*;
pub use vpn::*;

#[cfg(test)]
mod tests {
    use sqlx::{Connection, PgConnection};

    use super::*;

    fn complete_resolved_row(status: ParticipationStatus) -> ResolvedParticipationRow {
        ResolvedParticipationRow {
            participation_id: Some(41),
            participation_status: Some(status as i16),
            participation_token: Some("participation-token".to_string()),
            participation_writeup_id: Some(51),
            participation_game_id: Some(61),
            participation_team_id: Some(71),
            participation_division_id: Some(81),
            participation_suspicion_score: Some(91),
        }
    }

    fn orphaned_resolved_row() -> ResolvedParticipationRow {
        ResolvedParticipationRow {
            participation_id: None,
            participation_status: None,
            participation_token: None,
            participation_writeup_id: None,
            participation_game_id: None,
            participation_team_id: None,
            participation_division_id: None,
            participation_suspicion_score: None,
        }
    }

    #[test]
    fn middleware_rejected_team_token_is_a_terminal_unauthorized_result() {
        let rejected = crate::services::ad::api_token::RejectedTeamToken;
        let error = reject_prechecked_team_token(Some(&rejected)).unwrap_err();
        assert!(matches!(error, AppError::Unauthorized));
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
        assert!(reject_prechecked_team_token(None).is_ok());
    }

    #[test]
    fn participation_join_row_projects_every_model_field_exactly() {
        let actual =
            map_resolved_participation(Some(complete_resolved_row(ParticipationStatus::Accepted)))
                .unwrap();

        assert_eq!(
            actual,
            participation::Model {
                id: 41,
                status: ParticipationStatus::Accepted,
                token: "participation-token".to_string(),
                writeup_id: Some(51),
                game_id: 61,
                team_id: 71,
                division_id: Some(81),
                suspicion_score: 91,
            }
        );
    }

    #[test]
    fn participation_join_row_preserves_nullable_model_fields() {
        let mut row = complete_resolved_row(ParticipationStatus::Accepted);
        row.participation_writeup_id = None;
        row.participation_division_id = None;

        let actual = map_resolved_participation(Some(row)).unwrap();

        assert_eq!(actual.writeup_id, None);
        assert_eq!(actual.division_id, None);
    }

    #[test]
    fn participation_join_mapping_preserves_existing_api_errors() {
        let no_link = map_resolved_participation(None).unwrap_err();
        assert_eq!(no_link.status(), StatusCode::BAD_REQUEST);
        assert_eq!(no_link.to_string(), "Not participating in this game");

        let orphan = map_resolved_participation(Some(orphaned_resolved_row())).unwrap_err();
        assert_eq!(orphan.status(), StatusCode::NOT_FOUND);
        assert_eq!(orphan.to_string(), "Participation not found");

        let pending =
            map_resolved_participation(Some(complete_resolved_row(ParticipationStatus::Pending)))
                .unwrap_err();
        assert_eq!(pending.status(), StatusCode::BAD_REQUEST);
        assert_eq!(pending.to_string(), "Participation not accepted");
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn participation_join_query_distinguishes_all_membership_states() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              status SMALLINT NOT NULL,
              token TEXT NOT NULL,
              writeup_id INTEGER,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              division_id INTEGER,
              suspicion_score INTEGER NOT NULL
            );
            CREATE TEMP TABLE "UserParticipations" (
              user_id UUID NOT NULL,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              PRIMARY KEY (user_id, game_id)
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let no_link_user = uuid::Uuid::from_u128(1);
        let no_link = load_resolved_participation(&mut connection, no_link_user, 10)
            .await
            .unwrap_err();
        assert_eq!(no_link.status(), StatusCode::BAD_REQUEST);
        assert_eq!(no_link.to_string(), "Not participating in this game");

        let orphan_user = uuid::Uuid::from_u128(2);
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
               (user_id, game_id, team_id, participation_id)
               VALUES ($1, 10, 20, 999)"#,
        )
        .bind(orphan_user)
        .execute(&mut connection)
        .await
        .unwrap();
        let orphan = load_resolved_participation(&mut connection, orphan_user, 10)
            .await
            .unwrap_err();
        assert_eq!(orphan.status(), StatusCode::NOT_FOUND);
        assert_eq!(orphan.to_string(), "Participation not found");

        sqlx::query(
            r#"INSERT INTO "Participations"
               (id, status, token, writeup_id, game_id, team_id, division_id, suspicion_score)
               VALUES
               (11, $1, 'accepted-token', 101, 10, 21, 201, 301),
               (12, $2, 'pending-token', NULL, 10, 22, NULL, 302)"#,
        )
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ParticipationStatus::Pending as i16)
        .execute(&mut connection)
        .await
        .unwrap();

        let accepted_user = uuid::Uuid::from_u128(3);
        let pending_user = uuid::Uuid::from_u128(4);
        sqlx::query(
            r#"INSERT INTO "UserParticipations"
               (user_id, game_id, team_id, participation_id)
               VALUES ($1, 10, 21, 11), ($2, 10, 22, 12)"#,
        )
        .bind(accepted_user)
        .bind(pending_user)
        .execute(&mut connection)
        .await
        .unwrap();

        let accepted = load_resolved_participation(&mut connection, accepted_user, 10)
            .await
            .unwrap();
        assert_eq!(
            accepted,
            participation::Model {
                id: 11,
                status: ParticipationStatus::Accepted,
                token: "accepted-token".to_string(),
                writeup_id: Some(101),
                game_id: 10,
                team_id: 21,
                division_id: Some(201),
                suspicion_score: 301,
            }
        );

        let pending = load_resolved_participation(&mut connection, pending_user, 10)
            .await
            .unwrap_err();
        assert_eq!(pending.status(), StatusCode::BAD_REQUEST);
        assert_eq!(pending.to_string(), "Participation not accepted");
    }
}
