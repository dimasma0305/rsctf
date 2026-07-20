//! hubs/container.rs — RSCTF `ContainerExecHub` (interactive container terminal)
//! over SignalR (the React `ContainerExecModal` connects to `/hub/containerExec`).
//!
//! Protocol (JSON hub protocol, mirroring the C# hub + the modal's client code):
//!   * client `POST /hub/containerExec/negotiate` → transport advertisement,
//!   * client `GET /hub/containerExec`, handshake `{"protocol":"json",…}` → `{}`,
//!   * client invokes `Open(containerGuid, shell)` → we reply (completion) with a
//!     fresh session id string,
//!   * we push container stdout/stderr to the caller's `Receive(sid, base64)`
//!     client method and signal end-of-stream via `Closed(sid, reason)`,
//!   * client invokes `Input(sid, base64)` → written to the exec's stdin,
//!     `Resize(sid, cols, rows)` → TTY resize, `Close(sid)` → tear the exec down.
//!
//! When a Docker daemon + the target container are reachable we open a real
//! `docker exec` (bollard, AttachStdin/Stdout/Stderr + Tty) and pump both ways.
//! A **self-hosted (BYOC)** service has no local container — a `byoc:<pid>:<cid>`
//! guid routes the shell over the team's agent tunnel's `'E'` stream instead
//! (see `open_byoc_exec`), reusing `services::byoc_tunnel`. When nothing is
//! reachable we degrade gracefully: `Open` still succeeds and returns a session id,
//! we send a short notice, and the terminal sits idle so xterm attaches without
//! error rather than 500-ing.

use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::Docker;
use futures::{SinkExt, Stream, StreamExt};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::hubs::signalr;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::models::data::container;
use crate::utils::codec::{base64_decode, base64_encode};
use crate::utils::enums::Role;

/// SignalR record separator (0x1E) that terminates every message.
const RS: char = '\u{1e}';
const MAX_WS_MESSAGE_BYTES: usize = 256 * 1024;
const MAX_PACKED_INVOCATIONS: usize = 64;
const MAX_SESSIONS_PER_CONNECTION: usize = 4;
const MAX_TARGET_BYTES: usize = 256;
const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_INPUT_BASE64_BYTES: usize = 4 * MAX_INPUT_BYTES.div_ceil(3);
const MAX_OUTPUT_CHUNK_BYTES: usize = 16 * 1024;
const MIN_TTY_COLS: u64 = 20;
const MAX_TTY_COLS: u64 = 500;
const MIN_TTY_ROWS: u64 = 5;
const MAX_TTY_ROWS: u64 = 200;
const EXEC_INPUT_QUEUE: usize = 8;
const EXEC_OUTPUT_QUEUE: usize = 32;
const TARGET_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(2);
const WRITER_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

mod admission;
mod scoped;
mod session;
#[cfg(test)]
mod tests;

use session::Session;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/hub/containerExec",
            limited(Policy::PrivilegedHubAdmission, get(container_hub)),
        )
        .route(
            "/hub/containerExec/negotiate",
            limited(
                Policy::PrivilegedHubAdmission,
                post(signalr::admin_negotiate),
            ),
        )
        .route(
            "/hub/containerExec/games/{game_id}",
            limited(Policy::PrivilegedHubAdmission, get(scoped_container_hub)),
        )
        .route(
            "/hub/containerExec/games/{game_id}/negotiate",
            limited(Policy::PrivilegedHubAdmission, post(scoped_negotiate)),
        )
}

