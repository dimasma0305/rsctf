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

use std::time::Duration as StdDuration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use crate::app_state::SharedState;

type PortConfig = (String, u16, Option<String>);

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

    ports
        .into_iter()
        .map(|(name, port, banner)| {
            tokio::spawn(run_listener(
                state.clone(),
                bind_addr.clone(),
                name,
                port,
                banner,
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
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            accepted = listener.accept() => match accepted {
                Ok((socket, peer)) => {
                    connections.spawn(handle_connection(
                        state.clone(),
                        name.clone(),
                        port,
                        banner.clone(),
                        socket,
                        peer,
                    ));
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
}

async fn handle_connection(
    state: SharedState,
    name: String,
    port: u16,
    banner: Option<String>,
    mut socket: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
) {
    let ip = peer.ip().to_string();
    if let Some(banner) = &banner {
        let _ = socket.write_all(banner.as_bytes()).await;
        let _ = socket.write_all(b"\r\n").await;
    }
    // Read (and discard) a short probe with a tight timeout so a
    // slow-loris connection can't pin the task.
    let mut buf = [0u8; 256];
    let _ = tokio::time::timeout(StdDuration::from_secs(3), socket.read(&mut buf)).await;
    let _ = socket.shutdown().await;

    let bait = format!("{name}:{port}");
    crate::services::suspicion::record_honeypot_tcp_hit(&state, &bait, Some(ip)).await;
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
    use std::time::Duration;

    use super::{parse_ports, wait_for_shutdown};

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
}
