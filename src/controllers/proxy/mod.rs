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
//!     Requires `AdminUser` and the container must NOT be linked to any game or
//!     exercise instance (throwaway test container only).
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

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

use futures::{SinkExt, StreamExt};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{AdminUser, CurrentUser, MaybeUser};
use crate::models::data::{
    container, game, game_instance, participation, user, user_participation,
};
use crate::services::worker::{parse_worker_handle, WorkerHandle};
use crate::utils::enums::{GamePermission, ParticipationStatus, Role};
use rsctf_worker_protocol::{
    DataStreamRequest, TcpProxyRequest, ValidatedWorkloadSpec, WorkloadFence,
};

mod egress;
#[cfg(test)]
mod tests;

use egress::{build_egress_scan, record_flag_egress, EgressScan, RollingFlagMatcher};

/// Buffer size for TCP→WebSocket reads, matching RSCTF's `BufferSize`.
const BUFFER_SIZE: usize = 4096;
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
        .route("/api/proxy/{id}", get(proxy_for_instance))
        // GET /api/proxy/noinst/{id} — proxy TCP over websocket for admin test containers.
        .route("/api/proxy/noinst/{id}", get(proxy_for_noinstance))
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
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // Resolve the target BEFORE accepting the upgrade so a rejected/absent
    // container just closes cleanly (never 500).
    let access = resolve_instance_target(&st, user, id).await;

    // Capture the connecting IP + User-Agent the same way the rest of rsctf does,
    // BEFORE the upgrade consumes the request. Used only for the access-event row
    // (best-effort forensics), never for access control.
    let remote_ip =
        crate::services::anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_default();
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|ua| ua.chars().take(512).collect::<String>()) // RSCTF caps UA at 512
        .filter(|ua| !ua.is_empty());

    let st_log = st.clone();
    ws.max_frame_size(MAX_CLIENT_MESSAGE_SIZE)
        .max_message_size(MAX_CLIENT_MESSAGE_SIZE)
        .on_upgrade(move |socket| async move {
            let (endpoint, scan, lease, admission) = match access {
                Some(a) => {
                    let (admission, scan, lease) = match &a.owner {
                        InstanceOwner::Game(game) => {
                            let Some(admission) = st_log.proxy_admission.try_acquire(
                                a.accessing_user_id,
                                game.accessing_participation_id,
                                a.container_id,
                            ) else {
                                run_or_close(st_log, socket, None, None, None, None).await;
                                return;
                            };
                            // Game access feeds game-scoped forensic evidence.
                            // Exercises deliberately skip those tables because they
                            // have no game, team, or participation identity.
                            log_container_access(&st_log, &a, game, remote_ip.clone(), user_agent)
                                .await;
                            let scan = build_egress_scan(&st_log, &a, game, remote_ip).await;
                            let lease = InstanceLease {
                                db: st_log.db.clone(),
                                pool: st_log.pg().clone(),
                                user_id: a.accessing_user_id,
                                owner: LeaseOwner::Game {
                                    game_id: game.game_id,
                                    participation_id: game.accessing_participation_id,
                                },
                            };
                            (admission, scan, lease)
                        }
                        InstanceOwner::Exercise(exercise) => {
                            let Some(admission) = st_log.proxy_admission.try_acquire_exercise(
                                a.accessing_user_id,
                                exercise.exercise_instance_id,
                                a.container_id,
                            ) else {
                                run_or_close(st_log, socket, None, None, None, None).await;
                                return;
                            };
                            let lease = InstanceLease {
                                db: st_log.db.clone(),
                                pool: st_log.pg().clone(),
                                user_id: a.accessing_user_id,
                                owner: LeaseOwner::Exercise {
                                    exercise_instance_id: exercise.exercise_instance_id,
                                    exercise_id: exercise.exercise_id,
                                    container_id: a.container_id,
                                },
                            };
                            (admission, None, lease)
                        }
                    };
                    (Some(a.endpoint), scan, Some(lease), Some(admission))
                }
                None => (None, None, None, None),
            };
            run_or_close(st_log, socket, endpoint, scan, lease, admission).await;
        })
}