/// Admin-only, mirroring the C# hub's `OnConnectedAsync` abort of non-admins.
async fn container_hub(
    ws: WebSocketUpgrade,
    State(st): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !st.config.runtime_role.capabilities().network {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "container exec route reached a non-network replica; check stateful route configuration",
        )
            .into_response();
    }
    match signalr::hub_identity(&st, &params, &headers).await {
        Some((user, token)) if user.is_admin() => {
            let Some(connection_permit) = admission::try_connection_permit(user.id) else {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            };
            ws.max_message_size(MAX_WS_MESSAGE_BYTES)
                .max_frame_size(MAX_WS_MESSAGE_BYTES)
                .on_upgrade(move |s| {
                    serve_exec(
                        s,
                        st,
                        ExecAuthorization::PlatformAdmin { token },
                        ExecScope::PlatformAdmin,
                        connection_permit,
                    )
                })
                .into_response()
        }
        Some(_) => StatusCode::FORBIDDEN.into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Negotiate a terminal constrained to one game. Authentication comes from the
/// normal HTTP extractor; the exact game membership is resolved from Postgres
/// rather than inferred from a platform role or a stale token claim.
async fn scoped_negotiate(
    State(st): State<SharedState>,
    Path(game_id): Path<i32>,
    user: CurrentUser,
) -> Response {
    if !st.config.runtime_role.capabilities().network {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match scoped::game_access(&st, &user, game_id).await {
        Ok(true) => signalr::negotiate().await.into_response(),
        Ok(false) => StatusCode::FORBIDDEN.into_response(),
        Err(error) => {
            tracing::error!(game_id, %error, "scoped container exec negotiation failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn scoped_container_hub(
    ws: WebSocketUpgrade,
    State(st): State<SharedState>,
    Path(game_id): Path<i32>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !st.config.runtime_role.capabilities().network {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "container exec route reached a non-network replica; check stateful route configuration",
        )
            .into_response();
    }
    let Some((user, token)) = signalr::hub_identity(&st, &params, &headers).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match scoped::game_access(&st, &user, game_id).await {
        Ok(true) => {
            let Some(connection_permit) = admission::try_connection_permit(user.id) else {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            };
            ws.max_message_size(MAX_WS_MESSAGE_BYTES)
                .max_frame_size(MAX_WS_MESSAGE_BYTES)
                .on_upgrade(move |socket| {
                    serve_exec(
                        socket,
                        st,
                        ExecAuthorization::Game { token, game_id },
                        ExecScope::Game(game_id),
                        connection_permit,
                    )
                })
                .into_response()
        }
        Ok(false) => StatusCode::FORBIDDEN.into_response(),
        Err(error) => {
            tracing::error!(game_id, %error, "scoped container exec upgrade failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Clone, Copy)]
enum ExecScope {
    PlatformAdmin,
    Game(i32),
}

enum ExecAuthorization {
    PlatformAdmin { token: String },
    Game { token: String, game_id: i32 },
}

impl ExecAuthorization {
    async fn is_valid(&self, st: &SharedState) -> bool {
        match self {
            Self::PlatformAdmin { token } => {
                crate::middlewares::privilege_authentication::authenticate_token(st, token)
                    .await
                    .is_ok_and(|user| user.require_role(Role::Admin).is_ok())
            }
            Self::Game { token, game_id } => {
                let Ok(user) =
                    crate::middlewares::privilege_authentication::authenticate_token(st, token)
                        .await
                else {
                    return false;
                };
                scoped::game_access(st, &user, *game_id)
                    .await
                    .unwrap_or(false)
            }
        }
    }
}

/// Bollard's boxed output stream type for an attached exec.
type ExecOutput = Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>;

/// A live exec, produced by [`open_exec`] before the pump is spawned.
struct LiveExec {
    output: ExecOutput,
    input_tx: mpsc::Sender<Vec<u8>>,
    input_task: JoinHandle<()>,
    exec_id: String,
    docker: Docker,
}

/// Drive one `/hub/containerExec` connection end to end.
async fn serve_exec(
    socket: WebSocket,
    st: SharedState,
    authorization: ExecAuthorization,
    scope: ExecScope,
    _connection_permit: admission::ConnectionPermit,
) {
    let (mut tx, mut ws_rx) = socket.split();

    // Handshake: the client's first frame is `{"protocol":"json","version":1}`.
    match ws_rx.next().await {
        Some(Ok(Message::Text(_))) => {
            if tx
                .send(Message::Text(format!("{{}}{RS}").into()))
                .await
                .is_err()
            {
                return;
            }
        }
        _ => return,
    }
    // All outgoing frames funnel through one mpsc so the connection loop and the
    // per-session pump tasks never race on the socket sink. Frames already carry
    // the trailing record separator.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(EXEC_OUTPUT_QUEUE);
    let mut writer = tokio::spawn(async move {
        while let Some(s) = out_rx.recv().await {
            if tx.send(Message::Text(s.into())).await.is_err() {
                break;
            }
        }
    });

    let mut sessions: HashMap<String, Session> = HashMap::new();
    let mut open_budget = admission::OpenBudget::new();
    let mut ping = interval(Duration::from_secs(15));
    let mut authorization_revoked = false;

    'connection: loop {
        tokio::select! {
            incoming = ws_rx.next() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    // A frame may pack several RS-separated messages.
                    if packed_invocation_count(&t).is_none() {
                        break 'connection;
                    }
                    for part in t.split(RS) {
                        let part = part.trim();
                        if part.is_empty() {
                            continue;
                        }
                        // Revalidate each packed invocation. An awaited Open may
                        // overlap a membership or stamp revocation; the next
                        // invocation in the same WebSocket frame must not inherit
                        // the earlier authorization snapshot.
                        if !authorization.is_valid(&st).await {
                            authorization_revoked = true;
                            break 'connection;
                        }
                        if let Ok(msg) = serde_json::from_str::<Value>(part) {
                            if !handle_invocation(
                                &st,
                                &authorization,
                                &out_tx,
                                &mut sessions,
                                &mut open_budget,
                                scope,
                                msg,
                            )
                            .await
                            {
                                authorization_revoked = true;
                                break 'connection;
                            }
                        }
                    }
                }
                // App-level pings are type-6 JSON frames handled above; ignore
                // any WS control frames / binary like the read-only hubs do.
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) | Some(Ok(Message::Binary(_))) => {}
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
            },
            _ = ping.tick() => {
                if !authorization.is_valid(&st).await {
                    authorization_revoked = true;
                    break;
                }
                if out_tx.try_send(format!("{{\"type\":6}}{RS}")).is_err() {
                    break;
                }
            }
        }
    }

    for (sid, sess) in sessions.drain() {
        sess.shutdown();
        if authorization_revoked {
            let _ = timeout(
                Duration::from_millis(250),
                out_tx.send(frame(&json!({
                    "type": 1,
                    "target": "Closed",
                    "arguments": [sid, "authorization revoked"],
                }))),
            )
            .await;
        }
    }
    drop(out_tx);
    if timeout(WRITER_DRAIN_TIMEOUT, &mut writer).await.is_err() {
        writer.abort();
    }
}

/// Serialize a hub message and append the record separator.
fn frame(v: &Value) -> String {
    format!("{v}{RS}")
}

/// `arguments[i]` as a string, if present.
fn arg_str(msg: &Value, i: usize) -> Option<&str> {
    msg.get("arguments")?.as_array()?.get(i)?.as_str()
}

/// `arguments[i]` as an unsigned integer, if present.
fn arg_u64(msg: &Value, i: usize) -> Option<u64> {
    msg.get("arguments")?.as_array()?.get(i)?.as_u64()
}

fn bounded_tty_dimension(value: Option<u64>, default: u64, min: u64, max: u64) -> u16 {
    value.unwrap_or(default).clamp(min, max) as u16
}

fn bounded_input(chunk: &str) -> Result<Vec<u8>, &'static str> {
    if chunk.len() > MAX_INPUT_BASE64_BYTES {
        return Err("Container input limit exceeded");
    }
    let bytes = base64_decode(chunk).ok_or("Malformed container input")?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("Container input limit exceeded");
    }
    Ok(bytes)
}

fn packed_invocation_count(text: &str) -> Option<usize> {
    let count = text
        .split(RS)
        .filter(|part| !part.trim().is_empty())
        .count();
    (count <= MAX_PACKED_INVOCATIONS).then_some(count)
}

/// Dispatch one type-1 invocation. Non-invocations (pings, completions, close)
/// need no action.
async fn handle_invocation(
    st: &SharedState,
    authorization: &ExecAuthorization,
    out_tx: &mpsc::Sender<String>,
    sessions: &mut HashMap<String, Session>,
    open_budget: &mut admission::OpenBudget,
    scope: ExecScope,
    msg: Value,
) -> bool {
    if msg.get("type").and_then(Value::as_u64) != Some(1) {
        return true;
    }
    let target = msg.get("target").and_then(Value::as_str).unwrap_or("");
    // Echoed verbatim in the completion; absent for non-blocking sends.
    let inv_id = msg.get("invocationId").cloned();

    match target {
        "Open" => {
            if !open_budget.try_take() {
                complete_error(out_tx, inv_id, "Container exec open rate limit exceeded");
                return true;
            }
            let guid = arg_str(&msg, 0);
            let shell = match arg_str(&msg, 1) {
                Some("bash") => "bash",
                _ => "sh",
            };
            if sessions.len() >= MAX_SESSIONS_PER_CONNECTION
                || guid.is_none_or(|value| value.len() > MAX_TARGET_BYTES)
            {
                complete_error(out_tx, inv_id, "Container exec connection limit exceeded");
                return true;
            }
            let Some(active_permit) = admission::try_session_permit() else {
                complete_error(out_tx, inv_id, "Container exec capacity exceeded");
                return true;
            };
            let Some((docker_live, byoc_live, welcome)) =
                open_target(st, authorization, scope, guid, shell).await
            else {
                complete_error(
                    out_tx,
                    inv_id,
                    "Target is not unambiguously owned by this game",
                );
                return true;
            };
            if !authorization.is_valid(st).await {
                discard_open(docker_live, byoc_live);
                return false;
            }
            let sid = Uuid::new_v4().simple().to_string();

            // The client only accepts frames for a session id it already knows,
            // and it learns the id from THIS completion — so the completion and
            // the welcome must be queued before any pump output. Since the queue
            // is FIFO and single-consumer, enqueue them, then spawn the pump.
            if let Some(id) = inv_id {
                if out_tx
                    .try_send(frame(&json!({
                        "type": 3, "invocationId": id, "result": sid.clone(),
                    })))
                    .is_err()
                {
                    discard_open(docker_live, byoc_live);
                    return true;
                }
            }
            if out_tx
                .try_send(frame(&json!({
                    "type": 1, "target": "Receive",
                    "arguments": [sid.clone(), base64_encode(welcome.as_bytes())],
                })))
                .is_err()
            {
                discard_open(docker_live, byoc_live);
                return true;
            }

            let session = if let Some(le) = docker_live {
                Session::docker(le, sid.clone(), out_tx.clone(), active_permit)
            } else if let Some(be) = byoc_live {
                Session::byoc(be, sid.clone(), out_tx.clone(), active_permit)
            } else {
                Session::idle(active_permit)
            };
            sessions.insert(sid, session);
        }
        "Input" => {
            if let (Some(sid), Some(chunk)) = (arg_str(&msg, 0), arg_str(&msg, 1)) {
                if let Some(sess) = sessions.get(sid) {
                    let bytes = match bounded_input(chunk) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            complete_error(out_tx, inv_id, error);
                            return true;
                        }
                    };
                    if sess.input_rejected(bytes) {
                        complete_error(out_tx, inv_id, "Container input limit exceeded");
                        return true;
                    }
                }
            }
            complete_void(out_tx, inv_id);
        }
        "Resize" => {
            if let Some(sid) = arg_str(&msg, 0) {
                // Client order is (cols, rows); bollard is (width, height).
                let cols = bounded_tty_dimension(arg_u64(&msg, 1), 80, MIN_TTY_COLS, MAX_TTY_COLS);
                let rows = bounded_tty_dimension(arg_u64(&msg, 2), 24, MIN_TTY_ROWS, MAX_TTY_ROWS);
                if let Some(sess) = sessions.get(sid) {
                    sess.resize(cols, rows);
                }
            }
            complete_void(out_tx, inv_id);
        }
        "Close" => {
            if let Some(sid) = arg_str(&msg, 0) {
                if let Some(sess) = sessions.remove(sid) {
                    sess.shutdown();
                }
            }
            complete_void(out_tx, inv_id);
        }
        _ => complete_void(out_tx, inv_id),
    }
    true
}

