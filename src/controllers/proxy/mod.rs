//! Player and administrator WebSocket proxy routes.
//! (per-container WebSocket proxy).
//!
//! The two routes tunnel a raw TCP connection to a byoc ("bring your own
//! container") challenge instance over a WebSocket, so a browser terminal can
//! talk to an SSH/pwn/HTTP service running inside the container:
//!
//!   * `GET /api/proxy/{id}`         — a live player instance. Access is gated:
//!     game containers must belong to the caller's participation; exercise
//!     containers must belong to the caller's exact per-user exercise instance.
//!   * `GET /api/proxy/noinst/{id}`  — an admin "no instance" test container.
//!     Requires a live admin session or an exact short-lived WSRX capability,
//!     and the container must NOT be linked to any game or exercise instance
//!     (throwaway test container only).
//!
//! On a WebSocket upgrade we resolve the container GUID to its `Containers` row,
//! derive its reachable `ip:port` (game.rs stores the host-published address
//! there for the Docker backend), open a `tokio::net::TcpStream` to it, and pump
//! bytes bidirectionally — inbound WebSocket Binary/Text frames become TCP
//! writes, TCP reads become outbound WebSocket Binary frames — until either side
//! closes.
//!
//! Everything degrades gracefully: a missing/forbidden/unreachable container
//! never yields a 500. We accept the upgrade and close the socket cleanly
//! (RSCTF returns 418/404, but for a WebSocket handshake a clean close is the
//! faithful graceful behaviour).
//!
//! On a successful open of a *player* instance we also best-effort record a
//! [`container_access_event`](crate::models::data::container_access_event) row
//! (RSCTF `ContainerAccessLogger`) — the ground-truth access log the
//! container-access cheat detectors correlate against solves — and, when the
//! accessing team differs from the container owner while the game is live, raise
//! `CrossTeamContainerAccess`. Neither ever breaks the tunnel. Long-lived player
//! sessions are capped per user, participation and workload so one team cannot
//! consume every trusted-worker data stream.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use sea_orm::EntityTrait;
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{CurrentUser, MaybeUser};
use crate::models::data::{container, game_instance};
use crate::services::live_roster::LiveParticipationIdentity;
use crate::services::worker::WorkerHandle;
use crate::utils::enums::{ParticipationStatus, Role};
use rsctf_worker_protocol::{
    DataStreamRequest, TcpProxyRequest, ValidatedWorkloadSpec, WorkloadFence,
};

mod access_log;
mod authorization;
mod capability;
mod egress;
mod target;
#[cfg(test)]
mod tests;
mod transport;

use crate::services::authorization_lease::LeaseGenerationCache;
use access_log::log_container_access_on;
use authorization::{
    exercise_lease_is_valid, game_proxy_scope_is_valid, game_proxy_session_is_valid,
    try_acquire_game_proxy_open_fence, GameProxyOpenFence, GameProxyTargetIdentity,
};
use capability::{
    issue_instance_capability, issue_noinstance_capability, proxy_instance_latency_probe,
    proxy_noinstance_latency_probe, proxy_user, ProxyCapabilityQuery,
};
use egress::{build_egress_scan, load_egress_participation, EgressMetadataRevision, EgressScan};
use target::{game_proxy_target_identity, proxy_target, resolve_noinstance_target, ProxyTarget};
use transport::{close_at_capacity, close_cleanly, endpoint_unavailable_close, proxy_pump};

/// Maximum client frame and reassembled message; raw TCP clients can segment writes.
const MAX_CLIENT_MESSAGE_SIZE: usize = 64 * 1024;

/// Cap on how long we wait for the TCP connect to the container to succeed. An
/// unreachable IP would otherwise hang for the OS default (minutes) on an
/// already-upgraded socket — a slow hang is not "degrade gracefully".
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Hard ceiling on a single proxied session, mirroring RSCTF's 30-minute
/// `CancelAfter` on the proxy pump.
const SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub fn router() -> Router<SharedState> {
    Router::new()
        // GET /api/proxy/{id} — proxy TCP over websocket for a live instance.
        .route(
            "/api/proxy/{id}",
            get(proxy_for_instance).options(proxy_instance_latency_probe),
        )
        .route(
            "/api/proxy/{id}/capability",
            post(issue_instance_capability),
        )
        // GET /api/proxy/noinst/{id} — proxy TCP over websocket for admin test containers.
        .route(
            "/api/proxy/noinst/{id}",
            get(proxy_for_noinstance).options(proxy_noinstance_latency_probe),
        )
        .route(
            "/api/proxy/noinst/{id}/capability",
            post(issue_noinstance_capability),
        )
}

