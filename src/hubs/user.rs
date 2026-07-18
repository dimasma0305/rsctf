//! hubs/user.rs — RSCTF `UserHub` (IUserClient.ReceivedGameNotice) over SignalR.
use std::collections::HashMap;

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::app_state::SharedState;
use crate::hubs::signalr;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/hub/user", get(user_hub))
        .route("/hub/user/negotiate", post(signalr::negotiate))
}

async fn user_hub(
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
            &["ReceivedGameNotice"],
            Some(scope.game_id),
            scope.authorization,
        )
    })
    .into_response()
}
