//! In-process BYOC ("bring your own container") tunnel — the server side of the
//! `rsctf-byoc-agent` process. A team's agent dials `Byoc/Agent` over a WebSocket
//! and runs a **yamux client** on it; rsctf is the yamux **server** (the agent
//! never opens streams, only accepts — and yamux keys stream-ID parity on the
//! mode, so this pairing is mandatory). For every connection a checker/attacker
//! makes to the team's service, rsctf opens a yamux stream, writes the `'S'` type
//! byte, and pipes bytes to the agent, which forwards them to the team's real
//! service. Rotating flags are pushed over an `'F'` stream (`'F'` + u64 BE seq,
//! then the flag bytes).
//!
//! This replaces RSCTF's separate relay CONTAINER with an in-process tunnel
//! (chosen divergence — see the ad-vpn-in-process-decision memory). rsctf exposes
//! each team's service on an internal TCP port on the A&D services network, so the
//! existing checker (which joins that network) reaches it with no extra hop, and
//! `ad_team_service.host:port` points at that listener.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::{Arc, OnceLock};

use axum::extract::ws::{Message, WebSocket};
use bytes::Bytes;
use futures::io::AsyncWriteExt;
use futures::{future, SinkExt, StreamExt};
use ipnet::Ipv4Net;
use sea_orm::DatabaseConnection;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

mod authorization;
mod control;
mod endpoint;
mod lifecycle;
use authorization::live_tunnel_authorized;
pub use control::start_control_listener;
use endpoint::RelayEndpoint;
use lifecycle::{activate_tunnel, deactivate_tunnel};

const STREAM_SERVICE: u8 = b'S';
const STREAM_FLAG: u8 = b'F';
/// Interactive exec-shell stream (BYOC SSH): `'E'` + u16 cols + u16 rows (BE), then
/// raw PTY bytes both ways. The agent docker-exec's a shell in its service container.
const STREAM_EXEC: u8 = b'E';

/// Per-tunnel concurrency + flow-control ceiling. yamux's default receive window
/// is 1 GiB/connection; cap it so one team flooding its own tunnel can't inflate
/// toward that (a DoS bound, not a steady-state saving — the window auto-tunes up
/// from small). Window floor is `256 KiB * max_num_streams`, so 64 → 16 MiB.
const MAX_STREAMS_PER_TUNNEL: usize = 64;
const MAX_RECV_WINDOW: usize = 16 * 1024 * 1024;
/// Concurrent SERVICE ('S') streams a tunnel will open, leaving headroom under
/// `MAX_STREAMS_PER_TUNNEL` for control streams (flag pushes 'F' + shells 'E') so a
/// service-connection flood can't consume every stream and starve them (#16).
const MAX_SERVICE_STREAMS: usize = 56;
const MAX_PENDING_OPEN_REQUESTS: usize = MAX_STREAMS_PER_TUNNEL;
const AUTHORIZATION_LEASE_SECONDS: u64 = 15;
/// A tunnel that never completed publication gets a short retry window. Once
/// published, its endpoint remains stable until authorization expires.
const FAILED_ACTIVATION_GRACE: std::time::Duration = std::time::Duration::from_secs(15);

/// A request to open a new outbound yamux stream, answered with the stream.
type OpenReq = oneshot::Sender<Result<yamux::Stream, ()>>;

