//! Authenticated monitoring surface shared by API and background runtime roles.
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::app_state::SharedState;

fn authorized(configured: Option<&str>, presented: Option<&str>) -> bool {
    let Some(expected) = configured.filter(|value| value.len() >= 32) else {
        return false;
    };
    let Some(actual) = presented.and_then(|value| value.strip_prefix("Bearer ")) else {
        return false;
    };
    crate::utils::crypto_utils::ct_eq(expected, actual)
}

pub(crate) async fn prometheus(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let configured = std::env::var("RSCTF_METRICS_TOKEN").ok();
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    if !authorized(configured.as_deref(), presented) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let body = crate::services::observability::render(state.pg().size(), state.pg().num_idle());
    (
        [
            (
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exporter_requires_an_explicit_strong_token_and_exact_bearer() {
        let token = "x".repeat(32);
        let bearer = format!("Bearer {token}");
        assert!(authorized(Some(&token), Some(&bearer)));
        for configured in [None, Some(""), Some("weak")] {
            assert!(!authorized(configured, Some(&bearer)));
        }
        for presented in [None, Some("Bearer wrong"), Some(token.as_str())] {
            assert!(!authorized(Some(&token), presented));
        }
    }
}
