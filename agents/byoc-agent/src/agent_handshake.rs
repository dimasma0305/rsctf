use std::time::Duration;

use super::{ConnectFailure, AGENT_STATE_HEADER, MAXIMUM_SERVER_RETRY_AFTER};

fn retry_after(
    response: &tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
) -> Option<Duration> {
    let seconds = response
        .headers()
        .get(tokio_tungstenite::tungstenite::http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()?;
    Some(Duration::from_secs(seconds).min(MAXIMUM_SERVER_RETRY_AFTER))
}

pub(super) fn handshake_failure(
    response: tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
) -> ConnectFailure {
    let status = response.status();
    let state = response
        .headers()
        .get(AGENT_STATE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let terminal = state
        .as_deref()
        .is_some_and(|state| state.starts_with("terminal-"))
        || status == tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED
        || status == tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN
        || status == tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND
        || status == tokio_tungstenite::tungstenite::http::StatusCode::GONE
        || status == tokio_tungstenite::tungstenite::http::StatusCode::UPGRADE_REQUIRED
        || status.is_client_error()
            && status != tokio_tungstenite::tungstenite::http::StatusCode::REQUEST_TIMEOUT
            && status != tokio_tungstenite::tungstenite::http::StatusCode::TOO_MANY_REQUESTS
            && status != tokio_tungstenite::tungstenite::http::StatusCode::TOO_EARLY;
    ConnectFailure {
        message: format!("WebSocket handshake returned {status}"),
        state,
        terminal,
        retry_after: retry_after(&response),
        connected_for: None,
    }
}
