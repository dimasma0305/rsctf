//! Per-event VPN access boundary.
//!
//! The ordinary HTTPS API remains public, but an event can require a short-lived
//! proof that was minted through its WireGuard peer. Proofs are bound to the
//! live user session, participation, peer generation and policy revision.

use std::sync::LazyLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{
    authenticate_token, session_token, CurrentUser,
};
use crate::services::event_security::{
    load_policy, stamp_hash, verify_proof, VpnProofClaims, VPN_PROOF_HEADER,
};
use crate::utils::error::{AppError, AppResult};

const PROOF_SUBJECT_CACHE_TTL: Duration = Duration::from_secs(2);
static PROOF_SUBJECT_FLIGHT: LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);

fn protected_game_path(path: &str) -> Option<i32> {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    if !segments.next()?.eq_ignore_ascii_case("api")
        || !segments.next()?.eq_ignore_ascii_case("game")
    {
        return None;
    }
    let game_id = segments.next()?.parse::<i32>().ok()?;
    if game_id <= 0 {
        return None;
    }
    let suffix = segments.next();
    // The compatibility Toolkit download is another entry point for the same
    // personal profile when the event gate is enabled. Exempt only that exact
    // path; every other A&D/KotH route remains proof-protected.
    let is_toolkit_vpn_config = suffix.is_some_and(|segment| segment.eq_ignore_ascii_case("ad"))
        && segments
            .next()
            .is_some_and(|segment| segment.eq_ignore_ascii_case("vpn"))
        && segments
            .next()
            .is_some_and(|segment| segment.eq_ignore_ascii_case("config"))
        && segments.next().is_none();
    // Joining, the public event summary, enrollment, and the connectivity check
    // must remain reachable before a participant has a VPN profile. Both profile
    // download routes enforce accepted participation in their controllers.
    if suffix.is_none()
        || suffix.is_some_and(|segment| {
            segment.eq_ignore_ascii_case("vpn") || segment.eq_ignore_ascii_case("check")
        })
        || is_toolkit_vpn_config
    {
        return None;
    }
    Some(game_id)
}

fn subject_cache_key(claims: &VpnProofClaims) -> String {
    format!(
        "event-vpn-proof-subject:{}:{}:{}:{}",
        claims.peer_id, claims.peer_generation, claims.policy_revision, claims.security_stamp_hash
    )
}

async fn proof_subject_is_current(
    st: &SharedState,
    claims: &VpnProofClaims,
    expected_security_stamp: &str,
) -> AppResult<bool> {
    let key = subject_cache_key(claims);
    if st.cache.get(&key).await.is_some() {
        return Ok(true);
    }
    let app = st.clone();
    let fill_key = key.clone();
    let claims = claims.clone();
    let expected_security_stamp = expected_security_stamp.to_owned();
    let result = PROOF_SUBJECT_FLIGHT
        .run(&key, move || async move {
            if app.cache.get(&fill_key).await.is_some() {
                return Some(bytes::Bytes::from_static(b"1"));
            }
            let current: bool = sqlx::query_scalar(
                r#"SELECT EXISTS(
                       SELECT 1
                         FROM "EventVpnUserPeers" peer
                         JOIN "Games" game ON game.id = peer.game_id
                         JOIN "Participations" participation
                           ON participation.game_id = peer.game_id
                          AND participation.id = peer.participation_id
                         JOIN "Teams" team ON team.id = participation.team_id
                         JOIN "AspNetUsers" account ON account.id = peer.user_id
                         JOIN "UserParticipations" historical
                           ON historical.user_id = peer.user_id
                          AND historical.game_id = peer.game_id
                          AND historical.team_id = participation.team_id
                          AND historical.participation_id = participation.id
                        WHERE peer.id = $1 AND peer.game_id = $2
                          AND peer.user_id = $3 AND peer.participation_id = $4
                          AND peer.generation = $5 AND peer.revoked_at_utc IS NULL
                          AND game.vpn_policy_revision = $6
                          AND game.vpn_access_required = TRUE
                          AND game.deletion_pending = FALSE
                          AND participation.status = 1
                          AND team.deletion_pending = FALSE
                          AND account.email_confirmed = TRUE
                          AND account.role <> -1
                          AND account.security_stamp = $7
                          AND (
                              team.captain_id = peer.user_id
                              OR EXISTS (
                                  SELECT 1 FROM "TeamMembers" member
                                   WHERE member.team_id = team.id
                                     AND member.user_id = peer.user_id
                              )
                          )
                   )"#,
            )
            .bind(claims.peer_id)
            .bind(claims.game_id)
            .bind(claims.user_id)
            .bind(claims.participation_id)
            .bind(claims.peer_generation)
            .bind(claims.policy_revision)
            .bind(&expected_security_stamp)
            .fetch_one(app.pg())
            .await
            .ok()?;
            if !current {
                return None;
            }
            app.cache
                .set(&fill_key, b"1", Some(PROOF_SUBJECT_CACHE_TTL))
                .await;
            Some(bytes::Bytes::from_static(b"1"))
        })
        .await;
    Ok(result.is_some())
}