/// `GET /api/proxy/noinst/{id}` — TCP-over-WebSocket proxy to an admin test
/// (NoInstance) container. `AdminUser` gates the route; the container must be a
/// proxy container that is not linked to any game or exercise instance.
async fn proxy_for_noinstance(
    State(st): State<SharedState>,
    _admin: AdminUser,
    ws: WebSocketUpgrade,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let target = resolve_noinstance_target(&st, id).await;
    ws.max_frame_size(MAX_CLIENT_MESSAGE_SIZE)
        .max_message_size(MAX_CLIENT_MESSAGE_SIZE)
        .on_upgrade(move |socket| run_or_close(st, socket, target, None, None, None))
}

/// Everything needed both to proxy a player container AND to log the access +
/// run cross-team detection on open (RSCTF `ContainerAccessContext`).
struct InstanceAccess {
    /// The reachable `ip:port` the proxy dials.
    endpoint: ProxyTarget,
    container_id: Uuid,
    accessing_user_id: Uuid,
    accessing_user_name: String,
    owner: InstanceOwner,
}

enum InstanceOwner {
    Game(GameAccess),
    Exercise(ExerciseAccess),
}

struct GameAccess {
    game_id: i32,
    challenge_id: i32,
    /// Participation that owns the container (its `GameInstance`'s team).
    owner_participation_id: i32,
    /// The accessing user's own participation in this game.
    accessing_participation_id: i32,
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
            ExerciseResolution::Granted(access) => return Some(access),
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
    let part = participation::Entity::find_by_id(instance.participation_id)
        .one(&st.db)
        .await
        .ok()??;
    if part.status != ParticipationStatus::Accepted {
        return None;
    }
    let link = user_participation::Entity::find_by_id((user.id, part.game_id))
        .one(&st.db)
        .await
        .ok()??;
    if link.participation_id != part.id {
        return None;
    }

