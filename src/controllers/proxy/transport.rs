use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::egress::{record_flag_egress, EgressScan, RollingFlagMatcher};
use crate::services::proxy_admission::ProxyTrafficPermit;

const BUFFER_SIZE: usize = 4096;
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Accept the upgraded socket and close it cleanly when there is no valid proxy
/// target. Sending the frame before dropping the socket keeps rejection graceful.
pub(super) async fn close_cleanly(mut socket: WebSocket) {
    let _ = socket.send(normal_close()).await;
}

pub(super) fn normal_close() -> Message {
    close_message(close_code::NORMAL, "")
}

pub(super) fn endpoint_unavailable_close() -> Message {
    close_message(close_code::AGAIN, "proxy endpoint unavailable")
}

pub(super) fn transport_failure_close() -> Message {
    close_message(close_code::ERROR, "proxy transport failed")
}

pub(super) fn work_budget_close() -> Message {
    close_message(
        close_code::POLICY,
        "proxy traffic budget exceeded; retry after 2 seconds",
    )
}

fn close_message(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

fn reserve(traffic: Option<&ProxyTrafficPermit>, bytes: usize) -> bool {
    traffic.is_none_or(|traffic| traffic.try_reserve(bytes))
}

/// Pump one admitted tunnel with bounded per-session and per-process work. The
/// byte hot path uses local atomic credit rather than a Redis round trip; the
/// connection admission owner remains held by the caller until this returns.
pub(super) async fn proxy_pump<S>(
    socket: WebSocket,
    stream: S,
    scan: Option<EgressScan>,
    traffic: Option<ProxyTrafficPermit>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (mut tcp_rd, mut tcp_wr) = tokio::io::split(stream);
    let last_activity = Arc::new(AtomicU64::new(now_millis()));
    let (budget_signal, mut budget_exceeded) = tokio::sync::oneshot::channel::<()>();

    let ingress_activity = Arc::clone(&last_activity);
    let ingress_traffic = traffic.clone();
    let ws_to_tcp = async {
        let mut budget_signal = Some(budget_signal);
        let mut throttled = false;
        while let Some(Ok(msg)) = ws_rx.next().await {
            let write = match msg {
                Message::Binary(data) => {
                    if !reserve(ingress_traffic.as_ref(), data.len()) {
                        throttled = true;
                        break;
                    }
                    ingress_activity.store(now_millis(), Ordering::Release);
                    tcp_wr.write_all(&data[..]).await
                }
                Message::Text(text) => {
                    if !reserve(ingress_traffic.as_ref(), text.len()) {
                        throttled = true;
                        break;
                    }
                    ingress_activity.store(now_millis(), Ordering::Release);
                    tcp_wr.write_all(text.as_str().as_bytes()).await
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => {
                    ingress_activity.store(now_millis(), Ordering::Release);
                    continue;
                }
            };
            if write.is_err() {
                break;
            }
        }
        if throttled {
            if let Some(signal) = budget_signal.take() {
                let _ = signal.send(());
            }
            // Let the writer half deliver the stable policy close frame before
            // this direction can win the outer race and cancel it.
            std::future::pending::<()>().await;
        }
        let _ = tcp_wr.shutdown().await;
    };

    let egress_activity = Arc::clone(&last_activity);
    let egress_traffic = traffic;
    let tcp_to_ws = async {
        let mut buf = vec![0u8; BUFFER_SIZE];
        let mut egress_recorded = false;
        let mut egress_matcher = scan
            .as_ref()
            .map(|scan| RollingFlagMatcher::new(&scan.flag));
        loop {
            let read = tokio::select! {
                result = &mut budget_exceeded => {
                    if result.is_ok() {
                        let _ = ws_tx.send(work_budget_close()).await;
                        break;
                    }
                    std::future::pending::<std::io::Result<usize>>().await
                }
                read = tcp_rd.read(&mut buf) => read,
            };
            match read {
                Ok(0) => {
                    let _ = ws_tx.send(normal_close()).await;
                    break;
                }
                Err(_) => {
                    let _ = ws_tx.send(transport_failure_close()).await;
                    break;
                }
                Ok(n) => {
                    if !reserve(egress_traffic.as_ref(), n) {
                        let _ = ws_tx.send(work_budget_close()).await;
                        break;
                    }
                    egress_activity.store(now_millis(), Ordering::Release);
                    if !egress_recorded {
                        if let Some(scan) = &scan {
                            let matched = egress_matcher
                                .as_mut()
                                .is_some_and(|matcher| matcher.contains(&scan.flag, &buf[..n]));
                            if matched {
                                egress_recorded = true;
                                let scan = scan.clone();
                                tokio::spawn(async move { record_flag_egress(&scan).await });
                            }
                        }
                    }
                    if ws_tx.send(Message::from(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    };

    let idle_watch = async {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let idle_for = now_millis().saturating_sub(last_activity.load(Ordering::Acquire));
            if idle_for >= IDLE_TIMEOUT.as_millis() as u64 {
                return;
            }
        }
    };

    tokio::select! {
        _ = ws_to_tcp => {}
        _ = tcp_to_ws => {}
        _ = idle_watch => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overload_close_has_stable_retry_reason() {
        let Message::Close(Some(frame)) = work_budget_close() else {
            panic!("expected close frame");
        };
        assert_eq!(frame.code, close_code::POLICY);
        assert!(frame.reason.contains("retry after 2 seconds"));
    }
}