/// Handle to one connected agent's yamux session — used to open `'S'`/`'F'`
/// streams. Registered per `(participation, challenge)` in [`Registry`].
/// Monotonic per-connection id so a reconnecting agent for the same
/// `(participation, challenge)` cleanly supersedes the old session and the old
/// session's teardown never clobbers the new registration.
static NEXT_CONN_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone)]
struct TunnelHandle {
    id: u64,
    open: mpsc::Sender<OpenReq>,
    /// Fired when a newer agent for the same `(pid, cid)` registers, so the old
    /// session tears down its listener + driver instead of leaking them.
    shutdown: Arc<tokio::sync::Notify>,
    /// Observes completion of the owning WebSocket task. Revocation waits for
    /// this before reporting success so old yamux streams cannot outlive a
    /// rotated capability.
    closed: tokio::sync::watch::Receiver<bool>,
    /// Latest flag pushed, so a reconnecting agent re-receives it.
    seq: Arc<std::sync::atomic::AtomicU64>,
    /// Live held-open streams ('S' service pipes + 'E' shells) — the driver
    /// fast-polls only while this is nonzero. An open shell must bump it (like a
    /// live service pipe) or keystroke I/O gets 50ms-batched.
    active: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AuthorizationGeneration {
    participation: u64,
    challenge: u64,
}

#[derive(Default)]
struct AuthorizationGenerations {
    participations: HashMap<i32, u64>,
    challenges: HashMap<i32, u64>,
}

impl AuthorizationGenerations {
    fn current(&self, pid: i32, cid: i32) -> AuthorizationGeneration {
        AuthorizationGeneration {
            participation: self.participations.get(&pid).copied().unwrap_or_default(),
            challenge: self.challenges.get(&cid).copied().unwrap_or_default(),
        }
    }
}

impl TunnelHandle {
    async fn open_stream(&self) -> Option<yamux::Stream> {
        let (tx, rx) = oneshot::channel();
        self.open.send(tx).await.ok()?;
        rx.await.ok()?.ok()
    }
}

/// Global registry of live BYOC tunnels, keyed by `(participation_id, challenge_id)`.
#[derive(Default)]
pub struct Registry {
    endpoints: Mutex<HashMap<(i32, i32), Arc<RelayEndpoint>>>,
    /// A disconnect advances the relevant generation under an exclusive guard.
    /// Activation holds a shared guard while publishing its VPN policy, so a
    /// revocation cannot return and then be followed by a stale publication.
    authorization_generations: tokio::sync::RwLock<AuthorizationGenerations>,
    events: crate::services::event_bus::EventBus,
}

impl Registry {
    pub fn new(events: crate::services::event_bus::EventBus) -> Self {
        Self {
            events,
            ..Self::default()
        }
    }

    async fn get(&self, pid: i32, cid: i32) -> Option<TunnelHandle> {
        let endpoint = self.endpoints.lock().await.get(&(pid, cid)).cloned()?;
        endpoint.current().await
    }

    async fn reserve_endpoint(
        &self,
        pid: i32,
        cid: i32,
        host: &str,
    ) -> AppResult<(Arc<RelayEndpoint>, u64, tokio::sync::OwnedSemaphorePermit)> {
        loop {
            if let Some(endpoint) = self.endpoints.lock().await.get(&(pid, cid)).cloned() {
                let activation = endpoint.try_activation().map_err(|_| {
                    AppError::unavailable("A relay activation is already pending; retry shortly")
                })?;
                if let Some(epoch) = endpoint.claim().await {
                    return Ok((endpoint, epoch, activation));
                }
                // A retiring entry is removed by its owner. Yield rather than
                // returning an endpoint whose acceptor is already stopping.
                tokio::task::yield_now().await;
                continue;
            }

            let candidate = RelayEndpoint::bind(host.to_string())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            let selected = {
                let mut endpoints = self.endpoints.lock().await;
                endpoints
                    .entry((pid, cid))
                    .or_insert_with(|| candidate.clone())
                    .clone()
            };
            if Arc::ptr_eq(&selected, &candidate) {
                // The initial epoch already represents this first claim.
                let activation = candidate.try_activation().map_err(|_| {
                    AppError::unavailable("A relay activation is already pending; retry shortly")
                })?;
                return Ok((candidate, 1, activation));
            }
            candidate.stop_acceptor();
            candidate.wait_closed().await;
            let activation = selected.try_activation().map_err(|_| {
                AppError::unavailable("A relay activation is already pending; retry shortly")
            })?;
            if let Some(epoch) = selected.claim().await {
                return Ok((selected, epoch, activation));
            }
        }
    }

    async fn contains_endpoint(&self, pid: i32, cid: i32, endpoint_id: u64) -> bool {
        self.endpoints
            .lock()
            .await
            .get(&(pid, cid))
            .is_some_and(|endpoint| endpoint.id() == endpoint_id)
    }

    async fn authorization_generation(&self, pid: i32, cid: i32) -> AuthorizationGeneration {
        self.authorization_generations
            .read()
            .await
            .current(pid, cid)
    }

