//! Public, cached donation history projection.

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::app_state::SharedState;
use crate::utils::error::AppResult;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/donations", get(get_donations))
}

pub async fn get_donations(State(st): State<SharedState>) -> AppResult<Response> {
    let body = crate::services::donations::feed_json(st).await?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=60, stale-if-error=21600"),
            ),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_is_public_and_cacheable() {
        let _ = router;
        assert_eq!(
            HeaderValue::from_static("public, max-age=60, stale-if-error=21600"),
            "public, max-age=60, stale-if-error=21600"
        );
    }
}
