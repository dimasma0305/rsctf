//! Minimal SignalR server protocol (JSON hub protocol over WebSockets), so the
//! RSCTF React client's `@microsoft/signalr` connections work against us.
//!
//! Flow: client `POST /hub/{name}/negotiate` -> we return a connection token +
//! the WebSocket transport; client opens `GET /hub/{name}?id=...`, sends the
//! handshake `{"protocol":"json","version":1}\x1e`, we reply `{}\x1e`, then we
//! stream hub invocations (`{"type":1,"target":..,"arguments":[..]}\x1e`) from
//! the `AppState` event bus and keep alive with pings (`{"type":6}`).

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::header::COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{interval, timeout, Duration, Instant};

use crate::app_state::{HubEvent, SharedState};
use crate::hubs::admission;
use crate::middlewares::privilege_authentication::{
    authenticate_token, session_cookie_value, AdminUser, CurrentUser, MonitorUser,
    MAX_SESSION_TOKEN_BYTES,
};
use crate::services::event_bus::EventReceiver;
use crate::utils::enums::Role;
use crate::utils::error::AppError;

/// SignalR record separator (0x1E) that terminates every message.
const RS: char = '\u{1e}';
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024;
const WRITE_BUFFER_BYTES: usize = 64 * 1024;
const MAX_WRITE_BUFFER_BYTES: usize = 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const CLIENT_FRAME_BURST: f64 = 32.0;
const CLIENT_FRAME_RATE: f64 = 8.0;
const CLIENT_BYTE_BURST: f64 = 64.0 * 1024.0;
const CLIENT_BYTE_RATE: f64 = 8.0 * 1024.0;
const MAX_SIGNALR_CONTROL_BYTES: usize = 512;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignalRHandshake<'a> {
    protocol: &'a str,
    version: u8,
}

#[derive(Deserialize)]
struct SignalRClientMessage {
    #[serde(rename = "type")]
    message_type: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientMessageDisposition {
    KeepAlive,
    Close,
    Unsupported,
}

pub(super) struct InboundBudget {
    frame_tokens: f64,
    byte_tokens: f64,
    updated: Instant,
}

impl InboundBudget {
    pub(super) fn new() -> Self {
        Self {
            frame_tokens: CLIENT_FRAME_BURST,
            byte_tokens: CLIENT_BYTE_BURST,
            updated: Instant::now(),
        }
    }

    pub(super) fn admit(&mut self, bytes: usize) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.updated).as_secs_f64();
        self.updated = now;
        self.frame_tokens =
            (self.frame_tokens + elapsed * CLIENT_FRAME_RATE).min(CLIENT_FRAME_BURST);
        self.byte_tokens = (self.byte_tokens + elapsed * CLIENT_BYTE_RATE).min(CLIENT_BYTE_BURST);
        let byte_cost = bytes as f64;
        let local_admitted = self.frame_tokens >= 1.0 && self.byte_tokens >= byte_cost;
        let admitted = local_admitted && admission::try_inbound_frame(bytes);
        admission::record_inbound_attempt(bytes, admitted);
        if !admitted {
            return false;
        }
        self.frame_tokens -= 1.0;
        self.byte_tokens -= byte_cost;
        true
    }
}

fn valid_handshake(text: &str) -> bool {
    let Some(payload) = text.strip_suffix(RS) else {
        return false;
    };
    if payload.len() > MAX_SIGNALR_CONTROL_BYTES || payload.contains(RS) {
        return false;
    }
    serde_json::from_str::<SignalRHandshake<'_>>(payload)
        .is_ok_and(|handshake| handshake.protocol == "json" && handshake.version == 1)
}

fn client_message(text: &str) -> ClientMessageDisposition {
    if text.len() > MAX_SIGNALR_CONTROL_BYTES || !text.ends_with(RS) {
        return ClientMessageDisposition::Unsupported;
    }
    let mut disposition = ClientMessageDisposition::KeepAlive;
    for payload in text.split_terminator(RS) {
        let Ok(message) = serde_json::from_str::<SignalRClientMessage>(payload) else {
            return ClientMessageDisposition::Unsupported;
        };
        match message.message_type {
            6 => {}
            7 => disposition = ClientMessageDisposition::Close,
            _ => return ClientMessageDisposition::Unsupported,
        }
    }
    disposition
}

fn policy_close(reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code: close_code::POLICY,
        reason: reason.into(),
    }))
}