/// `GET /api/proxy/{id}` — TCP-over-WebSocket proxy to a player's container.
///
/// Resolves the container and enforces that it is a proxy container owned by the
/// caller's participation, then pumps bytes. Any failure (unauthenticated, not
/// owned, missing, or unreachable) results in a clean WebSocket close rather
/// than an error status.
async fn proxy_for_instance(
    State(st): State<SharedState>,
    user: MaybeUser,
    Query(capability): Query<ProxyCapabilityQuery>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let user = proxy_user(&st, user, capability, id, false).await;
    // Capture the connecting IP + User-Agent the same way the rest of rsctf does,
    // BEFORE the upgrade consumes the request. Used only for the access-event row
    // (best-effort forensics), never for access control.
    let remote_ip =
        crate::services::anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_default();
    let admission_source = remote_ip.parse().unwrap_or_else(|_| peer.ip());
    if let Some(subject) = user.as_ref().map(|user| user.id) {
        if let Some(response) =
            crate::middlewares::rate_limiter::admit_proxy_open(subject, &remote_ip, id, None).await
        {
            return response;
        }
    }
    // Only admitted opens resolve container and participation state.
    let access = resolve_instance_target(&st, MaybeUser(user), id).await;
    if let Some(participation_id) = access.as_ref().and_then(|access| match &access.owner {
        InstanceOwner::Game(game) => Some(game.accessing_participation_id),
        InstanceOwner::Exercise(_) => None,
    }) {
        if let Some(response) =
            crate::middlewares::rate_limiter::admit_proxy_participation(participation_id).await
        {
            return response;
        }
    }
    let event_vpn_source = remote_ip.parse::<Ipv4Addr>().ok();
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|ua| ua.chars().take(512).collect::<String>()) // RSCTF caps UA at 512
        .filter(|ua| !ua.is_empty());

    let st_log = st.clone();
    ws.max_frame_size(MAX_CLIENT_MESSAGE_SIZE)
        .max_message_size(MAX_CLIENT_MESSAGE_SIZE)
        .on_upgrade(move |socket| async move {
            let (endpoint, scan, lease, admission, open_fence) = match access {
                Some(a) => {
                    let (admission, scan, lease, open_fence) = match &a.owner {
                        InstanceOwner::Game(game) => {
                            let Some(admission) = st_log
                                .proxy_admission
                                .try_acquire_distributed(
                                    st_log.pg(),
                                    a.accessing_user_id,
                                    game.accessing_participation_id,
                                    game.game_id,
                                    a.container_id,
                                    admission_source,
                                )
                                .await
                            else {
                                close_at_capacity(socket).await;
                                return;
                            };
                            // Egress metadata is preparatory only and may use a
                            // second pool connection. Complete it before the
                            // one-connection final authorization transaction.
                            let scan =
                                build_egress_scan(&st_log, &a, game, remote_ip.clone()).await;
                            // Request-level resolution can become stale while
                            // HTTP upgrades. Revalidate the exact stamped roster,
                            // effective division policy, and container/backend
                            // identity here. The specialized helper takes the
                            // suspicion advisory before row locks.
                            let Some(mut open_fence) = try_acquire_game_proxy_open_fence(
                                st_log.pg(),
                                LiveParticipationIdentity {
                                    user_id: a.accessing_user_id,
                                    expected_security_stamp: &a.accessing_security_stamp,
                                    game_id: game.game_id,
                                    team_id: game.accessing_team_id,
                                    participation_id: game.accessing_participation_id,
                                },
                                game.challenge_id,
                                &game.target_identity,
                                event_vpn_source,
                                game.is_monitor,
                            )
                            .await
                            else {
                                run_or_close(st_log, socket, None, None, None, None, None).await;
                                return;
                            };
                            // Stage access evidence on that same transaction.
                            // A failed insert rolls the final authorization back;
                            // no unaudited backend stream is opened.
                            if let Err(error) = log_container_access_on(
                                open_fence.transaction_mut(),
                                &st_log,
                                &a,
                                game,
                                remote_ip,
                                user_agent,
                            )
                            .await
                            {
                                tracing::warn!(
                                    container = %a.container_id,
                                    %error,
                                    "container access evidence transaction failed"
                                );
                                open_fence.rollback().await;
                                run_or_close(st_log, socket, None, None, None, None, None).await;
                                return;
                            }
                            let lease = InstanceLease {
                                pool: st_log.pg().clone(),
                                user_id: a.accessing_user_id,
                                security_stamp: a.accessing_security_stamp.clone(),
                                owner: LeaseOwner::Game {
                                    game_id: game.game_id,
                                    team_id: game.accessing_team_id,
                                    participation_id: game.accessing_participation_id,
                                    challenge_id: game.challenge_id,
                                    target_identity: game.target_identity.clone(),
                                    event_vpn_source,
                                    bypass_event_vpn: game.is_monitor,
                                },
                            };
                            (admission, scan, lease, Some(open_fence))
                        }
                        InstanceOwner::Exercise(exercise) => {
                            let Some(admission) = st_log
                                .proxy_admission
                                .try_acquire_exercise_distributed(
                                    st_log.pg(),
                                    a.accessing_user_id,
                                    exercise.exercise_instance_id,
                                    a.container_id,
                                    admission_source,
                                )
                                .await
                            else {
                                close_at_capacity(socket).await;
                                return;
                            };
                            let lease = InstanceLease {
                                pool: st_log.pg().clone(),
                                user_id: a.accessing_user_id,
                                security_stamp: a.accessing_security_stamp.clone(),
                                owner: LeaseOwner::Exercise {
                                    exercise_instance_id: exercise.exercise_instance_id,
                                    exercise_id: exercise.exercise_id,
                                    container_id: a.container_id,
                                },
                            };
                            (admission, None, lease, None)
                        }
                    };
                    (
                        Some(a.endpoint),
                        scan,
                        Some(lease),
                        Some(admission),
                        open_fence,
                    )
                }
                None => (None, None, None, None, None),
            };
            run_or_close(st_log, socket, endpoint, scan, lease, admission, open_fence).await;
        })
        .into_response()
}