/// Send a void (`result`-less) completion when the invocation expected one.
fn complete_void(out_tx: &mpsc::Sender<String>, inv_id: Option<Value>) {
    if let Some(id) = inv_id {
        let _ = out_tx.try_send(frame(&json!({ "type": 3, "invocationId": id })));
    }
}

fn complete_error(out_tx: &mpsc::Sender<String>, inv_id: Option<Value>, error: &str) {
    if let Some(id) = inv_id {
        let _ = out_tx.try_send(frame(&json!({
            "type": 3, "invocationId": id, "error": error,
        })));
    }
}

fn discard_open(docker: Option<LiveExec>, byoc: Option<ByocExec>) {
    if let Some(exec) = docker {
        exec.input_task.abort();
    }
    if let Some(exec) = byoc {
        exec.input_task.abort();
    }
}

/// Authorize and resolve one `Open` target. The platform Admin branch retains
/// the historical behavior. The game branch resolves one immutable backend id
/// through [`scoped`] before Docker or the BYOC tunnel registry is touched.
async fn open_target(
    st: &SharedState,
    authorization: &ExecAuthorization,
    scope: ExecScope,
    guid: Option<&str>,
    shell: &str,
) -> Option<(Option<LiveExec>, Option<ByocExec>, String)> {
    match scope {
        ExecScope::PlatformAdmin => {
            if let Some(rest) = guid.and_then(|value| value.strip_prefix("byoc:")) {
                let (byoc, welcome) = open_byoc_exec(st, rest).await;
                Some((None, byoc, welcome))
            } else {
                let (docker, welcome) = open_exec(st, guid, shell).await;
                Some((docker, None, welcome))
            }
        }
        ExecScope::Game(game_id) => {
            let authorized = match scoped::authorize_target(st, game_id, guid).await {
                Ok(Some(target)) => target,
                Ok(None) => return None,
                Err(error) => {
                    tracing::error!(game_id, %error, "scoped container target lookup failed");
                    return None;
                }
            };
            if !authorization.is_valid(st).await {
                return None;
            }
            match authorized {
                scoped::ScopedExecTarget::Docker(container_id) => {
                    let canonical_id = match timeout(
                        TARGET_RESOLUTION_TIMEOUT,
                        st.containers.resolve_interactive_exec_target(&container_id),
                    )
                    .await
                    {
                        Ok(Ok(canonical_id)) => canonical_id,
                        Err(error) => {
                            tracing::warn!(
                                game_id,
                                %error,
                                "scoped container backend rejected an interactive exec target"
                            );
                            return None;
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(
                                game_id,
                                %error,
                                "scoped container backend rejected an interactive exec target"
                            );
                            return None;
                        }
                    };
                    let (docker, welcome) = open_resolved_exec(&canonical_id, shell).await;
                    Some((docker, None, welcome))
                }
                scoped::ScopedExecTarget::Byoc {
                    participation_id,
                    challenge_id,
                } => {
                    let (byoc, welcome) =
                        open_byoc_exec_ids(st, participation_id, challenge_id).await;
                    Some((None, byoc, welcome))
                }
            }
        }
    }
}