    let endpoint = proxy_target(&container)?;
    Some(InstanceAccess {
        endpoint,
        container_id: id,
        accessing_user_id: user.id,
        accessing_user_name: user.name.clone(),
        owner: InstanceOwner::Game(GameAccess {
            game_id: part.game_id,
            challenge_id: instance.challenge_id,
            owner_participation_id: part.id,
            accessing_participation_id: link.participation_id,
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
    Granted(InstanceAccess),
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
    ExerciseResolution::Granted(InstanceAccess {
        endpoint,
        container_id: container.id,
        accessing_user_id: user.id,
        accessing_user_name: user.name.clone(),
        owner: InstanceOwner::Exercise(exercise),
    })
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
    token: String,
    writeup_id: Option<i32>,
    game_id: i32,
    team_id: i32,
    division_id: Option<i32>,
    suspicion_score: i32,
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
                  participation.token,
                  participation.writeup_id,
                  participation.game_id,
                  participation.team_id,
                  participation.division_id,
                  participation.suspicion_score
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
    let part = participation::Model {
        id: row.participation_id,
        status: ParticipationStatus::Accepted,
        token: row.token,
        writeup_id: row.writeup_id,
        game_id: row.game_id,
        team_id: row.team_id,
        division_id: row.division_id,
        suspicion_score: row.suspicion_score,
    };
    let permission = crate::controllers::game::effective_permission(st, &part, row.challenge_id)
        .await
        .ok()?;
    if !permission.contains(GamePermission::VIEW_CHALLENGE) {
        return None;
    }
    let endpoint = proxy_target(&container)?;
    Some(InstanceAccess {
        endpoint,
        container_id: container.id,
        accessing_user_id: user.id,
        accessing_user_name: user.name.clone(),
        owner: InstanceOwner::Game(GameAccess {
            game_id: row.game_id,
            challenge_id: row.challenge_id,
            owner_participation_id: row.participation_id,
            accessing_participation_id: row.participation_id,
            is_monitor: user.is_monitor(),
        }),
    })
}

/// Best-effort: persist a `ContainerAccessEvent` for this proxy open (RSCTF
/// `ContainerAccessLogger.LogAccess`) and, when the accessing participation is not
/// the container owner and the game is still live, raise
/// `CrossTeamContainerAccess` against the accessor. Any error is logged and
/// swallowed so the tunnel is never broken.
///
/// NOTE: rsctf's proxy gate ([`resolve_instance_target`]) requires the caller to
/// be registered on the OWNING participation, so `accessing == owner` always holds
/// here and the cross-team branch is currently dormant. It is kept verbatim so it
/// fires the moment the gate is relaxed to RSCTF's bearer-capability model (where
/// any holder of a container GUID can connect). The access-event row, which feeds
/// the four submission-time detectors, is always written.
async fn log_container_access(
    st: &SharedState,
    a: &InstanceAccess,
    game: &GameAccess,
    remote_ip: String,
    user_agent: Option<String>,
) {
    use crate::models::data::container_access_event;

    let row = container_access_event::ActiveModel {
        game_id: Set(game.game_id),
        challenge_id: Set(game.challenge_id),
        container_owner_participation_id: Set(game.owner_participation_id),
        container_id: Set(a.container_id),
        accessing_user_id: Set(Some(a.accessing_user_id)),
        accessing_user_name: Set(Some(a.accessing_user_name.clone())),
        accessing_participation_id: Set(Some(game.accessing_participation_id)),
        remote_ip: Set(remote_ip),
        user_agent: Set(user_agent),
        connected_at_utc: Set(chrono::Utc::now()),
        ..Default::default()
    };
    if let Err(e) = row.insert(&st.db).await {
        tracing::warn!(container = %a.container_id, error = %e, "ContainerAccessEvent persist failed");
    }

    // Cross-team access is a live-game concern: after a game ends, challenges
    // relaunch as practice through this same proxy path, so a post-game cross-team
    // open must not pin the top-weight signal on the just-ended game (gate matches
    // the sibling post-game detectors). Admins/monitors legitimately reach any
    // container.
    if game.is_monitor || game.accessing_participation_id == game.owner_participation_id {
        return;
    }
    let live = match game::Entity::find_by_id(game.game_id).one(&st.db).await {
        Ok(Some(g)) => g.end_time_utc > chrono::Utc::now(),
        _ => false,
    };
    if !live {
        return;
    }

    if let Err(e) = crate::services::suspicion::record_cross_team_container_access(
        &st.db,
        game.game_id,
        game.accessing_participation_id,
        Some(game.challenge_id),
    )
    .await
    {
        tracing::warn!(container = %a.container_id, error = %e, "CrossTeamContainerAccess raise failed");
    }
}

/// Resolve the reachable `ip:port` for an admin test container. The route is
/// already gated to `AdminUser`; here we require a proxy container that is not
/// linked to a game or exercise instance (a throwaway test container).
async fn resolve_noinstance_target(st: &SharedState, id: Uuid) -> Option<ProxyTarget> {
    let container = container::Entity::find_by_id(id).one(&st.db).await.ok()??;
    if !container.is_proxy
        || container.game_instance_id.is_some()
        || container.exercise_instance_id.is_some()
    {
        return None;
    }
    let legacy_exercise_owner = sqlx::query_scalar::<_, bool>(LEGACY_EXERCISE_OWNER_SQL)
        .bind(container.id)
        .fetch_one(st.pg())
        .await
        .ok()?;
    if legacy_exercise_owner {
        return None;
    }
    proxy_target(&container)
}

#[derive(Clone)]
enum ProxyTarget {
    Tcp(String),
    Worker(WorkerHandle),
}

fn proxy_target(container: &container::Model) -> Option<ProxyTarget> {
    if let Some(handle) = parse_worker_handle(&container.container_id) {
        return Some(ProxyTarget::Worker(handle));
    }
    target_endpoint(container).map(ProxyTarget::Tcp)
}

/// Build the `ip:port` the proxy should dial. RSCTF connects to the container's
/// `IP:Port`; game.rs stores the host-reachable published address into those
/// columns for the Docker backend. Returns `None` when the address is unusable.
fn target_endpoint(container: &container::Model) -> Option<String> {
    if container.ip.trim().is_empty() || container.port <= 0 {
        return None;
    }
    Some(format!("{}:{}", container.ip, container.port))
}

/// Given a resolved target (or `None`), either proxy the connection or close the
/// WebSocket cleanly. Never panics.
async fn run_or_close(
    st: SharedState,
    mut socket: WebSocket,
    target: Option<ProxyTarget>,
    scan: Option<EgressScan>,
    lease: Option<InstanceLease>,
    _admission: Option<crate::services::proxy_admission::ProxyPermit>,
) {
    let Some(target) = target else {
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
                        let _ = socket.send(endpoint_unavailable_close()).await;
                        return;
                    }
                };
            let _ = stream.set_nodelay(true);
            let session = proxy_session(socket, stream, scan, lease);
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
                    let _ = socket.send(endpoint_unavailable_close()).await;
                    return;
                }
            };
            let session = proxy_session(socket, stream, scan, lease);
            let _ = tokio::time::timeout(SESSION_TIMEOUT, session).await;
        }
    }
}

