//! hubs/attack.rs — RSCTF `AttackHub` (IAttackClient) over SignalR, plus the
//! plain-WebSocket mirror (`RSCTF.Services.AttackStreamService`) at
//! `GET /hub/attack/ws?game={id}` that the React attack-arena page connects to.
use std::collections::HashMap;
use std::net::SocketAddr;

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{interval, Duration, Instant};

use crate::app_state::SharedState;
use crate::hubs::{admission, signalr};
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::services::event_bus::EventReceiver;

const ATTACK_TARGETS: &[&str] = &["ReceivedAttack"];

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/hub/attack",
            limited(Policy::PublicHubAdmission, get(attack_hub)),
        )
        .route(
            "/hub/attack/negotiate",
            limited(Policy::PublicHubAdmission, post(signalr::negotiate)),
        )
        // Plain-WebSocket mirror of the SignalR feed for the public attack-arena
        // page (no SignalR negotiate/framing). RSCTF: AttackStreamService.
        .route(
            "/hub/attack/ws",
            limited(Policy::PublicHubAdmission, get(attack_ws)),
        )
}

async fn attack_hub(
    ws: WebSocketUpgrade,
    State(st): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let scope = match signalr::public_game_scope(&st, &params, &headers).await {
        Ok(scope) => scope,
        Err(status) => return status.into_response(),
    };
    let Some(connection_permit) = admission::try_connection_permit(
        admission::client_key(&headers, peer.ip()),
        admission::Scope::Game(scope.game_id),
    ) else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let rx = st
        .events
        .subscribe_game_targets(scope.game_id, ATTACK_TARGETS);
    signalr::bounded_upgrade(ws)
        .on_upgrade(move |s| {
            signalr::serve(
                s,
                rx,
                ATTACK_TARGETS,
                Some(scope.game_id),
                scope.authorization,
                connection_permit,
            )
        })
        .into_response()
}

/// `GET /hub/attack/ws?game={id}` — plain-WebSocket mirror of the SignalR attack
/// feed. One JSON object per text frame; the client sends nothing. Every frame
/// carries a `kind`: `"hello"` (once, on connect), `"ping"` (keepalive), or an
/// attack/koth event (same per-game broadcast the SignalR hub forwards). Same
/// public-but-not-Hidden gate as `AttackHub` (draft games are monitor-only).
async fn attack_ws(
    ws: WebSocketUpgrade,
    State(st): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let scope = match signalr::public_game_scope(&st, &params, &headers).await {
        Ok(scope) => scope,
        Err(status) => return status.into_response(),
    };
    let Some(connection_permit) = admission::try_connection_permit(
        admission::client_key(&headers, peer.ip()),
        admission::Scope::Game(scope.game_id),
    ) else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };

    let rx = st
        .events
        .subscribe_game_targets(scope.game_id, ATTACK_TARGETS);
    signalr::bounded_upgrade(ws)
        .on_upgrade(move |s| {
            serve_raw(s, rx, scope.game_id, scope.authorization, connection_permit)
        })
        .into_response()
}