/// `GET /api/proxy/noinst/{id}` — TCP-over-WebSocket proxy to an admin test
/// (NoInstance) container. A live admin browser session or an exact
/// admin-minted WSRX capability gates the route; the container must be a proxy
/// container that is not linked to any game or exercise instance.
async fn proxy_for_noinstance(
    State(st): State<SharedState>,
    user: MaybeUser,
    Query(capability): Query<ProxyCapabilityQuery>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let remote_ip =
        crate::services::anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_default();
    let source = remote_ip.parse().unwrap_or_else(|_| peer.ip());
    let principal = proxy_user(&st, user, capability, id, true).await;
    if let Some(subject) = principal.as_ref().map(|user| user.id) {
        if let Some(response) =
            crate::middlewares::rate_limiter::admit_proxy_open(subject, &remote_ip, id, None).await
        {
            return response;
        }
    }
    let admission = match principal.as_ref() {
        Some(principal) => {
            st.proxy_admission
                .try_acquire_preview_distributed(st.pg(), principal.id, id, source)
                .await
        }
        None => None,
    };
    if principal.is_some() && admission.is_none() {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, "2")],
            "proxy capacity exceeded",
        )
            .into_response();
    }
    let target = if principal.is_some() {
        resolve_noinstance_target(&st, id).await
    } else {
        None
    };
    let lease = principal.map(|principal| InstanceLease {
        pool: st.pg().clone(),
        user_id: principal.id,
        security_stamp: principal.security_stamp,
        owner: LeaseOwner::Preview { container_id: id },
    });
    ws.max_frame_size(MAX_CLIENT_MESSAGE_SIZE)
        .max_message_size(MAX_CLIENT_MESSAGE_SIZE)
        .on_upgrade(move |socket| run_or_close(st, socket, target, None, lease, admission, None))
        .into_response()
}

