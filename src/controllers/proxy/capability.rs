//! Narrow credentials used by the native WSRX helper.
//!
//! A desktop WSRX tunnel opens the remote WebSocket itself and therefore does
//! not inherit the browser's HttpOnly session cookie. The authenticated page
//! mints a short-lived bearer bound to one user, security stamp, container and
//! route class. The WebSocket still runs the ordinary live authorization and
//! exact target fences before opening the backend stream.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{resolve_instance_target, resolve_noinstance_target};
use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{
    authenticate_live_identity, AdminUser, CurrentUser, MaybeUser,
};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Default, Deserialize)]
pub(super) struct ProxyCapabilityQuery {
    capability: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyCapabilityModel {
    token: String,
}

fn no_store_capability(token: String) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(ProxyCapabilityModel { token }),
    )
}

/// WSRX measures a tunnel with an unauthenticated HTTP OPTIONS request before
/// it opens the WebSocket. This probe reveals no target data and never opens a
/// connection, so a uniform 204 is both safe and compatible with Pingfall.
pub(super) async fn proxy_latency_probe() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub(super) async fn issue_instance_capability(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    if resolve_instance_target(&st, MaybeUser(Some(user.clone())), id)
        .await
        .is_none()
    {
        return Err(AppError::not_found("Proxy target not found"));
    }
    let token = st
        .token
        .issue_proxy_capability(user.id, &user.security_stamp, id, false)?;
    Ok(no_store_capability(token))
}

pub(super) async fn issue_noinstance_capability(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    if resolve_noinstance_target(&st, id).await.is_none() {
        return Err(AppError::not_found("Proxy target not found"));
    }
    let token = st
        .token
        .issue_proxy_capability(user.id, &user.security_stamp, id, true)?;
    Ok(no_store_capability(token))
}

/// Prefer an ordinary browser/API principal when present. Native WSRX carries
/// only the narrow query capability; resolve its subject against the current
/// account row so a ban, role change, logout or stamp rotation still revokes it.
pub(super) async fn proxy_user(
    st: &SharedState,
    user: MaybeUser,
    query: ProxyCapabilityQuery,
    container_id: Uuid,
    preview: bool,
) -> Option<CurrentUser> {
    if let Some(user) = user.0 {
        if !preview || user.is_admin() {
            return Some(user);
        }
    }

    let token = query.capability.as_deref()?;
    let identity = st
        .token
        .verify_proxy_capability(token, container_id, preview)
        .ok()?;
    let user = authenticate_live_identity(st, identity.user_id, &identity.security_stamp)
        .await
        .ok()?;
    (!preview || user.is_admin()).then_some(user)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wsrx_latency_probe_is_a_success_without_opening_a_target() {
        assert_eq!(proxy_latency_probe().await, StatusCode::NO_CONTENT);
        assert_eq!(
            include_str!("mod.rs")
                .matches(".options(proxy_latency_probe)")
                .count(),
            2
        );
    }
}