async fn proxy_session<S>(
    socket: WebSocket,
    stream: S,
    scan: Option<EgressScan>,
    lease: Option<InstanceLease>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match lease {
        Some(lease) => {
            tokio::select! {
                _ = pump(socket, stream, scan) => {}
                _ = wait_for_revocation(lease) => {}
            }
        }
        None => pump(socket, stream, scan).await,
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

struct InstanceLease {
    db: sea_orm::DatabaseConnection,
    pool: sqlx::PgPool,
    user_id: Uuid,
    owner: LeaseOwner,
}

#[derive(Clone, Copy)]
enum LeaseOwner {
    Game {
        game_id: i32,
        participation_id: i32,
    },
    Exercise {
        exercise_instance_id: i32,
        exercise_id: i32,
        container_id: Uuid,
    },
}

async fn wait_for_revocation(lease: InstanceLease) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    tick.tick().await;
    loop {
        tick.tick().await;
        let account_valid = user::Entity::find_by_id(lease.user_id)
            .one(&lease.db)
            .await
            .ok()
            .flatten()
            .is_some_and(|account| account.role != Role::Banned);
        let owner_valid = match lease.owner {
            LeaseOwner::Game {
                game_id,
                participation_id,
            } => {
                let participation_valid = participation::Entity::find_by_id(participation_id)
                    .one(&lease.db)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|part| {
                        part.game_id == game_id && part.status == ParticipationStatus::Accepted
                    });
                let link_valid = user_participation::Entity::find_by_id((lease.user_id, game_id))
                    .one(&lease.db)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|link| link.participation_id == participation_id);
                participation_valid && link_valid
            }
            LeaseOwner::Exercise {
                exercise_instance_id,
                exercise_id,
                container_id,
            } => {
                exercise_lease_is_valid(
                    &lease.pool,
                    lease.user_id,
                    exercise_instance_id,
                    exercise_id,
                    container_id,
                )
                .await
            }
        };
        if !account_valid || !owner_valid {
            return;
        }
    }
}

const EXERCISE_LEASE_SQL: &str = r#"SELECT EXISTS (
    SELECT 1
      FROM "ExerciseInstances" instance
      JOIN "ExerciseChallenges" exercise ON exercise.id = instance.exercise_id
      JOIN "Containers" container ON container.id = instance.container_id
     WHERE instance.id = $1
       AND instance.exercise_id = $2
       AND instance.user_id = $3
       AND instance.is_loaded = TRUE
       AND instance.container_id = $4
       AND exercise.is_enabled = TRUE
       AND exercise.publish_time_utc <= clock_timestamp()
       AND container.is_proxy = TRUE
       AND container.game_instance_id IS NULL
       AND (
           container.exercise_instance_id IS NULL
           OR container.exercise_instance_id = instance.id
       )
)"#;

