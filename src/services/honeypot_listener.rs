//! services/honeypot_listener.rs — ported from RSCTF `HoneypotPortListenerService`.
//!
//! Binds raw-TCP decoy listeners (fake ssh/redis/mysql/… services) so a scanner
//! or automated tool poking a "service" port that no real challenge exposes is
//! caught: each connection sends an optional banner, reads a short probe, then
//! records a `HoneypotProtocolHit` (attributed by source IP — a TCP connect isn't
//! browser-forgeable, so the IP fallback is kept, unlike the HTTP baits).
//!
//! Ports are configured via `RSCTF_HONEYPOT_PORTS` (empty = disabled), formatted
//! `name:port[:banner]` comma-separated, e.g.
//! `ssh:2222:SSH-2.0-OpenSSH_8.9,redis:6379,mysql:3306`. Bind address defaults to
//! `0.0.0.0` (override with `RSCTF_HONEYPOT_LISTEN`). The deployment must publish
//! these container ports for the listeners to be reachable.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use crate::app_state::SharedState;

type PortConfig = (String, u16, Option<String>);

const MAX_TCP_CONNECTIONS: usize = 128;
const MAX_TCP_CONNECTIONS_PER_SOURCE: usize = 4;
const MAX_TCP_STARTS_PER_SOURCE_WINDOW: u32 = 16;
const TCP_SOURCE_WINDOW_SECONDS: u64 = 10;
const TCP_SOURCE_IDLE_TTL: StdDuration = StdDuration::from_secs(10 * 60);
const MAX_TCP_SOURCE_ENTRIES: usize = 4_096;
const TCP_CONNECTION_DEADLINE: StdDuration = StdDuration::from_secs(3);
// Bound accept/drop CPU when a saturated source keeps the kernel backlog hot.
const TCP_REJECT_BACKOFF: StdDuration = StdDuration::from_millis(10);

#[derive(Debug)]
struct SourceAdmission {
    active: usize,
    starts: u32,
    window: u64,
    last_seen: Instant,
}

#[derive(Debug)]
struct TcpAdmission {
    global: Arc<Semaphore>,
    sources: Mutex<HashMap<IpAddr, SourceAdmission>>,
    accepted: AtomicU64,
    dropped: AtomicU64,
}