/// Everything needed both to proxy a player container AND to log the access +
/// run cross-team detection on open (RSCTF `ContainerAccessContext`).
struct InstanceAccess {
    /// The reachable `ip:port` the proxy dials.
    endpoint: ProxyTarget,
    container_id: Uuid,
    accessing_user_id: Uuid,
    accessing_user_name: String,
    accessing_security_stamp: String,
    owner: InstanceOwner,
}

enum InstanceOwner {
    Game(GameAccess),
    Exercise(ExerciseAccess),
}

struct GameAccess {
    game_id: i32,
    accessing_team_id: i32,
    challenge_id: i32,
    /// Participation that owns the container (its `GameInstance`'s team).
    owner_participation_id: i32,
    /// The accessing user's own participation in this game.
    accessing_participation_id: i32,
    /// Monotonic event challenge revision plus the exact per-instance flag row.
    /// Together these make flag-egress metadata cache entries immutable.
    egress_revision: Option<EgressMetadataRevision>,
    target_identity: GameProxyTargetIdentity,
    /// Monitor/Admin — legitimately reaches any container, so never flagged.
    is_monitor: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExerciseAccess {
    exercise_instance_id: i32,
    exercise_id: i32,
}

/// Resolve the reachable `ip:port` for a player's proxy container, enforcing
/// game-participation or exercise-instance ownership, plus the context needed
/// for the applicable session controls. Returns `None` (→ clean close) on any
/// failure.
///
/// All DB lookups swallow errors into `None` so a transient DB blip closes the
/// socket cleanly rather than surfacing a 500 on an upgraded connection.
async fn resolve_instance_target(
    st: &SharedState,
    user: MaybeUser,
    id: Uuid,
) -> Option<InstanceAccess> {
    let user = user.0?;

    let container = container::Entity::find_by_id(id).one(&st.db).await.ok()??;
    if !container.is_proxy {
        return None;
    }

    if container.game_instance_id.is_none() {
        match resolve_exercise_instance_target(st, &user, &container).await {
            ExerciseResolution::Granted(access) => return Some(*access),
            ExerciseResolution::Denied => return None,
            ExerciseResolution::NotExercise => {
                return resolve_shared_instance_target(st, &user, container).await;
            }
        }
    }

    // The container must belong to a game instance owned by the caller's
    // participation: container → instance → participation → (game, team), and
    // the caller must be registered on that exact participation in that game.
    let gi_id = container.game_instance_id?;
    let instance = game_instance::Entity::find_by_id(gi_id)
        .one(&st.db)
        .await
        .ok()??;
    let part = load_egress_participation(st.pg(), instance.participation_id, user.id).await?;
    let target_identity = game_proxy_target_identity(&container, Some(instance.id));
    if !game_proxy_scope_is_valid(
        st.pg(),
        LiveParticipationIdentity {
            user_id: user.id,
            expected_security_stamp: &user.security_stamp,
            game_id: part.game_id,
            team_id: part.team_id,
            participation_id: part.id,
        },
        instance.challenge_id,
        &target_identity,
    )
    .await
    {
        return None;
    }

    let endpoint = proxy_target(&container)?;
    Some(InstanceAccess {
        endpoint,
        container_id: id,
        accessing_user_id: user.id,
        accessing_user_name: user.name.clone(),
        accessing_security_stamp: user.security_stamp.clone(),
        owner: InstanceOwner::Game(GameAccess {
            game_id: part.game_id,
            accessing_team_id: part.team_id,
            challenge_id: instance.challenge_id,
            owner_participation_id: part.id,
            accessing_participation_id: part.id,
            egress_revision: instance.flag_id.map(|flag_id| EgressMetadataRevision {
                challenge_configuration_revision: part.challenge_configuration_revision,
                flag_id,
            }),
            target_identity,
            is_monitor: user.is_monitor(),
        }),
    })
}

const EXERCISE_ACCESS_SQL: &str = r#"SELECT instance.id AS exercise_instance_id,
           instance.exercise_id,
           instance.user_id,
           instance.is_loaded,
           exercise.is_enabled,
           exercise.publish_time_utc
      FROM "ExerciseInstances" instance
      JOIN "ExerciseChallenges" exercise ON exercise.id = instance.exercise_id
     WHERE instance.container_id = $1
       AND ($2::INTEGER IS NULL OR instance.id = $2)
  ORDER BY instance.id
     LIMIT 2"#;

const LEGACY_EXERCISE_OWNER_SQL: &str = r#"SELECT EXISTS (
    SELECT 1 FROM "ExerciseInstances" WHERE container_id = $1
)"#;