async fn exercise_lease_is_valid(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    exercise_instance_id: i32,
    exercise_id: i32,
    container_id: Uuid,
) -> bool {
    sqlx::query_scalar::<_, bool>(EXERCISE_LEASE_SQL)
        .bind(exercise_instance_id)
        .bind(exercise_id)
        .bind(user_id)
        .bind(container_id)
        .fetch_one(pool)
        .await
        .unwrap_or(false)
}

/// Pump bytes bidirectionally between the WebSocket and the TCP stream until
/// either side closes. Returns when the first direction finishes; dropping the
/// other future cancels it (which drops its half of the connection).
async fn pump<S>(socket: WebSocket, stream: S, scan: Option<EgressScan>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (mut tcp_rd, mut tcp_wr) = tokio::io::split(stream);

    // WebSocket → TCP: forward Binary/Text payloads; stop on Close/error.
    // Ping/Pong are handled by axum and must NOT be forwarded into the TCP
    // stream (they are protocol control frames, not application bytes).
    let ws_to_tcp = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let write = match msg {
                Message::Binary(data) => tcp_wr.write_all(&data[..]).await,
                Message::Text(text) => tcp_wr.write_all(text.as_str().as_bytes()).await,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            if write.is_err() {
                break;
            }
        }
        // Signal EOF to the container so a half-closing client is honoured.
        let _ = tcp_wr.shutdown().await;
    };

    // TCP → WebSocket: forward reads as Binary frames; on EOF/error send a Close.
    let tcp_to_ws = async {
        let mut buf = vec![0u8; BUFFER_SIZE];
        let mut egress_recorded = false;
        let mut egress_matcher = scan
            .as_ref()
            .map(|scan| RollingFlagMatcher::new(&scan.flag));
        loop {
            match tcp_rd.read(&mut buf).await {
                Ok(0) => {
                    let _ = ws_tx.send(normal_close()).await;
                    break;
                }
                Err(_) => {
                    let _ = ws_tx.send(transport_failure_close()).await;
                    break;
                }
                Ok(n) => {
                    // Flag-egress scan (admin-feed only, never scored): if the
                    // container streams its OWN team flag out to the client, record
                    // one windowed FlagEgressEvent per session. Fire-and-forget so
                    // it never stalls the tunnel.
                    if !egress_recorded {
                        if let Some(sc) = &scan {
                            let matched = egress_matcher
                                .as_mut()
                                .is_some_and(|matcher| matcher.contains(&sc.flag, &buf[..n]));
                            if matched {
                                egress_recorded = true;
                                let sc = sc.clone();
                                tokio::spawn(async move { record_flag_egress(&sc).await });
                            }
                        }
                    }
                    // `Message::from(Vec<u8>)` yields a Binary frame without
                    // pulling in the `bytes` crate as a direct dependency.
                    if ws_tx.send(Message::from(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    };

    // Finish as soon as either direction ends; the other is cancelled on drop.
    tokio::select! {
        _ = ws_to_tcp => {}
        _ = tcp_to_ws => {}
    }
}

/// Accept the upgraded socket and close it cleanly (used when there is nothing
/// to proxy). Send a Close frame, then drop the socket.
async fn close_cleanly(mut socket: WebSocket) {
    let _ = socket.send(normal_close()).await;
}

fn normal_close() -> Message {
    close_message(close_code::NORMAL, "")
}

fn endpoint_unavailable_close() -> Message {
    close_message(close_code::AGAIN, "proxy endpoint unavailable")
}

fn transport_failure_close() -> Message {
    close_message(close_code::ERROR, "proxy transport failed")
}

fn close_message(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}
