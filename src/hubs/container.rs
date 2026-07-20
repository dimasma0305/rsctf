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
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
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
use crate::models::data::container;
use crate::utils::codec::{base64_decode, base64_encode};
use crate::utils::enums::Role;

/// SignalR record separator (0x1E) that terminates every message.
const RS: char = '\u{1e}';

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/hub/containerExec", get(container_hub))
        .route(
            "/hub/containerExec/negotiate",
            post(signalr::admin_negotiate),
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
        Some((user, token)) if user.is_admin() => ws
            .on_upgrade(move |s| serve_exec(s, st, token))
            .into_response(),
        Some(_) => StatusCode::FORBIDDEN.into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Bollard's boxed output stream type for an attached exec.
type ExecOutput = Pin<Box<dyn Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>>;

/// A live exec, produced by [`open_exec`] before the pump is spawned.
struct LiveExec {
    output: ExecOutput,
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
    input_task: JoinHandle<()>,
    exec_id: String,
    docker: Docker,
}

/// Per-session state kept by the connection loop (keyed by session id).
struct Session {
    /// stdin sink for the exec (`None` for a graceful idle session).
    input_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// exec id + docker handle, for TTY resize (`None` when idle OR BYOC — the local
    /// daemon can't resize a tunnel'd exec, so resize is a no-op there).
    exec_id: Option<String>,
    docker: Option<Docker>,
    /// background tasks to abort on close/disconnect.
    pump: Option<JoinHandle<()>>,
    input_task: Option<JoinHandle<()>>,
    /// Held for a BYOC session's life so the tunnel keeps fast-polling; dropped on
    /// shutdown (its only job — never read). `None` for docker/idle sessions.
    #[allow(dead_code)]
    byoc_guard: Option<crate::services::byoc_tunnel::ExecGuard>,
}

impl Session {
    fn idle() -> Self {
        Session {
            input_tx: None,
            exec_id: None,
            docker: None,
            pump: None,
            input_task: None,
            byoc_guard: None,
        }
    }

    /// Cancel every background task; dropping `input_tx` also closes stdin.
    fn shutdown(self) {
        if let Some(h) = self.pump {
            h.abort();
        }
        if let Some(h) = self.input_task {
            h.abort();
        }
    }
}

/// Drive one `/hub/containerExec` connection end to end.
async fn serve_exec(socket: WebSocket, st: SharedState, token: String) {
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
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(s) = out_rx.recv().await {
            if tx.send(Message::Text(s.into())).await.is_err() {
                break;
            }
        }
    });

    let mut sessions: HashMap<String, Session> = HashMap::new();
    let mut ping = interval(Duration::from_secs(15));

    loop {
        tokio::select! {
            incoming = ws_rx.next() => match incoming {
                Some(Ok(Message::Text(t))) => {
                    let still_admin = crate::middlewares::privilege_authentication::authenticate_token(
                        &st,
                        &token,
                    )
                    .await
                    .is_ok_and(|user| user.require_role(Role::Admin).is_ok());
                    if !still_admin {
                        break;
                    }
                    // A frame may pack several RS-separated messages.
                    for part in t.split(RS) {
                        let part = part.trim();
                        if part.is_empty() {
                            continue;
                        }
                        if let Ok(msg) = serde_json::from_str::<Value>(part) {
                            handle_invocation(&st, &out_tx, &mut sessions, msg).await;
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
                let still_admin = crate::middlewares::privilege_authentication::authenticate_token(
                    &st,
                    &token,
                )
                .await
                .is_ok_and(|user| user.require_role(Role::Admin).is_ok());
                if !still_admin {
                    break;
                }
                if out_tx.send(format!("{{\"type\":6}}{RS}")).is_err() {
                    break;
                }
            }
        }
    }

    for (_, sess) in sessions.drain() {
        sess.shutdown();
    }
    writer.abort();
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

/// Dispatch one type-1 invocation. Non-invocations (pings, completions, close)
/// need no action.
async fn handle_invocation(
    st: &SharedState,
    out_tx: &mpsc::UnboundedSender<String>,
    sessions: &mut HashMap<String, Session>,
    msg: Value,
) {
    if msg.get("type").and_then(Value::as_u64) != Some(1) {
        return;
    }
    let target = msg.get("target").and_then(Value::as_str).unwrap_or("");
    // Echoed verbatim in the completion; absent for non-blocking sends.
    let inv_id = msg.get("invocationId").cloned();

    match target {
        "Open" => {
            let guid = arg_str(&msg, 0);
            let shell = match arg_str(&msg, 1) {
                Some("bash") => "bash",
                _ => "sh",
            };
            let sid = Uuid::new_v4().simple().to_string();

            // A "byoc:<pid>:<cid>" guid is a self-hosted service — its container is on
            // the team's box, so route the shell over their agent tunnel's 'E' stream
            // instead of the local Docker daemon. Anything else is a raw docker id.
            let (docker_live, byoc_live, welcome) = match guid.and_then(|g| g.strip_prefix("byoc:"))
            {
                Some(rest) => {
                    let (b, w) = open_byoc_exec(st, rest).await;
                    (None, b, w)
                }
                None => {
                    let (d, w) = open_exec(st, guid, shell).await;
                    (d, None, w)
                }
            };

            // The client only accepts frames for a session id it already knows,
            // and it learns the id from THIS completion — so the completion and
            // the welcome must be queued before any pump output. Since the queue
            // is FIFO and single-consumer, enqueue them, then spawn the pump.
            if let Some(id) = inv_id {
                let _ = out_tx.send(frame(&json!({
                    "type": 3, "invocationId": id, "result": sid.clone(),
                })));
            }
            let _ = out_tx.send(frame(&json!({
                "type": 1, "target": "Receive",
                "arguments": [sid.clone(), base64_encode(welcome.as_bytes())],
            })));

            let session = if let Some(le) = docker_live {
                Session {
                    input_tx: Some(le.input_tx),
                    exec_id: Some(le.exec_id),
                    docker: Some(le.docker),
                    pump: Some(spawn_pump(le.output, sid.clone(), out_tx.clone())),
                    input_task: Some(le.input_task),
                    byoc_guard: None,
                }
            } else if let Some(be) = byoc_live {
                Session {
                    input_tx: Some(be.input_tx),
                    exec_id: None,
                    docker: None,
                    pump: Some(spawn_pump_reader(be.read, sid.clone(), out_tx.clone())),
                    input_task: Some(be.input_task),
                    byoc_guard: Some(be.guard),
                }
            } else {
                Session::idle()
            };
            sessions.insert(sid, session);
        }
        "Input" => {
            if let (Some(sid), Some(chunk)) = (arg_str(&msg, 0), arg_str(&msg, 1)) {
                if let Some(sess) = sessions.get(sid) {
                    if let (Some(tx), Some(bytes)) = (&sess.input_tx, base64_decode(chunk)) {
                        let _ = tx.send(bytes);
                    }
                }
            }
            complete_void(out_tx, inv_id);
        }
        "Resize" => {
            if let Some(sid) = arg_str(&msg, 0) {
                // Client order is (cols, rows); bollard is (width, height).
                let cols = arg_u64(&msg, 1).unwrap_or(80) as u16;
                let rows = arg_u64(&msg, 2).unwrap_or(24) as u16;
                if let Some(sess) = sessions.get(sid) {
                    if let (Some(docker), Some(exec_id)) = (&sess.docker, &sess.exec_id) {
                        let docker = docker.clone();
                        let exec_id = exec_id.clone();
                        // Off the read loop so a slow daemon can't stall input.
                        tokio::spawn(async move {
                            let _ = docker
                                .resize_exec(
                                    &exec_id,
                                    ResizeExecOptions {
                                        width: cols,
                                        height: rows,
                                    },
                                )
                                .await;
                        });
                    }
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
}

/// Send a void (`result`-less) completion when the invocation expected one.
fn complete_void(out_tx: &mpsc::UnboundedSender<String>, inv_id: Option<Value>) {
    if let Some(id) = inv_id {
        let _ = out_tx.send(frame(&json!({ "type": 3, "invocationId": id })));
    }
}

/// Resolve the container, connect to Docker, and open an attached exec.
///
/// Returns `(Some(live), welcome)` when a real exec was started, or
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
        &container_id,
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
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
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
fn spawn_pump(
    mut output: ExecOutput,
    sid: String,
    out_tx: mpsc::UnboundedSender<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match output.next().await {
                Some(Ok(chunk)) => {
                    let bytes = chunk.into_bytes();
                    if bytes.is_empty() {
                        continue;
                    }
                    let f = frame(&json!({
                        "type": 1, "target": "Receive",
                        "arguments": [&sid, base64_encode(&bytes)],
                    }));
                    if out_tx.send(f).is_err() {
                        break;
                    }
                }
                Some(Err(e)) => {
                    // serde_json escapes the (possibly quote-bearing) error text.
                    let _ = out_tx.send(frame(&json!({
                        "type": 1, "target": "Closed",
                        "arguments": [&sid, e.to_string()],
                    })));
                    break;
                }
                None => {
                    let _ = out_tx.send(frame(&json!({
                        "type": 1, "target": "Closed",
                        "arguments": [&sid, "eof"],
                    })));
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
    input_tx: mpsc::UnboundedSender<Vec<u8>>,
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
    // Default 80x24 — the 'E' header carries the initial size and admin resize is a
    // no-op over the tunnel (a v1 limitation; the SSH bastion path opens at PTY size).
    let Some((stream, guard)) =
        crate::services::byoc_tunnel::open_exec_stream(st, pid, cid, 80, 24).await
    else {
        return (None, notice);
    };
    use tokio_util::compat::{FuturesAsyncReadCompatExt, FuturesAsyncWriteCompatExt};
    let (rd, wr) = futures::AsyncReadExt::split(stream);
    let mut wr = wr.compat_write();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
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
    out_tx: mpsc::UnboundedSender<String>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 8192];
        loop {
            match read.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = out_tx.send(frame(&json!({
                        "type": 1, "target": "Closed", "arguments": [&sid, "eof"],
                    })));
                    break;
                }
                Ok(n) => {
                    let f = frame(&json!({
                        "type": 1, "target": "Receive",
                        "arguments": [&sid, base64_encode(&buf[..n])],
                    }));
                    if out_tx.send(f).is_err() {
                        break;
                    }
                }
            }
        }
    })
}