/// Apply one conservative transport envelope to every read-only broadcast hub.
/// Clients send only the tiny SignalR handshake and keepalive frames, so larger
/// inbound messages are never legitimate. The write ceiling also prevents a
/// slow or failing peer from retaining an unbounded tungstenite buffer.
pub fn bounded_upgrade(ws: WebSocketUpgrade) -> WebSocketUpgrade {
    ws.write_buffer_size(WRITE_BUFFER_BYTES)
        .max_write_buffer_size(MAX_WRITE_BUFFER_BYTES)
        .max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
}

/// `POST /hub/{name}/negotiate` — advertise the WebSocket transport only.
pub async fn negotiate() -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    Json(serde_json::json!({
        "negotiateVersion": 1,
        "connectionId": id,
        "connectionToken": id,
        "availableTransports": [
            { "transport": "WebSockets", "transferFormats": ["Text", "Binary"] }
        ]
    }))
}

/// SignalR negotiation for organizer-only hubs. Keep the authorization
/// boundary identical across every privileged transport entry point instead
/// of advertising an admin transport to anonymous callers.
pub async fn admin_negotiate(_admin: AdminUser) -> impl IntoResponse {
    negotiate().await
}

/// SignalR negotiation for monitor/admin hubs.
pub async fn monitor_negotiate(_monitor: MonitorUser) -> impl IntoResponse {
    negotiate().await
}

/// Resolve the caller from live account state for a hub connection. SignalR
/// passes the token as `?access_token=` (or `?token=`), and browsers also send
/// the session cookie. Invalid, revoked, deleted, or banned sessions are absent.
pub fn hub_token(params: &HashMap<String, String>, headers: &HeaderMap) -> Option<String> {
    if let Some(t) = params.get("access_token").or_else(|| params.get("token")) {
        return (!t.is_empty() && t.len() <= MAX_SESSION_TOKEN_BYTES).then(|| t.clone());
    }
    let cookies = headers.get(COOKIE).and_then(|v| v.to_str().ok())?;
    session_cookie_value(cookies)
        .filter(|token| !token.is_empty() && token.len() <= MAX_SESSION_TOKEN_BYTES)
        .map(str::to_owned)
}

