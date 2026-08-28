//! Named per-route rate-limit decorator and shared rejection response.

use super::{check_async, partition_key, Policy};
use axum::extract::Request;
use axum::http::{header, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;

use crate::app_state::SharedState;

/// Decorate a single route handler with a named policy — the axum analogue of
/// RSCTF's `[EnableRateLimiting(policy)]` attribute. The wrapped handler is
/// checked in addition to the always-on global window.
pub fn limited(policy: Policy, handler: MethodRouter<SharedState>) -> MethodRouter<SharedState> {
    handler.layer(axum::middleware::from_fn(
        move |req: Request, next: Next| run_policy(policy, req, next),
    ))
}

async fn run_policy(policy: Policy, req: Request, next: Next) -> Response {
    if let Err(retry_after) = check_async(policy, partition_key(policy, &req)).await {
        return too_many_requests(retry_after);
    }
    next.run(req).await
}

/// Build the normal typed 429 response with a whole-second retry hint.
pub(super) fn too_many_requests(retry_after: u64) -> Response {
    let mut response = crate::utils::shared::MessageResponse::new(
        format!("Too many requests. Please retry after {retry_after} seconds."),
        429,
    )
    .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}