impl TcpAdmission {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            global: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
            sources: Mutex::new(HashMap::new()),
            accepted: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        })
    }

    fn try_acquire(self: &Arc<Self>, source: IpAddr) -> Option<TcpPermit> {
        let global = match self.global.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        let now = Instant::now();
        let window = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / TCP_SOURCE_WINDOW_SECONDS;
        let mut sources = self
            .sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if sources.len() >= MAX_TCP_SOURCE_ENTRIES && !sources.contains_key(&source) {
            sources.retain(|_, entry| {
                entry.active > 0
                    || now.saturating_duration_since(entry.last_seen) < TCP_SOURCE_IDLE_TTL
            });
            if sources.len() >= MAX_TCP_SOURCE_ENTRIES {
                if let Some(oldest) = sources
                    .iter()
                    .filter(|(_, entry)| entry.active == 0)
                    .min_by_key(|(_, entry)| entry.last_seen)
                    .map(|(source, _)| *source)
                {
                    sources.remove(&oldest);
                } else {
                    drop(sources);
                    drop(global);
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
            }
        }
        let entry = sources.entry(source).or_insert(SourceAdmission {
            active: 0,
            starts: 0,
            window,
            last_seen: now,
        });
        if entry.window != window {
            entry.window = window;
            entry.starts = 0;
        }
        if entry.active >= MAX_TCP_CONNECTIONS_PER_SOURCE
            || entry.starts >= MAX_TCP_STARTS_PER_SOURCE_WINDOW
        {
            drop(sources);
            drop(global);
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        entry.active += 1;
        entry.starts += 1;
        entry.last_seen = now;
        drop(sources);
        self.accepted.fetch_add(1, Ordering::Relaxed);
        Some(TcpPermit {
            admission: self.clone(),
            source,
            _global: global,
        })
    }
}

struct TcpPermit {
    admission: Arc<TcpAdmission>,
    source: IpAddr,
    _global: OwnedSemaphorePermit,
}

impl Drop for TcpPermit {
    fn drop(&mut self) {
        let mut sources = self
            .admission
            .sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = sources.get_mut(&self.source) {
            entry.active = entry.active.saturating_sub(1);
            entry.last_seen = Instant::now();
        }
    }
}

/// Parse `RSCTF_HONEYPOT_PORTS` into `(name, port, banner)` triples.
fn parse_ports(raw: &str) -> Vec<PortConfig> {
    raw.split(',')
        .filter_map(|entry| {
            let mut parts = entry.trim().splitn(3, ':');
            let name = parts.next()?.trim();
            let port: u16 = parts.next()?.trim().parse().ok()?;
            if name.is_empty() || port == 0 {
                return None;
            }
            let banner = parts
                .next()
                .map(|b| b.trim().to_string())
                .filter(|b| !b.is_empty());
            Some((name.to_string(), port, banner))
        })
        .collect()
}

fn configured_ports() -> Vec<PortConfig> {
    std::env::var("RSCTF_HONEYPOT_PORTS")
        .map(|raw| parse_ports(&raw))
        .unwrap_or_default()
}

/// Launch best-effort, shutdown-aware honeypot TCP listeners.
///
/// Bind and runtime failures are logged inside each returned task and remain
/// non-fatal to the replica. The process lifecycle tracks these handles solely
/// to ensure listeners stop accepting before network ownership is released.
pub fn start(state: SharedState, shutdown: watch::Receiver<bool>) -> Vec<JoinHandle<()>> {
    let ports = configured_ports();
    if ports.is_empty() {
        return Vec::new();
    }
    let bind_addr =
        std::env::var("RSCTF_HONEYPOT_LISTEN").unwrap_or_else(|_| "0.0.0.0".to_string());
    let admission = TcpAdmission::new();

    ports
        .into_iter()
        .map(|(name, port, banner)| {
            tokio::spawn(run_listener(
                state.clone(),
                bind_addr.clone(),
                name,
                port,
                banner,
                admission.clone(),
                shutdown.clone(),
            ))
        })
        .collect()
}

async fn run_listener(
    state: SharedState,
    bind_addr: String,
    name: String,
    port: u16,
    banner: Option<String>,
    admission: Arc<TcpAdmission>,
    mut shutdown: watch::Receiver<bool>,
) {
    if *shutdown.borrow() {
        return;
    }
    let listener = tokio::select! {
        biased;
        _ = wait_for_shutdown(&mut shutdown) => return,
        result = TcpListener::bind((bind_addr.as_str(), port)) => match result {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(honeypot = %name, port, %error, "honeypot TCP bind failed");
                return;
            }
        },
    };
    tracing::info!(honeypot = %name, port, "honeypot TCP listener bound");

    let mut connections = JoinSet::new();
    loop {
        while let Some(result) = connections.try_join_next() {
            if let Err(error) = result {
                tracing::warn!(honeypot = %name, port, %error, "honeypot connection task failed");
            }
        }
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            accepted = listener.accept() => match accepted {
                Ok((socket, peer)) => {
                    if let Some(permit) = admission.try_acquire(peer.ip()) {
                        connections.spawn(handle_connection(
                            state.clone(),
                            name.clone(),
                            port,
                            banner.clone(),
                            socket,
                            peer,
                            permit,
                        ));
                    } else {
                        // Dropping the socket is cheap but an unpaced accept
                        // loop is not. Back off this listener without spawning
                        // another task; the kernel backlog supplies pressure.
                        tokio::time::sleep(TCP_REJECT_BACKOFF).await;
                    }
                }
                Err(error) => {
                    tracing::warn!(honeypot = %name, port, %error, "honeypot TCP listener stopped");
                    break;
                }
            },
            result = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = result {
                    tracing::warn!(honeypot = %name, port, %error, "honeypot connection task failed");
                }
            }
        }
    }

    connections.abort_all();
    while connections.join_next().await.is_some() {}
    tracing::info!(
        honeypot = %name,
        port,
        accepted = admission.accepted.load(Ordering::Relaxed),
        dropped = admission.dropped.load(Ordering::Relaxed),
        "honeypot TCP admission counters"
    );
}

async fn handle_connection(
    state: SharedState,
    name: String,
    port: u16,
    banner: Option<String>,
    mut socket: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
    _permit: TcpPermit,
) {
    let ip = peer.ip().to_string();
    let bait = format!("{name}:{port}");
    let telemetry = state
        .honeypot_telemetry
        .admit_source(state.config.as_ref(), &ip, &bait);
    let _ = socket.set_nodelay(true);
    let completed =
        bounded_protocol_exchange(&mut socket, banner.as_deref(), TCP_CONNECTION_DEADLINE).await;
    if !completed {
        tracing::debug!(honeypot = %name, port, "honeypot TCP probe reached its absolute deadline");
    }
    if let Some(admission) = telemetry {
        crate::services::suspicion::record_honeypot_tcp_hit(&state, &bait, admission);
    }
}

