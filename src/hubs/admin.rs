//! hubs/admin.rs — RSCTF `AdminHub` (IAdminClient) over SignalR.
use std::collections::HashMap;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::app_state::SharedState;
use crate::hubs::signalr;
use crate::utils::enums::Role;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/hub/admin", get(admin_hub))
        .route("/hub/admin/negotiate", post(signalr::negotiate))
}

async fn admin_hub(
    ws: WebSocketUpgrade,
    State(st): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    match signalr::hub_identity(&st, &params, &headers).await {
        Some((user, token)) if user.is_admin() => {
            // Admin log stream is global (not game-scoped).
            let rx = st.events.subscribe();
            let authorization = signalr::HubAuthorization::new(st, token, Role::Admin);
            ws.on_upgrade(move |s| {
                signalr::serve(s, rx, &["ReceivedLog"], None, Some(authorization))
            })
            .into_response()
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}
