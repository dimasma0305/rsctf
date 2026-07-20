//! Minimal SignalR server protocol (JSON hub protocol over WebSockets), so the
//! RSCTF React client's `@microsoft/signalr` connections work against us.
//!
//! Flow: client `POST /hub/{name}/negotiate` -> we return a connection token +
//! the WebSocket transport; client opens `GET /hub/{name}?id=...`, sends the
//! handshake `{"protocol":"json","version":1}\x1e`, we reply `{}\x1e`, then we
//! stream hub invocations (`{"type":1,"target":..,"arguments":[..]}\x1e`) from
//! the `AppState` event bus and keep alive with pings (`{"type":6}`).

use axum::extract::ws::{Message, WebSocket};
use axum::http::header::COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use futures::{SinkExt, StreamExt};
use sea_orm::EntityTrait;
use std::collections::HashMap;
use tokio::sync::broadcast::{error::RecvError, Receiver};
use tokio::time::{interval, Duration};

use crate::app_state::{HubEvent, SharedState};
use crate::middlewares::privilege_authentication::{
    authenticate_token, AdminUser, CurrentUser, SESSION_COOKIE,
};
use crate::models::data::game;
use crate::utils::enums::Role;

/// SignalR record separator (0x1E) that terminates every message.
const RS: char = '\u{1e}';

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

/// Resolve the caller from live account state for a hub connection. SignalR
/// passes the token as `?access_token=` (or `?token=`), and browsers also send
/// the session cookie. Invalid, revoked, deleted, or banned sessions are absent.
pub fn hub_token(params: &HashMap<String, String>, headers: &HeaderMap) -> Option<String> {
    if let Some(t) = params.get("access_token").or_else(|| params.get("token")) {
        return Some(t.clone());
    }
    let cookies = headers.get(COOKIE).and_then(|v| v.to_str().ok())?;
    for pair in cookies.split(';') {
        if let Some(v) = pair.trim().strip_prefix(&format!("{SESSION_COOKIE}=")) {
            return Some(v.to_string());
        }
    }
    None
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
            HubAuthorizationKind::PublicGame { game_id } => game::Entity::find_by_id(*game_id)
                .one(&self.st.db)
                .await
                .is_ok_and(|game| game.is_some_and(|game| !game.hidden)),
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
    match game::Entity::find_by_id(game_id).one(&st.db).await {
        Ok(Some(game)) if !game.hidden => Ok(PublicGameScope {
            game_id,
            authorization: Some(HubAuthorization::public_game(st.clone(), game_id)),
        }),
        Ok(Some(_)) => {
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
        Ok(_) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

/// Drive one SignalR connection: complete the handshake, then forward the
/// event-bus messages this hub serves — those whose `target` is in `targets` and
/// whose game matches `game_id` (a connection with no game filter sees all
/// games; a game-scoped event with no game id is broadcast to all) — invoking
/// the event's own `target`, and answer pings until the socket closes.
pub async fn serve(
    socket: WebSocket,
    mut rx: Receiver<HubEvent>,
    targets: &'static [&'static str],
    game_id: Option<i32>,
    authorization: Option<HubAuthorization>,
) {
    let (mut tx, mut ws_rx) = socket.split();

    // 1) Handshake: the client's first frame is `{"protocol":"json","version":1}`.
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

    let mut ping = interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            msg = ws_rx.next() => match msg {
                // Client pings/invocations — nothing to do for our read-only hubs.
                Some(Ok(Message::Text(_))) | Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
            ev = rx.recv() => match ev {
                Ok(event) => {
                    // Only forward events this hub serves, filtered to its game.
                    let game_ok = match (event.game_id, game_id) {
                        (Some(eg), Some(cg)) => eg == cg,
                        _ => true, // event-wide broadcast, or a connection with no filter
                    };
                    if !targets.contains(&event.target) || !game_ok {
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
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            _ = ping.tick() => {
                if let Some(auth) = &authorization {
                    if !auth.is_valid().await { break; }
                }
                if tx.send(Message::Text(format!("{{\"type\":6}}{RS}").into())).await.is_err() { break; }
            }
        }
    }
}