/// Drive one raw-WebSocket attack-feed connection. We own the socket and split it,
/// so the write half is the sole sender (a single writer per socket — WebSocket
/// forbids concurrent sends). Greet with a `hello`, then forward this game's
/// `ReceivedAttack` broadcasts as flat JSON frames (tagged with `kind`) and emit a
/// standards-compliant control ping every 25s so a reverse proxy can't
/// idle-drop the socket and conforming clients refresh the read deadline.
async fn serve_raw(
    socket: WebSocket,
    mut rx: EventReceiver,
    game_id: i32,
    authorization: Option<signalr::HubAuthorization>,
    _connection_permit: admission::ConnectionPermit,
) {
    let (mut tx, mut ws_rx) = socket.split();

    // Greeting so a client knows it connected and what frame kinds to expect.
    let hello = format!(
        "{{\"kind\":\"hello\",\"game\":{game_id},\"events\":[\"attack\",\"koth\",\"patch\"]}}"
    );
    if tx.send(Message::Text(hello.into())).await.is_err() {
        return;
    }

    // Keepalive interval (~25s), matching AttackStreamService's idle ping cadence.
    let mut keepalive = interval(Duration::from_secs(25));
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    keepalive.tick().await; // consume the immediate first tick
    let idle = tokio::time::sleep(Duration::from_secs(90));
    tokio::pin!(idle);
    let mut inbound = signalr::InboundBudget::new();

    loop {
        tokio::select! {
            msg = ws_rx.next() => match msg {
                Some(Ok(Message::Text(value))) => {
                    if value.len() > signalr::MAX_WS_MESSAGE_BYTES {
                        let _ = admission::meter_inbound_frame(value.len(), true);
                        admission::record_protocol_rejection();
                        admission::record_close(admission::CloseReason::Protocol);
                        let _ = tx.send(signalr::too_big_close()).await;
                        break;
                    }
                    if !inbound.admit(value.len()) {
                        admission::record_close(admission::CloseReason::Quota);
                        let _ = tx.send(signalr::policy_close("inbound feed quota exceeded")).await;
                        break;
                    }
                    admission::record_protocol_rejection();
                    admission::record_close(admission::CloseReason::Protocol);
                    let _ = tx.send(Message::Close(Some(CloseFrame {
                        code: close_code::POLICY,
                        reason: "raw attack feed is read-only".into(),
                    }))).await;
                    break;
                }
                Some(Ok(Message::Binary(value))) => {
                    if value.len() > signalr::MAX_WS_MESSAGE_BYTES {
                        let _ = admission::meter_inbound_frame(value.len(), true);
                        admission::record_protocol_rejection();
                        admission::record_close(admission::CloseReason::Protocol);
                        let _ = tx.send(signalr::too_big_close()).await;
                        break;
                    }
                    if !inbound.admit(value.len()) {
                        admission::record_close(admission::CloseReason::Quota);
                        let _ = tx.send(signalr::policy_close("inbound feed quota exceeded")).await;
                        break;
                    }
                    admission::record_protocol_rejection();
                    admission::record_close(admission::CloseReason::Protocol);
                    let _ = tx.send(Message::Close(Some(CloseFrame {
                        code: close_code::POLICY,
                        reason: "raw attack feed is read-only".into(),
                    }))).await;
                    break;
                }
                Some(Ok(Message::Ping(value))) => {
                    idle.as_mut().reset(Instant::now() + Duration::from_secs(90));
                    if !inbound.admit(value.len()) {
                        admission::record_close(admission::CloseReason::Quota);
                        let _ = tx.send(signalr::policy_close("inbound feed quota exceeded")).await;
                        break;
                    }
                    if tx.send(Message::Pong(value)).await.is_err() { break; }
                }
                Some(Ok(Message::Pong(value))) => {
                    idle.as_mut().reset(Instant::now() + Duration::from_secs(90));
                    if !inbound.admit(value.len()) {
                        admission::record_close(admission::CloseReason::Quota);
                        let _ = tx.send(signalr::policy_close("inbound feed quota exceeded")).await;
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            },
            ev = rx.recv() => match ev {
                Ok(event) => {
                    // Only this game's validated attack events reach this shard.
                    let game_ok = event.game_id == Some(game_id);
                    if event.target != "ReceivedAttack" || !game_ok {
                        continue;
                    }
                    // Re-tag the payload with the raw-feed `kind` the client dispatches
                    // on. Events the arena has no handler for (e.g. round-advance
                    // scoreboard signals) are dropped rather than forwarded.
                    if let Some(frame) = tag_frame(&event.payload) {
                        if tx.send(Message::Text(frame.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(game_id, skipped, "raw attack feed lost events; forcing authoritative reconnect");
                    admission::record_close(admission::CloseReason::FeedResync);
                    break;
                }
                Err(RecvError::Closed) => break,
            },
            _ = keepalive.tick() => {
                if let Some(auth) = &authorization {
                    if !auth.is_valid().await {
                        admission::record_close(admission::CloseReason::Authorization);
                        break;
                    }
                }
                if tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            _ = &mut idle => {
                admission::record_close(admission::CloseReason::IdleTimeout);
                let _ = tx.send(Message::Close(Some(CloseFrame {
                    code: close_code::POLICY,
                    reason: "raw attack feed idle timeout".into(),
                }))).await;
                break;
            }
        }
    }
}

/// Validate a per-game `ReceivedAttack` payload for the raw client feed. Returns
/// `None` for payloads with no arena handler (round-advance signals, malformed
/// frames) so they are not forwarded.
fn tag_frame(payload: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let kind = value.as_object()?.get("kind")?.as_str()?;
    matches!(kind, "attack" | "koth" | "patch").then(|| payload.to_string())
}

#[cfg(test)]
mod tests {
    use super::tag_frame;

    #[test]
    fn raw_feed_accepts_only_current_kind_frames() {
        let attack = r#"{"kind":"attack","teamName":"red"}"#;
        assert_eq!(tag_frame(attack).as_deref(), Some(attack));
        assert!(tag_frame(r#"{"kind":"unknown"}"#).is_none());
        assert!(tag_frame(r#"{"type":"adAttack"}"#).is_none());
        assert!(tag_frame("not-json").is_none());
    }
}