#[derive(Clone, Debug, sqlx::FromRow)]
struct ExerciseAccessRow {
    exercise_instance_id: i32,
    exercise_id: i32,
    user_id: Uuid,
    is_loaded: bool,
    is_enabled: bool,
    publish_time_utc: chrono::DateTime<chrono::Utc>,
}

enum ExerciseResolution {
    Granted(Box<InstanceAccess>),
    Denied,
    NotExercise,
}

/// Resolve both new forward-linked exercise containers and legacy rows which
/// only have `ExerciseInstances.container_id`. Once any exercise owner exists,
/// an ownership mismatch fails closed and never falls through to shared-game
/// authorization.
async fn resolve_exercise_instance_target(
    st: &SharedState,
    user: &CurrentUser,
    container: &container::Model,
) -> ExerciseResolution {
    let rows = match sqlx::query_as::<_, ExerciseAccessRow>(EXERCISE_ACCESS_SQL)
        .bind(container.id)
        .bind(container.exercise_instance_id)
        .fetch_all(st.pg())
        .await
    {
        Ok(rows) => rows,
        Err(_) => return ExerciseResolution::Denied,
    };
    if rows.is_empty() {
        return if container.exercise_instance_id.is_some() {
            ExerciseResolution::Denied
        } else {
            ExerciseResolution::NotExercise
        };
    }
    let Some(exercise) = authorize_exercise_access(
        container.exercise_instance_id,
        user.id,
        chrono::Utc::now(),
        &rows,
    ) else {
        return ExerciseResolution::Denied;
    };
    let Some(endpoint) = proxy_target(container) else {
        return ExerciseResolution::Denied;
    };
    ExerciseResolution::Granted(Box::new(InstanceAccess {
        endpoint,
        container_id: container.id,
        accessing_user_id: user.id,
        accessing_user_name: user.name.clone(),
        accessing_security_stamp: user.security_stamp.clone(),
        owner: InstanceOwner::Exercise(exercise),
    }))
}

fn authorize_exercise_access(
    linked_instance_id: Option<i32>,
    user_id: Uuid,
    now: chrono::DateTime<chrono::Utc>,
    rows: &[ExerciseAccessRow],
) -> Option<ExerciseAccess> {
    let [row] = rows else {
        return None;
    };
    if linked_instance_id.is_some_and(|id| id != row.exercise_instance_id)
        || row.user_id != user_id
        || !row.is_loaded
        || !row.is_enabled
        || row.publish_time_utc > now
    {
        return None;
    }
    Some(ExerciseAccess {
        exercise_instance_id: row.exercise_instance_id,
        exercise_id: row.exercise_id,
    })
}

#[derive(sqlx::FromRow)]
struct SharedAccessRow {
    challenge_id: i32,
    participation_id: i32,
    game_id: i32,
    team_id: i32,
}