/// Whether `container_id` is a raw docker id owned by a game A&D team service or a
/// KotH hill — the only raw ids the admin exec hub attaches to (never an arbitrary
/// host container).
async fn is_game_container(st: &SharedState, container_id: &str) -> bool {
    use crate::models::data::{ad_team_service, koth_target};
    let ad = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::ContainerId.eq(container_id))
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .is_some();
    if ad {
        return true;
    }
    koth_target::Entity::find()
        .filter(koth_target::Column::ContainerId.eq(container_id))
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// `(None, notice)` when we degrade to a graceful idle terminal (bad guid,
/// unknown container, or no reachable daemon). Never errors — the caller always
/// hands the client a session id.
async fn open_exec(
    st: &SharedState,
    guid: Option<&str>,
    shell: &str,
) -> (Option<LiveExec>, String) {
    let idle = "[rsctf] container backend unavailable — terminal is idle\r\n".to_string();

    let Some(raw) = guid else {
        return (None, idle);
    };
    // A jeopardy/instance container is a `container`-table Uuid; an A&D team service
    // or KotH hill passes its raw docker id directly (those aren't in that table).
    let container_id = if let Ok(uuid) = Uuid::parse_str(raw) {
        match container::Entity::find_by_id(uuid).one(&st.db).await {
            Ok(Some(row)) => row.container_id,
            _ => return (None, idle),
        }
    } else if is_game_container(st, raw).await {
        raw.to_string()
    } else {
        // Not a known game container — never exec into an arbitrary host container.
        return (None, idle);
    };

    let canonical_id = match timeout(
        TARGET_RESOLUTION_TIMEOUT,
        st.containers.resolve_interactive_exec_target(&container_id),
    )
    .await
    {
        Ok(Ok(canonical_id)) => canonical_id,
        Ok(Err(_)) | Err(_) => return (None, idle),
    };

    open_resolved_exec(&canonical_id, shell).await
}

