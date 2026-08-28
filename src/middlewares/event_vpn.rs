//! Per-event VPN access boundary.
//!
//! The ordinary HTTPS API remains public, but an event can require a short-lived
//! proof that was minted through its WireGuard peer. Proofs are bound to the
//! live user session, participation, peer generation and policy revision.

use std::sync::LazyLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
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
pub const EVENT_VPN_AUTH_REASON_HEADER: &str = "x-rsctf-auth-reason";
pub const EVENT_VPN_AUTH_REASON: &str = "event-vpn";
static PROOF_SUBJECT_FLIGHT: LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = LazyLock::new(crate::utils::single_flight::SingleFlight::new);

pub fn unauthorized_response() -> Response {
    let mut response = AppError::Unauthorized.into_response();
    debug_assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    response.headers_mut().insert(
        EVENT_VPN_AUTH_REASON_HEADER,
        HeaderValue::from_static(EVENT_VPN_AUTH_REASON),
    );
    response
}

fn protected_game_path(path: &str) -> Option<i32> {
    let mut segments = path.split('/').filter(|segment| !segment.is_empty());
    // Axum's registered A&D compatibility surface intentionally contains
    // mixed-case `/api/Game/{id}/Ad/...` routes. Classify the namespace with
    // the same ASCII case folding used by the browser proof interceptor so a
    // compatibility alias can never skip this gate.
    if !segments.next()?.eq_ignore_ascii_case("api")
        || !segments.next()?.eq_ignore_ascii_case("game")
    {
        return None;
    }
    let game_id_segment = segments.next()?;
    let game_id = game_id_segment
        .parse()
        .ok()
        .filter(|game_id| *game_id > 0)?;
    let suffix = segments.next();
    // Joining, the public event summary, enrollment, and the connectivity check
    // must remain reachable before a participant has a VPN profile.
    if suffix.is_none()
        || suffix.is_some_and(|segment| {
            segment.eq_ignore_ascii_case("vpn") || segment.eq_ignore_ascii_case("check")
        })
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

enum AuthorizationError {
    Session(AppError),
    VpnProof,
    Other(AppError),
}

async fn authorize_request(
    st: &SharedState,
    headers: &HeaderMap,
    game_id: i32,
) -> Result<Option<CurrentUser>, AuthorizationError> {
    let policy = load_policy(st, game_id)
        .await
        .map_err(AuthorizationError::Other)?;
    if !policy.gate_active_at(chrono::Utc::now()) {
        return Ok(None);
    }
    let token =
        session_token(headers).ok_or(AuthorizationError::Session(AppError::Unauthorized))?;
    let user = authenticate_token(st, &token)
        .await
        .map_err(AuthorizationError::Session)?;
    if user.is_monitor() {
        return Ok(Some(user));
    }
    let proof = headers
        .get(VPN_PROOF_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(AuthorizationError::VpnProof)?;
    let claims = verify_proof(&st.config.event_vpn_credential_key, proof)
        .map_err(|_| AuthorizationError::VpnProof)?;
    if claims.game_id != game_id
        || claims.user_id != user.id
        || claims.policy_revision != policy.revision
        || claims.security_stamp_hash != stamp_hash(&user.security_stamp)
        || !proof_subject_is_current(st, &claims, &user.security_stamp)
            .await
            .map_err(AuthorizationError::Other)?
    {
        return Err(AuthorizationError::VpnProof);
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
    match authorize_request(&st, &headers, game_id).await {
        Ok(user) => {
            if let Some(user) = user {
                request.extensions_mut().insert(user);
            }
            next.run(request).await
        }
        Err(AuthorizationError::Session(error) | AuthorizationError::Other(error)) => {
            error.into_response()
        }
        Err(AuthorizationError::VpnProof) => unauthorized_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use axum::middleware::from_fn_with_state;
    use axum::Router;
    use chrono::{Duration as ChronoDuration, Utc};
    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::{
        middleware, protected_game_path, subject_cache_key, EVENT_VPN_AUTH_REASON,
        EVENT_VPN_AUTH_REASON_HEADER,
    };
    use crate::app_state::{AppState, SharedState};
    use crate::models::internal::configs::AppConfig;
    use crate::services::cache::InMemoryCache;
    use crate::services::container::NoopContainerManager;
    use crate::services::event_security::{
        issue_proof, stamp_hash, EventVpnPolicy, VPN_PROOF_HEADER,
    };
    use crate::services::token::TokenService;
    use crate::storage::LocalBlobStorage;
    use crate::utils::enums::Role;

    const KEY: &str = "event-vpn-route-test-key-0123456789abcdef";
    const GAME_ID: i32 = 7;

    const PROTECTED_PATHS: &[&str] = &[
        // Jeopardy surfaces in lowercase, uppercase and mixed case.
        "/api/game/7/challenges/9",
        "/API/GAME/7/CHALLENGES/9",
        "/Api/GaMe/7/Challenges/9",
        // The registered A&D compatibility casing plus canonical variants.
        "/api/game/7/ad/scoreboard",
        "/API/GAME/7/AD/SCOREBOARD",
        "/api/Game/7/Ad/Scoreboard",
        // KotH is served by lowercase and mixed-case A&D aliases.
        "/api/game/7/ad/koth/scoreboard",
        "/API/GAME/7/AD/KOTH/SCOREBOARD",
        "/api/Game/7/Ad/Koth/Scoreboard",
    ];

    const PUBLIC_PATHS: &[&str] = &[
        "/api/game/7",
        "/API/GAME/7",
        "/api/Game/7/Check",
        "/API/game/7/VpN/challenge",
        "/api/game/recent",
        "/api/edit/games/7",
    ];

    fn test_state() -> SharedState {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool);
        let mut config = AppConfig::default();
        config.event_vpn_credential_key = KEY.to_owned();
        AppState::new(
            database,
            Arc::new(config),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(std::env::temp_dir())),
            TokenService::new(KEY, 60),
            Arc::new(NoopContainerManager),
        )
    }

    async fn prime_active_policy(st: &SharedState) {
        let now = Utc::now();
        let policy = EventVpnPolicy {
            game_id: GAME_ID,
            access_required: true,
            behavior_telemetry_enabled: false,
            flag_scan_enabled: false,
            provider_dns_telemetry_enabled: false,
            source_asn_telemetry_enabled: false,
            device_sharing_telemetry_enabled: false,
            revision: 11,
            start_time_utc: now - ChronoDuration::minutes(1),
            end_time_utc: now + ChronoDuration::minutes(1),
            override_active: false,
        };
        st.cache
            .set(
                &format!("event-vpn-policy:{GAME_ID}"),
                &serde_json::to_vec(&policy).unwrap(),
                None,
            )
            .await;
    }

    async fn prime_live_user(st: &SharedState, id: Uuid, role: Role, stamp: &str) {
        let entry = serde_json::json!({
            "Found": {
                "user": { "id": id, "role": role, "name": "route-test" },
                "security_stamp": stamp,
            }
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Cover a scheduler hop across the one-second live-authorization key
        // boundary without weakening the production cache's one-second TTL.
        for window in now.saturating_sub(1)..=now + 2 {
            st.cache
                .set(
                    &format!("_LiveAuthorization_{id}_{window:016x}"),
                    &serde_json::to_vec(&entry).unwrap(),
                    Some(Duration::from_secs(5)),
                )
                .await;
        }
    }

    fn gated_router(st: SharedState) -> Router {
        Router::new()
            .fallback(|| async { StatusCode::NO_CONTENT })
            .layer(from_fn_with_state(st.clone(), middleware))
            .with_state(st)
    }

    async fn get_response(
        router: &Router,
        path: &str,
        token: Option<&str>,
        proof: Option<&str>,
    ) -> axum::response::Response {
        let mut request = Request::get(path);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(proof) = proof {
            request = request.header(VPN_PROOF_HEADER, proof);
        }
        router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn get(
        router: &Router,
        path: &str,
        token: Option<&str>,
        proof: Option<&str>,
    ) -> StatusCode {
        get_response(router, path, token, proof).await.status()
    }

    #[test]
    fn route_classification_is_case_insensitive_for_every_game_mode() {
        for path in PROTECTED_PATHS {
            assert_eq!(protected_game_path(path), Some(GAME_ID), "{path}");
        }
        for path in PUBLIC_PATHS {
            assert_eq!(protected_game_path(path), None, "{path}");
        }
        assert_eq!(protected_game_path("/api/game/0/details"), None);
        assert_eq!(protected_game_path("/api/game/-7/details"), None);
        // Axum's integer path extractor accepts a leading plus sign too, so
        // the middleware must protect the same resolved game id.
        assert_eq!(protected_game_path("/api/game/+7/details"), Some(GAME_ID));
        assert_eq!(protected_game_path("/api/games/7/details"), None);
    }

    #[tokio::test]
    async fn active_gate_denies_anonymous_and_off_vpn_requests_for_every_casing() {
        let st = test_state();
        prime_active_policy(&st).await;
        let user_id = Uuid::new_v4();
        let stamp = "accepted-stamp";
        prime_live_user(&st, user_id, Role::User, stamp).await;
        let token = st
            .token
            .issue(user_id, Role::User, "player", stamp)
            .unwrap();
        let router = gated_router(st);

        for path in PROTECTED_PATHS {
            assert_eq!(
                get(&router, path, None, None).await,
                StatusCode::UNAUTHORIZED,
                "anonymous {path}"
            );
            assert_eq!(
                get(&router, path, Some(&token), None).await,
                StatusCode::UNAUTHORIZED,
                "off-VPN {path}"
            );
        }
    }

    #[tokio::test]
    async fn vpn_denial_is_distinct_from_an_expired_session() {
        let st = test_state();
        prime_active_policy(&st).await;
        let user_id = Uuid::new_v4();
        let stamp = "accepted-stamp";
        prime_live_user(&st, user_id, Role::User, stamp).await;
        let token = st
            .token
            .issue(user_id, Role::User, "player", stamp)
            .unwrap();
        let router = gated_router(st);

        let off_vpn = get_response(&router, "/api/game/7/challenges/9", Some(&token), None).await;
        assert_eq!(off_vpn.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            off_vpn
                .headers()
                .get(EVENT_VPN_AUTH_REASON_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(EVENT_VPN_AUTH_REASON)
        );

        let expired = get_response(
            &router,
            "/api/game/7/challenges/9",
            Some("invalid-session"),
            None,
        )
        .await;
        assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
        assert!(expired
            .headers()
            .get(EVENT_VPN_AUTH_REASON_HEADER)
            .is_none());
    }

    #[tokio::test]
    async fn accepted_proof_and_monitor_boundaries_hold_for_every_casing() {
        let st = test_state();
        prime_active_policy(&st).await;

        let user_id = Uuid::new_v4();
        let stamp = "accepted-stamp";
        prime_live_user(&st, user_id, Role::User, stamp).await;
        let user_token = st
            .token
            .issue(user_id, Role::User, "player", stamp)
            .unwrap();
        let (proof, claims) = issue_proof(
            KEY,
            user_id,
            GAME_ID,
            19,
            Uuid::new_v4(),
            2,
            11,
            stamp_hash(stamp),
        )
        .unwrap();
        let (wrong_game_proof, _) = issue_proof(
            KEY,
            user_id,
            GAME_ID + 1,
            19,
            claims.peer_id,
            2,
            11,
            stamp_hash(stamp),
        )
        .unwrap();
        st.cache
            .set(
                &subject_cache_key(&claims),
                b"1",
                Some(Duration::from_secs(5)),
            )
            .await;

        let monitor_id = Uuid::new_v4();
        let monitor_stamp = "monitor-stamp";
        prime_live_user(&st, monitor_id, Role::Monitor, monitor_stamp).await;
        let monitor_token = st
            .token
            .issue(monitor_id, Role::Monitor, "monitor", monitor_stamp)
            .unwrap();
        let router = gated_router(st);

        for path in PROTECTED_PATHS {
            assert_eq!(
                get(&router, path, Some(&user_token), Some(&proof)).await,
                StatusCode::NO_CONTENT,
                "accepted proof {path}"
            );
            assert_eq!(
                get(&router, path, Some(&user_token), Some("invalid-proof")).await,
                StatusCode::UNAUTHORIZED,
                "invalid proof {path}"
            );
            assert_eq!(
                get(&router, path, Some(&user_token), Some(&wrong_game_proof)).await,
                StatusCode::UNAUTHORIZED,
                "wrong-game proof {path}"
            );
            assert_eq!(
                get(&router, path, Some(&monitor_token), None).await,
                StatusCode::NO_CONTENT,
                "monitor {path}"
            );
        }
    }

    #[tokio::test]
    async fn enrollment_and_proof_bootstrap_routes_remain_public() {
        let st = test_state();
        prime_active_policy(&st).await;
        let router = gated_router(st);

        for path in PUBLIC_PATHS {
            assert_eq!(
                get(&router, path, None, None).await,
                StatusCode::NO_CONTENT,
                "{path}"
            );
        }
    }
}