/// A shared Jeopardy container intentionally has no `GameInstance` owner. The
/// caller must still be an accepted participant with permission to view the
/// exact challenge; the accessing participation is used as the forensic owner
/// so ordinary shared access cannot look like cross-team access.
async fn resolve_shared_instance_target(
    st: &SharedState,
    user: &CurrentUser,
    container: container::Model,
) -> Option<InstanceAccess> {
    let row = sqlx::query_as::<_, SharedAccessRow>(
        r#"SELECT challenge.id AS challenge_id,
                  participation.id AS participation_id,
                  participation.game_id,
                  participation.team_id
             FROM "GameChallenges" challenge
             JOIN "UserParticipations" membership
               ON membership.game_id = challenge.game_id
              AND membership.user_id = $2
             JOIN "Participations" participation
               ON participation.id = membership.participation_id
              AND participation.game_id = challenge.game_id
            WHERE challenge.shared_container_id = $1
              AND challenge.is_enabled = TRUE
              AND participation.status = $3
            LIMIT 1"#,
    )
    .bind(container.id)
    .bind(user.id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_optional(st.pg())
    .await
    .ok()??;
    let target_identity = game_proxy_target_identity(&container, None);
    if !game_proxy_scope_is_valid(
        st.pg(),
        LiveParticipationIdentity {
            user_id: user.id,
            expected_security_stamp: &user.security_stamp,
            game_id: row.game_id,
            team_id: row.team_id,
            participation_id: row.participation_id,
        },
        row.challenge_id,
        &target_identity,
    )
    .await
    {
        return None;
    }
    let endpoint = proxy_target(&container)?;
    Some(InstanceAccess {
        endpoint,
        container_id: container.id,
        accessing_user_id: user.id,
        accessing_user_name: user.name.clone(),
        accessing_security_stamp: user.security_stamp.clone(),
        owner: InstanceOwner::Game(GameAccess {
            game_id: row.game_id,
            accessing_team_id: row.team_id,
            challenge_id: row.challenge_id,
            owner_participation_id: row.participation_id,
            accessing_participation_id: row.participation_id,
            // Shared containers intentionally have no per-team dynamic flag row.
            egress_revision: None,
            target_identity,
            is_monitor: user.is_monitor(),
        }),
    })
}

/// Given a resolved target (or `None`), either proxy the connection or close the
/// WebSocket cleanly. Never panics.
async fn run_or_close(
    st: SharedState,
    mut socket: WebSocket,
    target: Option<ProxyTarget>,
    scan: Option<EgressScan>,
    lease: Option<InstanceLease>,
    admission: Option<crate::services::proxy_admission::ProxyPermit>,
    open_fence: Option<GameProxyOpenFence>,
) {
    let Some(target) = target else {
        if let Some(fence) = open_fence {
            fence.rollback().await;
        }
        close_cleanly(socket).await;
        return;
    };

    // Whole session is bounded; player sessions additionally lose their pump as
    // soon as live account/participation ownership is revoked.
    match target {
        ProxyTarget::Tcp(target) => {
            let stream =
                match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&target)).await {
                    Ok(Ok(stream)) => stream,
                    _ => {
                        if let Some(fence) = open_fence {
                            fence.rollback().await;
                        }
                        let _ = socket.send(endpoint_unavailable_close()).await;
                        return;
                    }
                };
            if let Some(fence) = open_fence {
                if !fence.release().await {
                    let _ = socket.send(endpoint_unavailable_close()).await;
                    return;
                }
            }
            let _ = stream.set_nodelay(true);
            let traffic = admission.as_ref().map(|permit| permit.traffic());
            let session = proxy_session(socket, stream, scan, lease, traffic);
            let _ = tokio::time::timeout(SESSION_TIMEOUT, session).await;
        }
        ProxyTarget::Worker(handle) => {
            let stream = match tokio::time::timeout(
                CONNECT_TIMEOUT,
                open_worker_stream(&st, handle),
            )
            .await
            {
                Ok(Ok(stream)) => stream,
                Ok(Err(error)) => {
                    tracing::debug!(
                        workload_id = %handle.workload_id,
                        assignment_id = %handle.assignment_id,
                        generation = handle.generation,
                        %error,
                        "trusted-worker proxy stream open failed"
                    );
                    if let Some(fence) = open_fence {
                        fence.rollback().await;
                    }
                    let _ = socket.send(endpoint_unavailable_close()).await;
                    return;
                }
                Err(_) => {
                    tracing::debug!(
                        workload_id = %handle.workload_id,
                        assignment_id = %handle.assignment_id,
                        generation = handle.generation,
                        "trusted-worker proxy stream open timed out"
                    );
                    if let Some(fence) = open_fence {
                        fence.rollback().await;
                    }
                    let _ = socket.send(endpoint_unavailable_close()).await;
                    return;
                }
            };
            if let Some(fence) = open_fence {
                if !fence.release().await {
                    let _ = socket.send(endpoint_unavailable_close()).await;
                    return;
                }
            }
            let traffic = admission.as_ref().map(|permit| permit.traffic());
            let session = proxy_session(socket, stream, scan, lease, traffic);
            let _ = tokio::time::timeout(SESSION_TIMEOUT, session).await;
        }
    }
}