/// Open an already authorized Docker runtime id. Callers must never pass user
/// input directly: the platform path resolves it through existing Admin rules,
/// while the game path additionally verifies installation ownership through the
/// configured container backend before reaching this function.
async fn open_resolved_exec(container_id: &str, shell: &str) -> (Option<LiveExec>, String) {
    let idle = "[rsctf] container backend unavailable — terminal is idle\r\n".to_string();

    // Connect to the local daemon directly: the ContainerManager trait doesn't
    // expose a raw handle, and this mirrors RSCTF's exec channel talking to the
    // engine straight. Every call is timeout-bounded so a dead DOCKER_HOST can't
    // wedge the single-threaded read loop.
    let Ok(docker) = Docker::connect_with_local_defaults() else {
        return (None, idle);
    };
    if !matches!(
        timeout(Duration::from_secs(2), docker.ping()).await,
        Ok(Ok(_))
    ) {
        return (None, idle);
    }

    let create = docker.create_exec(
        container_id,
        CreateExecOptions::<String> {
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: Some(true),
            cmd: Some(vec![shell.to_string()]),
            ..Default::default()
        },
    );
    let exec_id = match timeout(Duration::from_secs(5), create).await {
        Ok(Ok(res)) => res.id,
        _ => return (None, idle),
    };

    let start = docker.start_exec(
        &exec_id,
        Some(StartExecOptions {
            detach: false,
            tty: true,
            output_capacity: None,
        }),
    );
    let (output, mut input) = match timeout(Duration::from_secs(5), start).await {
        Ok(Ok(StartExecResults::Attached { output, input })) => (output, input),
        _ => return (None, idle),
    };

    // stdin pump: own the write half, drain the channel, flush after each chunk
    // so keystrokes without a newline (tab-completion, Ctrl-C) reach the shell.
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(EXEC_INPUT_QUEUE);
    let input_task = tokio::spawn(async move {
        while let Some(bytes) = input_rx.recv().await {
            if input.write_all(&bytes).await.is_err() {
                break;
            }
            let _ = input.flush().await;
        }
    });

    let short = &container_id[..container_id.len().min(12)];
    let welcome = format!("[rsctf] connected to {short} ({shell})\r\n");

    (
        Some(LiveExec {
            output,
            input_tx,
            input_task,
            exec_id,
            docker,
        }),
        welcome,
    )
}