pub async fn hub_identity(
    st: &SharedState,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Option<(CurrentUser, String)> {
    let token = hub_token(params, headers)?;
    let user = authenticate_token(st, &token).await.ok()?;
    Some((user, token))
}

pub async fn hub_user(
    st: &SharedState,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Option<CurrentUser> {
    hub_identity(st, params, headers)
        .await
        .map(|(user, _)| user)
}

enum HubAuthorizationKind {
    Role { token: String, min_role: Role },
    PublicGame { game_id: i32 },
}

/// Live authorization lease for a hub. Privileged leases revalidate the account;
/// public-game leases revalidate visibility so hiding a game closes anonymous
/// sockets instead of leaving a stale event subscription.
pub struct HubAuthorization {
    st: SharedState,
    kind: HubAuthorizationKind,
}

impl HubAuthorization {
    pub fn new(st: SharedState, token: String, min_role: Role) -> Self {
        Self {
            st,
            kind: HubAuthorizationKind::Role { token, min_role },
        }
    }

    pub fn public_game(st: SharedState, game_id: i32) -> Self {
        Self {
            st,
            kind: HubAuthorizationKind::PublicGame { game_id },
        }
    }

    pub(crate) async fn is_valid(&self) -> bool {
        match &self.kind {
            HubAuthorizationKind::Role { token, min_role } => authenticate_token(&self.st, token)
                .await
                .is_ok_and(|user| user.require_role(*min_role).is_ok()),
            HubAuthorizationKind::PublicGame { game_id } => {
                crate::controllers::game::load_game_cached(&self.st, *game_id)
                    .await
                    .is_ok_and(|game| !game.hidden)
            }
        }
    }
}

/// Require one concrete public-hub game scope and enforce hidden-game visibility
/// against the live principal. Missing/malformed ids are 400; unknown or hidden
/// games are 404 so neither case can degrade into an all-game subscription.
pub struct PublicGameScope {
    pub game_id: i32,
    pub authorization: Option<HubAuthorization>,
}

pub async fn public_game_scope(
    st: &SharedState,
    params: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Result<PublicGameScope, StatusCode> {
    let game_id = params
        .get("game")
        .ok_or(StatusCode::BAD_REQUEST)?
        .parse::<i32>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    match crate::controllers::game::load_game_cached(st, game_id).await {
        Ok(game) if !game.hidden => Ok(PublicGameScope {
            game_id,
            authorization: Some(HubAuthorization::public_game(st.clone(), game_id)),
        }),
        Ok(_) => {
            let (user, token) = hub_identity(st, params, headers)
                .await
                .filter(|(user, _)| user.is_monitor())
                .ok_or(StatusCode::NOT_FOUND)?;
            let _ = user;
            Ok(PublicGameScope {
                game_id,
                authorization: Some(HubAuthorization::new(st.clone(), token, Role::Monitor)),
            })
        }
        Err(AppError::NotFound(_)) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Drive one SignalR connection: complete the handshake, then forward the
/// event-bus messages this hub serves — those whose `target` is in `targets` and
/// whose game matches `game_id` (a connection with no game filter sees all
/// games; a game-scoped event with no game id is broadcast to all) — invoking
/// the event's own `target`, and answer pings until the socket closes.
pub(super) async fn serve(
    socket: WebSocket,
    mut rx: EventReceiver,
    targets: &'static [&'static str],
    game_id: Option<i32>,
    authorization: Option<HubAuthorization>,
    _connection_permit: admission::ConnectionPermit,
) {
    let (mut tx, mut ws_rx) = socket.split();

    // 1) Handshake: the client's first frame is `{"protocol":"json","version":1}`.
    match timeout(HANDSHAKE_TIMEOUT, ws_rx.next()).await {
        Ok(Some(Ok(Message::Text(text)))) if valid_handshake(text.as_str()) => {
            if tx
                .send(Message::Text(format!("{{}}{RS}").into()))
                .await
                .is_err()
            {
                return;
            }
        }
        _ => {
            admission::record_protocol_rejection();
            admission::record_close(admission::CloseReason::InvalidHandshake);
            let _ = tx.send(policy_close("invalid SignalR handshake")).await;
            return;
        }
    }

    let mut ping = interval(Duration::from_secs(15));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let idle = tokio::time::sleep(READ_IDLE_TIMEOUT);
    tokio::pin!(idle);
    let mut inbound = InboundBudget::new();
    loop {
        tokio::select! {
            msg = ws_rx.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    idle.as_mut().reset(Instant::now() + READ_IDLE_TIMEOUT);
                    if !inbound.admit(text.len()) {
                        admission::record_close(admission::CloseReason::Quota);
                        let _ = tx.send(policy_close("inbound feed quota exceeded")).await;
                        break;
                    }
                    match client_message(text.as_str()) {
                        ClientMessageDisposition::KeepAlive => {}
                        ClientMessageDisposition::Close => break,
                        ClientMessageDisposition::Unsupported => {
                            admission::record_protocol_rejection();
                            admission::record_close(admission::CloseReason::Protocol);
                            let _ = tx.send(Message::Text(format!("{{\"type\":7,\"error\":\"read-only hub\"}}{RS}").into())).await;
                            let _ = tx.send(policy_close("unsupported read-only hub invocation")).await;
                            break;
                        }
                    }
                }
                Some(Ok(Message::Ping(value))) => {
                    idle.as_mut().reset(Instant::now() + READ_IDLE_TIMEOUT);
                    if !inbound.admit(value.len()) {
                        admission::record_close(admission::CloseReason::Quota);
                        break;
                    }
                    if tx.send(Message::Pong(value)).await.is_err() { break; }
                }
                Some(Ok(Message::Pong(value))) => {
                    idle.as_mut().reset(Instant::now() + READ_IDLE_TIMEOUT);
                    if !inbound.admit(value.len()) {
                        admission::record_close(admission::CloseReason::Quota);
                        break;
                    }
                }
                Some(Ok(Message::Binary(value))) => {
                    let _ = inbound.admit(value.len());
                    admission::record_protocol_rejection();
                    admission::record_close(admission::CloseReason::Protocol);
                    let _ = tx.send(policy_close("binary application frames are unsupported")).await;
                    break;
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
            },
            ev = rx.recv() => match ev {
                Ok(event) => {
                    // Only forward events this hub serves, filtered to its game.
                    if !event_matches(targets, game_id, &event) {
                        continue;
                    }
                    // The payload is a JSON value; wrap it as a hub invocation of
                    // the event's own target method.
                    let frame = format!(
                        "{{\"type\":1,\"target\":\"{}\",\"arguments\":[{}]}}{RS}",
                        event.target, event.payload
                    );
                    if tx.send(Message::Text(frame.into())).await.is_err() { break; }
                }
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "SignalR feed lost realtime events; forcing authoritative reconnect");
                    admission::record_close(admission::CloseReason::FeedResync);
                    let _ = tx.send(Message::Text(format!("{{\"type\":7,\"error\":\"feed resync required\"}}{RS}").into())).await;
                    break;
                }
                Err(RecvError::Closed) => break,
            },
            _ = ping.tick() => {
                if let Some(auth) = &authorization {
                    if !auth.is_valid().await {
                        admission::record_close(admission::CloseReason::Authorization);
                        break;
                    }
                }
                if tx.send(Message::Text(format!("{{\"type\":6}}{RS}").into())).await.is_err() { break; }
            }
            _ = &mut idle => {
                admission::record_close(admission::CloseReason::IdleTimeout);
                let _ = tx.send(policy_close("read-only feed idle timeout")).await;
                break;
            }
        }
    }
}

pub(crate) fn event_matches(
    targets: &[&str],
    connection_game_id: Option<i32>,
    event: &HubEvent,
) -> bool {
    let game_ok = match (event.game_id, connection_game_id) {
        (Some(event_game), Some(connection_game)) => event_game == connection_game,
        _ => true,
    };
    targets.contains(&event.target) && game_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            format!("RSCTF_Token={token}")
                .parse()
                .expect("valid cookie"),
        );
        headers
    }

    #[test]
    fn hub_tokens_match_the_http_session_size_boundary() {
        let headers = HeaderMap::new();
        let maximum = "a".repeat(MAX_SESSION_TOKEN_BYTES);
        let oversized = "a".repeat(MAX_SESSION_TOKEN_BYTES + 1);

        for key in ["access_token", "token"] {
            let mut params = HashMap::new();
            params.insert(key.to_string(), maximum.clone());
            assert_eq!(hub_token(&params, &headers), Some(maximum.clone()));
            params.insert(key.to_string(), oversized.clone());
            assert_eq!(hub_token(&params, &headers), None);
            params.insert(key.to_string(), String::new());
            assert_eq!(hub_token(&params, &headers), None);
        }

        assert_eq!(
            hub_token(&HashMap::new(), &cookie_headers(&maximum)),
            Some(maximum)
        );
        assert_eq!(
            hub_token(&HashMap::new(), &cookie_headers(&oversized)),
            None
        );
        assert_eq!(hub_token(&HashMap::new(), &cookie_headers("")), None);
    }

    #[test]
    fn invalid_explicit_query_token_never_falls_back_to_cookie() {
        let params = HashMap::from([("access_token".to_string(), String::new())]);
        assert_eq!(hub_token(&params, &cookie_headers("valid-cookie")), None);
    }

    #[test]
    fn monitor_event_targets_cannot_cross_game_scopes() {
        let event = HubEvent {
            target: "ReceivedGameEvent",
            game_id: Some(7),
            payload: "{}".to_owned(),
        };
        assert!(event_matches(&["ReceivedGameEvent"], Some(7), &event));
        assert!(!event_matches(&["ReceivedGameEvent"], Some(8), &event));
        assert!(!event_matches(&["ReceivedSubmissions"], Some(7), &event));
    }

    #[test]
    fn read_only_signalr_accepts_only_the_exact_json_v1_handshake() {
        assert!(valid_handshake(
            "{\"protocol\":\"json\",\"version\":1}\u{1e}"
        ));
        for invalid in [
            "{\"protocol\":\"messagepack\",\"version\":1}\u{1e}",
            "{\"protocol\":\"json\",\"version\":2}\u{1e}",
            "{\"protocol\":\"json\",\"version\":1}",
            "{}\u{1e}",
            "{\"protocol\":\"json\",\"version\":1,\"extra\":true}\u{1e}",
        ] {
            assert!(!valid_handshake(invalid), "accepted {invalid:?}");
        }
    }

    #[test]
    fn read_only_signalr_rejects_invocations_and_binary_sized_controls() {
        assert_eq!(
            client_message("{\"type\":6}\u{1e}"),
            ClientMessageDisposition::KeepAlive
        );
        assert_eq!(
            client_message("{\"type\":7}\u{1e}"),
            ClientMessageDisposition::Close
        );
        assert_eq!(
            client_message("{\"type\":1,\"target\":\"invoke\"}\u{1e}"),
            ClientMessageDisposition::Unsupported
        );
        assert_eq!(
            client_message(&format!(
                "{{\"type\":6,\"padding\":\"{}\"}}{RS}",
                "x".repeat(512)
            )),
            ClientMessageDisposition::Unsupported
        );
    }

    #[test]
    fn per_connection_inbound_budget_is_bounded_and_refillable() {
        let mut budget = InboundBudget::new();
        for _ in 0..CLIENT_FRAME_BURST as usize {
            assert!(budget.admit(1));
        }
        assert!(!budget.admit(1));
        budget.updated = Instant::now() - Duration::from_secs(1);
        assert!(budget.admit(1));
    }
}