    async fn publication_guard(
        &self,
        pid: i32,
        cid: i32,
        expected: AuthorizationGeneration,
    ) -> Option<tokio::sync::RwLockReadGuard<'_, AuthorizationGenerations>> {
        let guard = self.authorization_generations.read().await;
        (guard.current(pid, cid) == expected).then_some(guard)
    }

    fn schedule_failed_activation_release(
        self: &Arc<Self>,
        pid: i32,
        cid: i32,
        endpoint: Arc<RelayEndpoint>,
        idle_epoch: u64,
    ) {
        let registry = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(FAILED_ACTIVATION_GRACE).await;
            if let Some(registry) = registry.upgrade() {
                registry
                    .retire_idle_endpoint(pid, cid, endpoint, idle_epoch)
                    .await;
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn schedule_authorized_idle_release(
        self: &Arc<Self>,
        st: SharedState,
        game_id: i32,
        pid: i32,
        cid: i32,
        token: String,
        endpoint: Arc<RelayEndpoint>,
        idle_epoch: u64,
    ) {
        let registry = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(AUTHORIZATION_LEASE_SECONDS))
                    .await;
                if !endpoint.is_idle_at(idle_epoch).await {
                    return;
                }
                if live_tunnel_authorized(&st, game_id, pid, cid, &token).await {
                    continue;
                }
                if let Some(registry) = registry.upgrade() {
                    registry
                        .retire_idle_endpoint(pid, cid, endpoint, idle_epoch)
                        .await;
                }
                return;
            }
        });
    }

    async fn retire_idle_endpoint(
        &self,
        pid: i32,
        cid: i32,
        endpoint: Arc<RelayEndpoint>,
        idle_epoch: u64,
    ) {
        if !endpoint.retire_if_idle(idle_epoch).await {
            return;
        }
        {
            let mut endpoints = self.endpoints.lock().await;
            if endpoints
                .get(&(pid, cid))
                .is_some_and(|current| current.id() == endpoint.id())
            {
                endpoints.remove(&(pid, cid));
            }
        }
        // `retire_if_idle` made this endpoint permanently unattachable. Even
        // if a newer map entry already replaced it, its private acceptor must
        // still be stopped so the losing socket/task cannot leak.
        endpoint.stop_acceptor();
        endpoint.wait_closed().await;
    }

    /// Terminate every live tunnel owned by a participation. Authorization
    /// mutations call this after persisting rejection or rotating the capability
    /// secret, so an already-upgraded WebSocket cannot outlive revocation.
    pub async fn disconnect_participation(
        &self,
        db: &DatabaseConnection,
        pid: i32,
    ) -> AppResult<()> {
        self.disconnect_participation_inner(db, pid, true).await
    }

    async fn disconnect_participation_inner(
        &self,
        db: &DatabaseConnection,
        pid: i32,
        propagate: bool,
    ) -> AppResult<()> {
        let mut generations = self.authorization_generations.write().await;
        let generation = generations.participations.entry(pid).or_default();
        *generation = generation.saturating_add(1);
        let endpoints = {
            let mut endpoints = self.endpoints.lock().await;
            let keys: Vec<(i32, i32)> = endpoints
                .keys()
                .filter(|(candidate, _)| *candidate == pid)
                .copied()
                .collect();
            keys.into_iter()
                .filter_map(|key| endpoints.remove(&key))
                .collect::<Vec<_>>()
        };
        let mut handles = Vec::with_capacity(endpoints.len());
        for endpoint in &endpoints {
            if let Some(handle) = endpoint.revoke().await {
                handles.push(handle);
            }
        }
        let revocation = async {
            sqlx::query(
                r#"UPDATE "AdTeamServices" service
                  SET host = '', port = 0, status = 2
                WHERE service.participation_id = $1
                  AND service.container_id IS NULL
                  AND EXISTS (
                        SELECT 1 FROM "GameChallenges" challenge
                         WHERE challenge.id = service.challenge_id
                           AND challenge.ad_self_hosted = TRUE
                  )"#,
            )
            .bind(pid)
            .execute(db.get_postgres_connection_pool())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            crate::services::ad_vpn::ensure_hub_and_sync(db).await
        }
        .await;
        wait_for_tunnel_shutdown(&mut handles).await;
        wait_for_endpoint_shutdown(&endpoints).await;
        if propagate
            && revocation.is_ok()
            && self.events.is_distributed()
            && !crate::services::ad_vpn::owns_instance_lease()
        {
            self.events.publish(crate::app_state::HubEvent {
                target: "InternalByocRevokeParticipation",
                game_id: None,
                payload: pid.to_string(),
            });
        }
        revocation
    }

    /// Terminate every live tunnel for a challenge when it is disabled or loses
    /// approval.
    pub async fn disconnect_challenge(&self, db: &DatabaseConnection, cid: i32) -> AppResult<()> {
        self.disconnect_challenge_inner(db, cid, true).await
    }

    async fn disconnect_challenge_inner(
        &self,
        db: &DatabaseConnection,
        cid: i32,
        propagate: bool,
    ) -> AppResult<()> {
        let mut generations = self.authorization_generations.write().await;
        let generation = generations.challenges.entry(cid).or_default();
        *generation = generation.saturating_add(1);
        let endpoints = {
            let mut endpoints = self.endpoints.lock().await;
            let keys: Vec<(i32, i32)> = endpoints
                .keys()
                .filter(|(_, candidate)| *candidate == cid)
                .copied()
                .collect();
            keys.into_iter()
                .filter_map(|key| endpoints.remove(&key))
                .collect::<Vec<_>>()
        };
        let mut handles = Vec::with_capacity(endpoints.len());
        for endpoint in &endpoints {
            if let Some(handle) = endpoint.revoke().await {
                handles.push(handle);
            }
        }
        let revocation = async {
            sqlx::query(
                r#"UPDATE "AdTeamServices" service
                  SET host = '', port = 0, status = 2
                WHERE service.challenge_id = $1
                  AND service.container_id IS NULL"#,
            )
            .bind(cid)
            .execute(db.get_postgres_connection_pool())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            crate::services::ad_vpn::ensure_hub_and_sync(db).await
        }
        .await;
        wait_for_tunnel_shutdown(&mut handles).await;
        wait_for_endpoint_shutdown(&endpoints).await;
        if propagate
            && revocation.is_ok()
            && self.events.is_distributed()
            && !crate::services::ad_vpn::owns_instance_lease()
        {
            self.events.publish(crate::app_state::HubEvent {
                target: "InternalByocRevokeChallenge",
                game_id: None,
                payload: cid.to_string(),
            });
        }
        revocation
    }
}