/// stdout/stderr pump: forward every exec chunk to the caller's `Receive`
/// method (base64), then signal end-of-stream via `Closed`.
fn spawn_pump(mut output: ExecOutput, sid: String, out_tx: mpsc::Sender<String>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match output.next().await {
                Some(Ok(chunk)) => {
                    let bytes = chunk.into_bytes();
                    if bytes.is_empty() {
                        continue;
                    }
                    if !send_receive_chunks(&out_tx, &sid, &bytes).await {
                        break;
                    }
                }
                Some(Err(e)) => {
                    // serde_json escapes the (possibly quote-bearing) error text.
                    let _ = out_tx
                        .send(frame(&json!({
                            "type": 1, "target": "Closed",
                            "arguments": [&sid, e.to_string()],
                        })))
                        .await;
                    break;
                }
                None => {
                    let _ = out_tx
                        .send(frame(&json!({
                            "type": 1, "target": "Closed",
                            "arguments": [&sid, "eof"],
                        })))
                        .await;
                    break;
                }
            }
        }
    })
}

/// BYOC analogue of [`LiveExec`]: the shell runs on the team's box, reached over its
/// agent tunnel's `'E'` stream. The read half is pumped to `Receive` like docker
/// output; the write half is fed by `input_tx`. `guard` keeps the tunnel fast-polling.
struct ByocExec {
    input_tx: mpsc::Sender<Vec<u8>>,
    input_task: JoinHandle<()>,
    read: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>,
    guard: crate::services::byoc_tunnel::ExecGuard,
}