async fn bounded_protocol_exchange(
    socket: &mut tokio::net::TcpStream,
    banner: Option<&str>,
    deadline: StdDuration,
) -> bool {
    tokio::time::timeout(deadline, async {
        if let Some(banner) = banner {
            let _ = socket.write_all(banner.as_bytes()).await;
            let _ = socket.write_all(b"\r\n").await;
        }
        // Read (and discard) one bounded probe. The surrounding timeout covers
        // banner writes, read, and shutdown as one absolute socket lifetime.
        let mut buf = [0u8; 256];
        let _ = socket.read(&mut buf).await;
        let _ = socket.shutdown().await;
    })
    .await
    .is_ok()
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use std::net::{IpAddr, Ipv4Addr};

    use tokio::net::{TcpListener, TcpStream};

    use super::{
        bounded_protocol_exchange, parse_ports, wait_for_shutdown, TcpAdmission,
        MAX_TCP_CONNECTIONS, TCP_REJECT_BACKOFF,
    };

    #[test]
    fn parses_named_ports_and_optional_banners() {
        assert_eq!(
            parse_ports("ssh:2222:SSH-2.0:test, redis:6379 ,mysql:3306"),
            vec![
                ("ssh".to_string(), 2222, Some("SSH-2.0:test".to_string())),
                ("redis".to_string(), 6379, None),
                ("mysql".to_string(), 3306, None),
            ]
        );
    }

    #[test]
    fn ignores_malformed_or_unusable_ports() {
        assert_eq!(
            parse_ports("missing, :1234,zero:0,huge:65536,nonnumeric:nope,ok:8080:"),
            vec![("ok".to_string(), 8080, None)]
        );
    }

    #[tokio::test]
    async fn shutdown_waiter_observes_the_shared_signal() {
        let (shutdown_tx, mut shutdown) = tokio::sync::watch::channel(false);
        shutdown_tx.send(true).expect("receiver remains alive");
        tokio::time::timeout(Duration::from_millis(100), wait_for_shutdown(&mut shutdown))
            .await
            .expect("shutdown waiter must return promptly");
    }

    #[test]
    fn tcp_admission_bounds_global_and_per_source_tasks() {
        let admission = TcpAdmission::new();
        let source = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let permits = (0..4)
            .map(|_| {
                admission
                    .try_acquire(source)
                    .expect("source burst admitted")
            })
            .collect::<Vec<_>>();
        assert!(admission.try_acquire(source).is_none());
        assert_eq!(admission.global.available_permits(), 124);
        drop(permits);
        assert_eq!(admission.global.available_permits(), 128);
        assert_eq!(admission.accepted.load(Ordering::Relaxed), 4);
        assert_eq!(admission.dropped.load(Ordering::Relaxed), 1);
        for _ in 4..16 {
            drop(
                admission
                    .try_acquire(source)
                    .expect("bounded source-rate allowance"),
            );
        }
        assert!(admission.try_acquire(source).is_none());
    }

    #[test]
    fn tcp_admission_enforces_one_global_task_ceiling() {
        let admission = TcpAdmission::new();
        let permits = (0..MAX_TCP_CONNECTIONS)
            .map(|index| {
                let source = IpAddr::V4(Ipv4Addr::new(
                    198,
                    51,
                    (index / 250) as u8,
                    (index % 250 + 1) as u8,
                ));
                admission.try_acquire(source).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(admission.global.available_permits(), 0);
        assert!(admission
            .try_acquire(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)))
            .is_none());
        drop(permits);
        assert_eq!(admission.global.available_permits(), MAX_TCP_CONNECTIONS);
    }

    #[test]
    fn saturated_accepts_have_a_nonzero_bounded_backoff() {
        assert!(TCP_REJECT_BACKOFF >= Duration::from_millis(1));
        assert!(TCP_REJECT_BACKOFF <= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn silent_socket_is_closed_by_one_absolute_deadline() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address);
        let accepted = listener.accept();
        let (client, accepted) = tokio::join!(client, accepted);
        let _client = client.unwrap();
        let (mut server, _) = accepted.unwrap();
        let completed =
            bounded_protocol_exchange(&mut server, Some("SSH-2.0-test"), Duration::from_millis(20))
                .await;
        assert!(!completed);
    }
}