async fn wait_for_tunnel_shutdown(handles: &mut [TunnelHandle]) {
    let closed = async {
        for handle in handles {
            if !*handle.closed.borrow() {
                let _ = handle.closed.changed().await;
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(5), closed)
        .await
        .is_err()
    {
        tracing::warn!("byoc: timed out waiting for a revoked tunnel task to exit");
    }
}

async fn wait_for_endpoint_shutdown(endpoints: &[Arc<RelayEndpoint>]) {
    let closed = async {
        for endpoint in endpoints {
            endpoint.wait_closed().await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(5), closed)
        .await
        .is_err()
    {
        tracing::warn!("byoc: timed out waiting for revoked relay listeners to exit");
    }
}

/// rsctf's own IPv4 on the A&D services network (the address the checker reaches
/// a BYOC listener at). Discovered once via a connected UDP socket toward the
/// subnet and cached — it doesn't change for the process lifetime.
fn services_ip() -> Result<String, String> {
    static CACHE: OnceLock<Result<String, String>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let cidr = crate::services::ad_vpn::services_cidr();
            let network: Ipv4Net = cidr
                .parse()
                .map_err(|error| format!("invalid A&D service CIDR {cidr}: {error}"))?;
            let probe = std::net::Ipv4Addr::from(
                u32::from(network.network())
                    .checked_add(1)
                    .ok_or_else(|| format!("A&D service CIDR {network} has no probe address"))?,
            );
            if !network.contains(&probe) {
                return Err(format!("A&D service CIDR {network} has no probe address"));
            }
            let socket = UdpSocket::bind("0.0.0.0:0")
                .map_err(|error| format!("bind service route probe: {error}"))?;
            socket
                .connect((probe, 9))
                .map_err(|error| format!("resolve service route for {network}: {error}"))?;
            let local = socket
                .local_addr()
                .map_err(|error| format!("read service route source: {error}"))?;
            let std::net::IpAddr::V4(local) = local.ip() else {
                return Err("A&D service route selected a non-IPv4 source".to_string());
            };
            if !network.contains(&local) {
                return Err(format!(
                    "A&D service route selected {local}, outside configured {network}"
                ));
            }
            Ok(local.to_string())
        })
        .clone()
}

/// Accept an agent WebSocket, run the yamux client over it, and attach it to the
/// team's stable process-local relay endpoint. Ordinary reconnects swap only
/// the yamux session; revocation or an idle-grace expiry releases the listener.
pub async fn serve_agent(
    st: SharedState,
    game_id: i32,
    pid: i32,
    cid: i32,
    token: String,
    ws: WebSocket,
) {
    // The WireGuard interface, firewall, and relay registry are protected by
    // one process-wide lease. A non-owner replica must never publish relays.
    if !crate::services::ad_vpn::owns_instance_lease() {
        tracing::warn!(
            pid,
            cid,
            "byoc: rejected on a replica without the A&D VPN lease"
        );
        return;
    }
    if !live_tunnel_authorized(&st, game_id, pid, cid, &token).await {
        return;
    }
    // Capture before doing any socket setup. A later disconnect advances this
    // generation and prevents this pending activation from reaching VPN sync.
    let authorization_generation = st.byoc.authorization_generation(pid, cid).await;
    if st.containers.backend_kind() == crate::services::container::ContainerBackendKind::Kubernetes
    {
        tracing::warn!(pid, cid, "byoc: Kubernetes relay mode is not supported");
        return;
    }

    // WS (binary frames) → a single AsyncRead+AsyncWrite for yamux.
    let (sink, stream) = ws.split();
    // Plain WS-message → byte reader. (An earlier `tokio_stream` idle-`Timeout`
    // wrapper here intermittently dropped read wakeups, stalling yamux
    // request/response round-trips for seconds — a silently-dead agent is instead
    // reaped by the connection close / supersede paths.)
    let mapped = stream.map(|m| match m {
        Ok(msg) => Ok(msg.into_data()),
        Err(e) => Err(std::io::Error::other(e)),
    });
    let reader = tokio_util::io::StreamReader::new(Box::pin(mapped));
    let writer = tokio_util::io::SinkWriter::new(tokio_util::io::CopyToBytes::new(
        sink.sink_map_err(std::io::Error::other)
            .with(|b: Bytes| future::ready(Ok::<Message, std::io::Error>(Message::Binary(b)))),
    ));
    let socket = tokio::io::join(reader, writer).compat();
    let mut cfg = yamux::Config::default();
    // Set streams before the window (the window setter validates against it).
    cfg.set_max_num_streams(MAX_STREAMS_PER_TUNNEL);
    cfg.set_max_connection_receive_window(Some(MAX_RECV_WINDOW));
    // The Go relay agent runs `yamux.Client`, so rsctf MUST be `Mode::Server` —
    // yamux keys stream-ID parity on the mode (client=odd, server=even). A
    // Client↔Client pairing collides IDs: stream OPEN + CLOSE propagate (so flag
    // pushes, which write-then-close, worked) but data on a HELD-OPEN stream never
    // delivers — which silently broke every request/response service check.
    // Verified with a standalone yamux repro: Client↔Client hangs, Server↔Client works.
    let conn = yamux::Connection::new(socket, cfg, yamux::Mode::Server);

    // Bind only on the isolated services interface. Binding 0.0.0.0 would expose
    // the relay port to peers on the app's database and proxy networks.
    let host = match services_ip() {
        Ok(host) => host,
        Err(error) => {
            tracing::warn!(%error, "byoc: could not resolve isolated service address");
            return;
        }
    };
    let (endpoint, claim_epoch, activation) = match st.byoc.reserve_endpoint(pid, cid, &host).await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::warn!(%error, "byoc: could not reserve stable service listener");
            return;
        }
    };
    let port = endpoint.port();

    let conn_id = NEXT_CONN_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let (closed_tx, closed_rx) = tokio::sync::watch::channel(false);
    let (open_tx, open_rx) = mpsc::channel::<OpenReq>(32);
    // Live held-open stream counter — shared by the handle (for 'E' shells opened by
    // the SSH bastion), the service acceptor ('S' pipes), and the driver's re-drive.
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handle = TunnelHandle {
        id: conn_id,
        open: open_tx,
        shutdown: shutdown.clone(),
        closed: closed_rx,
        seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        active: active.clone(),
    };
    if let Err(error) = activate_tunnel(
        &st,
        game_id,
        pid,
        cid,
        &token,
        authorization_generation,
        endpoint.clone(),
        handle.clone(),
    )
    .await
    {
        tracing::warn!(pid, cid, %error, "byoc: could not publish agent tunnel");
        drop(activation);
        st.byoc
            .schedule_failed_activation_release(pid, cid, endpoint, claim_epoch);
        let _ = closed_tx.send(true);
        return;
    }
    drop(activation);
    tracing::info!(pid, cid, %host, port, "byoc: agent tunnel up");

    let authorization_lease = async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(AUTHORIZATION_LEASE_SECONDS)).await;
            if !live_tunnel_authorized(&st, game_id, pid, cid, &token).await {
                break;
            }
        }
    };

    // Drive the yamux connection until it closes, a newer agent supersedes us,
    // or its live participation/game/challenge authorization expires.
    let authorization_expired = tokio::select! {
        _ = drive(conn, open_rx, active) => false,
        _ = shutdown.notified() => {
            tracing::info!(pid, cid, "byoc: agent superseded by a newer session");
            false
        }
        _ = authorization_lease => {
            tracing::info!(pid, cid, "byoc: agent authorization expired");
            true
        }
    };

    // Only the still-current connection may unpublish this stable address. A
    // superseded task has the same host/port as its replacement, so coordinates
    // alone are not a sufficient compare-and-swap fence.
    if let Some(idle_epoch) = deactivate_tunnel(&st, pid, cid, conn_id, &endpoint).await {
        tracing::info!(pid, cid, "byoc: agent tunnel down");
        if authorization_expired {
            st.byoc
                .retire_idle_endpoint(pid, cid, endpoint, idle_epoch)
                .await;
        } else {
            st.byoc.schedule_authorized_idle_release(
                st.clone(),
                game_id,
                pid,
                cid,
                token,
                endpoint,
                idle_epoch,
            );
        }
    }
    let _ = closed_tx.send(true);
}

