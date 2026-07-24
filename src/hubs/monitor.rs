//! hubs/monitor.rs — RSCTF `MonitorHub` (IMonitorClient) over SignalR.
use std::collections::HashMap;
use std::net::SocketAddr;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use sea_orm::EntityTrait;

use crate::app_state::SharedState;
use crate::hubs::{admission, signalr};
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::models::data::game;
use crate::utils::enums::Role;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/hub/monitor",
            limited(Policy::PrivilegedHubAdmission, get(monitor_hub)),
        )
        .route(
            "/hub/monitor/negotiate",
            limited(
                Policy::PrivilegedHubAdmission,
                post(signalr::monitor_negotiate),
            ),
        )
}

async fn monitor_hub(
    ws: WebSocketUpgrade,
    State(st): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    match signalr::hub_identity(&st, &params, &headers).await {
        Some((user, token)) if user.is_monitor() => {
            // The monitor page subscribes to BOTH ReceivedGameEvent (events feed)
            // and ReceivedSubmissions (submissions feed) on one connection.
            let game_id = match params.get("game") {
                Some(raw) => match raw.parse::<i32>() {
                    Ok(id) => match game::Entity::find_by_id(id).one(&st.db).await {
                        Ok(Some(_)) => Some(id),
                        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
                        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    },
                    Err(_) => return StatusCode::BAD_REQUEST.into_response(),
                },
                None => None,
            };
            let rx = st.events.subscribe();
            let Some(connection_permit) = admission::try_connection_permit(
                admission::client_key(&headers, peer.ip()),
                game_id.map_or(admission::Scope::Global, admission::Scope::Game),
            ) else {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            };
            let authorization = signalr::HubAuthorization::new(st, token, Role::Monitor);
            signalr::bounded_upgrade(ws)
                .on_upgrade(move |s| {
                    signalr::serve(
                        s,
                        rx,
                        &["ReceivedGameEvent", "ReceivedSubmissions"],
                        game_id,
                        Some(authorization),
                        connection_permit,
                    )
                })
                .into_response()
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}