/// Open a shell over a self-hosted service's agent tunnel — `rest` is `"<pid>:<cid>"`
/// (from a `byoc:` guid). `(None, notice)` when the id is malformed or the agent isn't
/// connected; never errors, like [`open_exec`].
async fn open_byoc_exec(st: &SharedState, rest: &str) -> (Option<ByocExec>, String) {
    let notice =
        "[rsctf] BYOC agent not connected — start the agent (setup.sh) and retry\r\n".to_string();
    let parsed = rest
        .split_once(':')
        .and_then(|(a, b)| Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?)));
    let Some((pid, cid)) = parsed else {
        return (None, notice);
    };
    open_byoc_exec_ids(st, pid, cid).await
}

async fn open_byoc_exec_ids(st: &SharedState, pid: i32, cid: i32) -> (Option<ByocExec>, String) {
    let notice =
        "[rsctf] BYOC agent not connected — start the agent (setup.sh) and retry\r\n".to_string();
    // Default 80x24 — the 'E' header carries the initial size and admin resize is a
    // no-op over the tunnel (a v1 limitation; the SSH bastion path opens at PTY size).
    let Ok(Some((stream, guard))) = timeout(
        Duration::from_secs(5),
        crate::services::byoc_tunnel::open_exec_stream(st, pid, cid, 80, 24),
    )
    .await
    else {
        return (None, notice);
    };
    use tokio_util::compat::{FuturesAsyncReadCompatExt, FuturesAsyncWriteCompatExt};
    let (rd, wr) = futures::AsyncReadExt::split(stream);
    let mut wr = wr.compat_write();
    let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(EXEC_INPUT_QUEUE);
    let input_task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Some(bytes) = input_rx.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                break;
            }
            let _ = wr.flush().await;
        }
    });
    let welcome = "[rsctf] connected to your BYOC service over the agent tunnel\r\n".to_string();
    (
        Some(ByocExec {
            input_tx,
            input_task,
            read: Box::pin(rd.compat()),
            guard,
        }),
        welcome,
    )
}

/// Pump a raw byte stream (a BYOC tunnel 'E' read half) to `Receive` frames — the
/// [`spawn_pump`] equivalent for an `AsyncRead` rather than a bollard exec stream.
fn spawn_pump_reader(
    mut read: std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>,
    sid: String,
    out_tx: mpsc::Sender<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 8192];
        loop {
            match read.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = out_tx
                        .send(frame(&json!({
                            "type": 1, "target": "Closed", "arguments": [&sid, "eof"],
                        })))
                        .await;
                    break;
                }
                Ok(n) => {
                    if !send_receive_chunks(&out_tx, &sid, &buf[..n]).await {
                        break;
                    }
                }
            }
        }
    })
}

async fn send_receive_chunks(out_tx: &mpsc::Sender<String>, sid: &str, bytes: &[u8]) -> bool {
    for chunk in bytes.chunks(MAX_OUTPUT_CHUNK_BYTES) {
        let message = frame(&json!({
            "type": 1, "target": "Receive",
            "arguments": [sid, base64_encode(chunk)],
        }));
        if out_tx.send(message).await.is_err() {
            return false;
        }
    }
    true
}