fn team_token_matches_game(
    token: &crate::services::ad::api_token::VerifiedTeamToken,
    game_id: i32,
) -> bool {
    token.participation.game_id == game_id
}

async fn authorize_request(
    st: &SharedState,
    headers: &HeaderMap,
    team_token: Option<crate::services::ad::api_token::VerifiedTeamToken>,
    rejected_team_token: bool,
    game_id: i32,
) -> AppResult<Option<CurrentUser>> {
    let policy = load_policy(st, game_id).await?;
    if !policy.gate_active_at(chrono::Utc::now()) {
        return Ok(None);
    }

    // Participation-scoped automation tokens are strong, revocable bearer
    // credentials for headless A&D clients. The outer global middleware has
    // already resolved the token against the current accepted roster. Let the
    // destination A&D handler enforce its narrower operation authorization;
    // do not require browser VPN proof or reinterpret this credential as JWT.
    if let Some(token) = team_token {
        if team_token_matches_game(&token, game_id) {
            return Ok(None);
        }
        return Err(AppError::Unauthorized);
    }
    if rejected_team_token {
        return Err(AppError::Unauthorized);
    }

    let token = session_token(headers).ok_or(AppError::Unauthorized)?;
    let user = authenticate_token(st, &token).await?;
    if user.is_monitor() {
        return Ok(Some(user));
    }
    let proof = headers
        .get(VPN_PROOF_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    let claims = verify_proof(&st.config.event_vpn_credential_key, proof)?;
    if claims.game_id != game_id
        || claims.user_id != user.id
        || claims.policy_revision != policy.revision
        || claims.security_stamp_hash != stamp_hash(&user.security_stamp)
        || !proof_subject_is_current(st, &claims, &user.security_stamp).await?
    {
        return Err(AppError::Unauthorized);
    }
    Ok(Some(user))
}

pub async fn middleware(
    State(st): State<SharedState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(game_id) = protected_game_path(request.uri().path()) else {
        return next.run(request).await;
    };
    let headers = request.headers().clone();
    let team_token = request
        .extensions()
        .get::<crate::services::ad::api_token::VerifiedTeamToken>()
        .cloned();
    let rejected_team_token = request
        .extensions()
        .get::<crate::services::ad::api_token::RejectedTeamToken>()
        .is_some();
    match authorize_request(&st, &headers, team_token, rejected_team_token, game_id).await {
        Ok(user) => {
            if let Some(user) = user {
                request.extensions_mut().insert(user);
            }
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{protected_game_path, team_token_matches_game};
    use crate::models::data::participation;
    use crate::services::ad::api_token::VerifiedTeamToken;
    use crate::utils::enums::ParticipationStatus;

    #[test]
    fn only_post_enrollment_game_paths_are_protected() {
        assert_eq!(protected_game_path("/api/game/7/details"), Some(7));
        assert_eq!(protected_game_path("/api/game/7/scoreboard"), Some(7));
        assert_eq!(protected_game_path("/api/Game/7/Ad/Targets"), Some(7));
        assert_eq!(protected_game_path("/API/gAmE/7/kOtH/hills"), Some(7));
        assert_eq!(protected_game_path("/api/game/7"), None);
        assert_eq!(protected_game_path("/api/game/7/check"), None);
        assert_eq!(protected_game_path("/api/game/7/vpn/config"), None);
        assert_eq!(protected_game_path("/api/Game/7/Check"), None);
        assert_eq!(protected_game_path("/api/Game/7/Vpn/Config"), None);
        assert_eq!(protected_game_path("/api/Game/7/Ad/Vpn/Config"), None);
        assert_eq!(
            protected_game_path("/api/Game/7/Ad/Vpn/Config/extra"),
            Some(7)
        );
        assert_eq!(protected_game_path("/api/Game/7/Ad/Targets"), Some(7));
        assert_eq!(protected_game_path("/api/game/recent"), None);
        assert_eq!(protected_game_path("/api/game/0/details"), None);
        assert_eq!(protected_game_path("/api/game/-1/details"), None);
        assert_eq!(protected_game_path("/api/edit/games/7"), None);
    }

    #[test]
    fn automation_token_is_bound_to_its_exact_game() {
        let token = VerifiedTeamToken {
            participation: participation::Model {
                id: 29,
                status: ParticipationStatus::Accepted,
                token: String::new(),
                writeup_id: None,
                game_id: 7,
                team_id: 11,
                division_id: None,
                suspicion_score: 0,
                competitive_admitted_at_utc: None,
            },
            partition_key: "ad:test".to_string(),
        };
        assert!(team_token_matches_game(&token, 7));
        assert!(!team_token_matches_game(&token, 8));
    }
}

#[cfg(test)]
#[path = "event_vpn_pg_tests.rs"]
mod pg_tests;
