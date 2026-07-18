//! hubs/attack.rs — RSCTF `AttackHub` (IAttackClient) over SignalR, plus the
//! plain-WebSocket mirror (`RSCTF.Services.AttackStreamService`) at
//! `GET /hub/attack/ws?game={id}` that the React attack-arena page connects to.
use std::collections::HashMap;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::{SinkExt, StreamExt};
use tokio::sync::broadcast::{error::RecvError, Receiver};
use tokio::time::{interval, Duration};

use crate::app_state::{HubEvent, SharedState};
use crate::hubs::signalr;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/hub/attack", get(attack_hub))
        .route("/hub/attack/negotiate", post(signalr::negotiate))
        // Plain-WebSocket mirror of the SignalR feed for the public attack-arena
        // page (no SignalR negotiate/framing). RSCTF: AttackStreamService.
        .route("/hub/attack/ws", get(attack_ws))
}

async fn attack_hub(
    ws: WebSocketUpgrade,
    State(st): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let scope = match signalr::public_game_scope(&st, &params, &headers).await {
        Ok(scope) => scope,
        Err(status) => return status.into_response(),
    };
    let rx = st.events.subscribe();
    ws.on_upgrade(move |s| {
        signalr::serve(
            s,
            rx,
            &["ReceivedAttack"],
            Some(scope.game_id),
            scope.authorization,
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
    headers: HeaderMap,
) -> Response {
    let scope = match signalr::public_game_scope(&st, &params, &headers).await {
        Ok(scope) => scope,
        Err(status) => return status.into_response(),
    };

    let rx = st.events.subscribe();
    ws.on_upgrade(move |s| serve_raw(s, rx, scope.game_id, scope.authorization))
        .into_response()
}

/// Drive one raw-WebSocket attack-feed connection. We own the socket and split it,
/// so the write half is the sole sender (a single writer per socket — WebSocket
/// forbids concurrent sends). Greet with a `hello`, then forward this game's
/// `ReceivedAttack` broadcasts as flat JSON frames (tagged with `kind`) and emit a
/// keepalive `ping` every 25s so a reverse proxy can't idle-drop the socket.
async fn serve_raw(
    socket: WebSocket,
    mut rx: Receiver<HubEvent>,
    game_id: i32,
    authorization: Option<signalr::HubAuthorization>,
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
    keepalive.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            // Consume-only feed: ignore client input, observe the close handshake.
            msg = ws_rx.next() => match msg {
                Some(Ok(Message::Text(_)))
                | Some(Ok(Message::Binary(_)))
                | Some(Ok(Message::Ping(_)))
                | Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
            },
            ev = rx.recv() => match ev {
                Ok(event) => {
                    // Only this game's attack broadcasts. `game_id: None` on an event
                    // means a game-wide broadcast → deliver to every game's feed.
                    let game_ok = event.game_id.is_none_or(|eg| eg == game_id);
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
                Err(RecvError::Lagged(_)) => {} // slow reader: skip dropped frames, keep going
                Err(RecvError::Closed) => break,
            },
            _ = keepalive.tick() => {
                if let Some(auth) = &authorization {
                    if !auth.is_valid().await { break; }
                }
                if tx.send(Message::Text("{\"kind\":\"ping\"}".to_string().into())).await.is_err() {
                    break;
                }
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