async fn proxy_session<S>(
    socket: WebSocket,
    stream: S,
    scan: Option<EgressScan>,
    lease: Option<InstanceLease>,
    traffic: Option<crate::services::proxy_admission::ProxyTrafficPermit>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match lease {
        Some(lease) => {
            tokio::select! {
                _ = proxy_pump(socket, stream, scan, traffic) => {}
                _ = wait_for_revocation(lease) => {}
            }
        }
        None => proxy_pump(socket, stream, scan, traffic).await,
    }
}

async fn open_worker_stream(
    st: &SharedState,
    handle: WorkerHandle,
) -> Result<crate::services::worker::WorkerDataStream, WorkerProxyOpenError> {
    let service = st
        .workers
        .as_ref()
        .ok_or(WorkerProxyOpenError::PlaneDisabled)?;
    let workload = st
        .worker_store
        .get_workload(handle.workload_id)
        .await?
        .ok_or(WorkerProxyOpenError::WorkloadNotFound)?;
    let generation =
        u64::try_from(handle.generation).map_err(|_| WorkerProxyOpenError::InvalidGeneration)?;
    if workload.assignment_id != handle.assignment_id || workload.generation != handle.generation {
        return Err(WorkerProxyOpenError::StaleFence);
    }
    let worker_id = workload.worker_id;
    let spec: ValidatedWorkloadSpec = serde_json::from_value(workload.definition.spec)
        .map_err(WorkerProxyOpenError::InvalidSpec)?;
    let request = DataStreamRequest::TcpProxy(TcpProxyRequest {
        fence: WorkloadFence {
            workload_id: handle.workload_id,
            assignment_id: handle.assignment_id,
            generation,
        },
        service: spec.primary_endpoint.service.clone(),
        port: spec.primary_endpoint.port.clone(),
        replica: None,
    });
    service
        .open_data_stream(worker_id, request)
        .await
        .map_err(WorkerProxyOpenError::Worker)
}