/// Push a rotating flag to the team's agent (writes it into the service's flag
/// file). Returns whether a live tunnel took it.
pub async fn push_flag(st: &SharedState, pid: i32, cid: i32, flag: &str) -> bool {
    let Some(handle) = st.byoc.get(pid, cid).await else {
        return false;
    };
    let Some(mut stream) = handle.open_stream().await else {
        return false;
    };
    let seq = handle.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let mut hdr = [0u8; 9];
    hdr[0] = STREAM_FLAG;
    hdr[1..].copy_from_slice(&seq.to_be_bytes());
    if stream.write_all(&hdr).await.is_err() || stream.write_all(flag.as_bytes()).await.is_err() {
        return false;
    }
    let _ = stream.close().await;
    true
}

/// Held for the life of an interactive exec (`'E'`) stream so the driver keeps
/// fast-polling while a shell is open (like a live `'S'` pipe). Drops → re-drive slows.
pub struct ExecGuard(#[allow(dead_code)] ActiveGuard);

/// Open an interactive shell (`'E'`) stream to the team's agent — the agent
/// docker-exec's a shell in its service container and pipes it. Writes the header
/// `'E' + u16 cols + u16 rows` (BE); the caller then bridges the SSH channel ↔ the
/// returned stream. The [`ExecGuard`] MUST be held for the shell's lifetime (store it
/// where it drops when the SSH connection dies) so the tunnel fast-polls — otherwise
/// keystroke I/O is 50ms-batched. `None` if the team has no live tunnel.
pub async fn open_exec_stream(
    st: &SharedState,
    pid: i32,
    cid: i32,
    cols: u16,
    rows: u16,
) -> Option<(yamux::Stream, ExecGuard)> {
    let handle = st.byoc.get(pid, cid).await?;
    let mut stream = handle.open_stream().await?;
    handle
        .active
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let guard = ExecGuard(ActiveGuard(handle.active.clone()));
    let mut hdr = [0u8; 5];
    hdr[0] = STREAM_EXEC;
    hdr[1..3].copy_from_slice(&cols.to_be_bytes());
    hdr[3..5].copy_from_slice(&rows.to_be_bytes());
    if stream.write_all(&hdr).await.is_err() {
        return None; // guard drops here → active decremented
    }
    Some((stream, guard))
}

/// Decrements the active-'S'-stream counter on drop, so the driver's fast re-poll
/// turns back off even if a pipe task is aborted or panics mid-flight.
struct ActiveGuard(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The yamux client driver: owns the connection, fulfils outbound-open requests,
/// and pumps I/O via `poll_next_inbound`. Exits when the connection closes.
async fn drive(
    mut conn: yamux::Connection<impl futures::AsyncRead + futures::AsyncWrite + Unpin>,
    mut open_rx: mpsc::Receiver<OpenReq>,
    active: Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::future::Future;
    use std::task::Poll;
    let mut waiters: std::collections::VecDeque<OpenReq> = std::collections::VecDeque::new();
    // Belt-and-suspenders re-drive with an ADAPTIVE period: yamux-over-WS occasionally
    // drops a read/flush wakeup, stalling a round-trip. Re-poll every 5ms while a
    // service stream is active (held-open request/response), but slow to 50ms when
    // idle — still catches a NEW connection's first-request dropped wakeup (so an
    // idle→active transition can't stall), without a 200-wakeups/s busy poll on every
    // idle tunnel.
    let mut wake = Box::pin(tokio::time::sleep(std::time::Duration::from_millis(50)));
    future::poll_fn(|cx| {
        // Fulfil as many pending opens as yamux will allow this poll.
        while !waiters.is_empty() {
            match conn.poll_new_outbound(cx) {
                Poll::Ready(Ok(stream)) => {
                    if let Some(tx) = waiters.pop_front() {
                        let _ = tx.send(Ok(stream));
                    }
                }
                Poll::Ready(Err(_)) => {
                    if let Some(tx) = waiters.pop_front() {
                        let _ = tx.send(Err(()));
                    }
                }
                Poll::Pending => break,
            }
        }
        // Take any newly-requested opens.
        while let Poll::Ready(msg) = open_rx.poll_recv(cx) {
            match msg {
                Some(tx) if waiters.len() < MAX_PENDING_OPEN_REQUESTS => waiters.push_back(tx),
                Some(tx) => {
                    let _ = tx.send(Err(()));
                }
                None => return Poll::Ready(()), // registry dropped → shut down
            }
        }
        // Pump the connection (rsctf is the yamux server; the agent never opens
        // inbound streams, so any `Ready(Some(_))` is a protocol end).
        match conn.poll_next_inbound(cx) {
            // rsctf is the yamux server; the agent (Client) must only ACCEPT streams,
            // never open them. An inbound stream is a protocol violation — or a
            // malicious agent trying to busy-spin us by opening streams we drop and
            // immediately re-wake on. Tear the tunnel down instead of spinning (#12).
            Poll::Ready(Some(_) | None) => return Poll::Ready(()),
            Poll::Pending => {}
        }
        // Re-drive on the adaptive timer: when it fires, re-poll immediately (the
        // pump above runs again to flush any I/O whose wakeup was dropped) and re-arm
        // at 5ms while a stream is active, else 50ms.
        if wake.as_mut().poll(cx).is_ready() {
            let ms = if active.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                5
            } else {
                50
            };
            wake.as_mut()
                .reset(tokio::time::Instant::now() + std::time::Duration::from_millis(ms));
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    })
    .await;
}

/// Bidirectionally pipe a tokio TCP client and a yamux stream (futures-io).
async fn pipe(client: tokio::net::TcpStream, stream: yamux::Stream) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    use tokio_util::compat::FuturesAsyncReadCompatExt;
    let mut client = client;
    // Bridge the futures-io yamux stream to tokio-io and copy both directions with
    // `copy_bidirectional`. (An earlier version split each side with a futures
    // BiLock + two concurrent copy loops; the BiLock's cross-half wakeups
    // intermittently stalled request/response round-trips for seconds.
    // `copy_bidirectional` drives both directions in one future with no lock.)
    let mut server = stream.compat();
    server.write_all(&[STREAM_SERVICE]).await?;
    tokio::io::copy_bidirectional(&mut client, &mut server).await?;
    Ok(())
}
