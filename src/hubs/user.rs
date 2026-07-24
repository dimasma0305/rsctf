//! hubs/user.rs — RSCTF `UserHub` (IUserClient.ReceivedGameNotice) over SignalR.
use std::collections::HashMap;
use std::net::SocketAddr;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::app_state::SharedState;
use crate::hubs::{admission, signalr};
use crate::middlewares::rate_limiter::{limited, Policy};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/hub/user",
            limited(Policy::PublicHubAdmission, get(user_hub)),
        )
        .route(
            "/hub/user/negotiate",
            limited(Policy::PublicHubAdmission, post(signalr::negotiate)),
        )
}

async fn user_hub(
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
    let rx = st.events.subscribe();
    signalr::bounded_upgrade(ws)
        .on_upgrade(move |s| {
            signalr::serve(
                s,
                rx,
                &["ReceivedGameNotice"],
                Some(scope.game_id),
                scope.authorization,
                connection_permit,
            )
        })
        .into_response()
}