#[derive(Debug, thiserror::Error)]
enum WorkerProxyOpenError {
    #[error("trusted-worker plane is disabled")]
    PlaneDisabled,
    #[error("trusted-worker workload lookup failed: {0}")]
    Store(#[from] crate::services::worker_store::WorkerStoreError),
    #[error("trusted-worker workload was not found")]
    WorkloadNotFound,
    #[error("trusted-worker workload generation is invalid")]
    InvalidGeneration,
    #[error("trusted-worker workload fence is stale")]
    StaleFence,
    #[error("trusted-worker workload specification is invalid: {0}")]
    InvalidSpec(serde_json::Error),
    #[error("trusted-worker data stream failed: {0}")]
    Worker(crate::services::worker::WorkerError),
}

#[derive(Clone)]
struct InstanceLease {
    pool: sqlx::PgPool,
    user_id: Uuid,
    security_stamp: String,
    owner: LeaseOwner,
}

#[derive(Clone, Eq, Hash, PartialEq)]
enum LeaseOwner {
    Game {
        game_id: i32,
        team_id: i32,
        participation_id: i32,
        challenge_id: i32,
        target_identity: GameProxyTargetIdentity,
        event_vpn_source: Option<Ipv4Addr>,
        bypass_event_vpn: bool,
    },
    Exercise {
        exercise_instance_id: i32,
        exercise_id: i32,
        container_id: Uuid,
    },
    Preview {
        container_id: Uuid,
    },
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct LeaseGenerationKey {
    user_id: Uuid,
    security_stamp: String,
    owner: LeaseOwner,
}

static LEASE_GENERATIONS: LazyLock<Arc<LeaseGenerationCache<LeaseGenerationKey>>> =
    LazyLock::new(LeaseGenerationCache::new);

async fn wait_for_revocation(lease: InstanceLease) {
    let jitter = Duration::from_millis(u64::from(lease.user_id.as_bytes()[0]) * 4);
    let key = LeaseGenerationKey {
        user_id: lease.user_id,
        security_stamp: lease.security_stamp.clone(),
        owner: lease.owner.clone(),
    };
    let (mut subscription, owner) = LEASE_GENERATIONS.subscribe(key);
    if let Some(owner) = owner {
        drop(tokio::spawn(owner.drive(
            Duration::from_secs(5) + jitter,
            move || {
                let lease = lease.clone();
                async move { lease_is_valid(&lease).await }
            },
        )));
    }
    subscription.invalidated().await;
}

async fn lease_is_valid(lease: &InstanceLease) -> bool {
    match &lease.owner {
        LeaseOwner::Game {
            game_id,
            team_id,
            participation_id,
            challenge_id,
            target_identity,
            event_vpn_source,
            bypass_event_vpn,
        } => {
            game_proxy_session_is_valid(
                &lease.pool,
                LiveParticipationIdentity {
                    user_id: lease.user_id,
                    expected_security_stamp: &lease.security_stamp,
                    game_id: *game_id,
                    team_id: *team_id,
                    participation_id: *participation_id,
                },
                *challenge_id,
                target_identity,
                *event_vpn_source,
                *bypass_event_vpn,
            )
            .await
        }
        LeaseOwner::Exercise {
            exercise_instance_id,
            exercise_id,
            container_id,
        } => {
            exercise_lease_is_valid(
                &lease.pool,
                lease.user_id,
                &lease.security_stamp,
                *exercise_instance_id,
                *exercise_id,
                *container_id,
            )
            .await
        }
        LeaseOwner::Preview { container_id } => {
            preview_lease_is_valid(
                &lease.pool,
                lease.user_id,
                &lease.security_stamp,
                *container_id,
            )
            .await
        }
    }
}

const PREVIEW_LEASE_SQL: &str = r#"SELECT EXISTS (
    SELECT 1
      FROM "AspNetUsers" account
      JOIN "Containers" container ON container.id = $3
     WHERE account.id = $1
       AND account.security_stamp = $2
       AND account.email_confirmed = TRUE
       AND account.role IN ($4, $5)
       AND container.is_proxy = TRUE
       AND container.game_instance_id IS NULL
       AND container.exercise_instance_id IS NULL
       AND NOT EXISTS (
           SELECT 1 FROM "ExerciseInstances" exercise
            WHERE exercise.container_id = container.id
       )
)"#;

async fn preview_lease_is_valid(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
    container_id: Uuid,
) -> bool {
    sqlx::query_scalar::<_, bool>(PREVIEW_LEASE_SQL)
        .bind(user_id)
        .bind(expected_security_stamp)
        .bind(container_id)
        .bind(Role::Admin as i16)
        .bind(Role::Monitor as i16)
        .fetch_one(pool)
        .await
        .unwrap_or(false)
}

#[cfg(test)]
const EXERCISE_LEASE_SQL: &str = r#"SELECT EXISTS (
    SELECT 1
      FROM "ExerciseInstances" instance
      JOIN "ExerciseChallenges" exercise ON exercise.id = instance.exercise_id
      JOIN "Containers" container ON container.id = instance.container_id
      JOIN "AspNetUsers" account ON account.id = instance.user_id
     WHERE instance.id = $1
       AND instance.exercise_id = $2
       AND instance.user_id = $3
       AND instance.is_loaded = TRUE
       AND instance.container_id = $4
       AND exercise.is_enabled = TRUE
       AND exercise.publish_time_utc <= clock_timestamp()
       AND container.is_proxy = TRUE
       AND container.game_instance_id IS NULL
       AND account.security_stamp = $5
       AND account.email_confirmed = TRUE
       AND account.role <> $6
       AND (
           container.exercise_instance_id IS NULL
           OR container.exercise_instance_id = instance.id
       )
)"#;
