//! Narrow credentials used by the native WSRX helper.
//!
//! A desktop WSRX tunnel opens the remote WebSocket itself and therefore does
//! not inherit the browser's HttpOnly session cookie. The authenticated page
//! mints a short-lived bearer bound to one user, security stamp, container and
//! route class. The WebSocket still runs the ordinary live authorization and
//! exact target fences before opening the backend stream.

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{resolve_instance_target, resolve_noinstance_target};
use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{
    authenticate_live_identity, AdminUser, CurrentUser, MaybeUser,
};
use crate::services::token::{IssuedProxyCapability, TokenService};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Default, Deserialize)]
pub(super) struct ProxyCapabilityQuery {
    capability: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyCapabilityModel {
    token: String,
    #[serde(with = "crate::utils::datetime::millis")]
    expires_at: DateTime<Utc>,
}

fn no_store_capability(capability: IssuedProxyCapability) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(ProxyCapabilityModel {
            token: capability.token,
            expires_at: capability.expires_at,
        }),
    )
}

/// WSRX measures a tunnel with an HTTP OPTIONS request before it opens the
/// WebSocket. Verify the signed query capability without touching the database,
/// so an expired or mismatched tunnel is not advertised as netcat-ready.
fn proxy_latency_status(
    token_service: &TokenService,
    query: ProxyCapabilityQuery,
    container_id: Uuid,
    preview: bool,
) -> StatusCode {
    let Some(token) = query.capability.as_deref() else {
        return StatusCode::NOT_FOUND;
    };
    if token_service
        .verify_proxy_capability(token, container_id, preview)
        .is_ok()
    {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

pub(super) async fn proxy_instance_latency_probe(
    State(st): State<SharedState>,
    Query(query): Query<ProxyCapabilityQuery>,
    Path(id): Path<Uuid>,
) -> StatusCode {
    proxy_latency_status(&st.token, query, id, false)
}

pub(super) async fn proxy_noinstance_latency_probe(
    State(st): State<SharedState>,
    Query(query): Query<ProxyCapabilityQuery>,
    Path(id): Path<Uuid>,
) -> StatusCode {
    proxy_latency_status(&st.token, query, id, true)
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

/// Resolve only the authenticated subject needed for pre-resolution churn
/// admission. Capability verification is local signature work; the live account
/// lookup stays behind this budget in `proxy_user`.
pub(super) fn proxy_subject(
    st: &SharedState,
    user: &MaybeUser,
    query: &ProxyCapabilityQuery,
    container_id: Uuid,
    preview: bool,
) -> Option<Uuid> {
    if let Some(user) = user.0.as_ref() {
        return Some(user.id);
    }
    let token = query.capability.as_deref()?;
    st.token
        .verify_proxy_capability(token, container_id, preview)
        .ok()
        .map(|identity| identity.user_id)
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

    #[test]
    fn proxy_capability_expiry_is_a_unix_millisecond_number() {
        let expires_at = DateTime::from_timestamp(1_725_000_000, 123_000_000).unwrap();
        let value = serde_json::to_value(ProxyCapabilityModel {
            token: "scoped-token".to_owned(),
            expires_at,
        })
        .unwrap();

        assert_eq!(value["expiresAt"], 1_725_000_000_123_i64);
    }

    #[test]
    fn wsrx_latency_probe_requires_an_exact_live_capability() {
        let service = TokenService::new("0123456789abcdef0123456789abcdef", 60);
        let container_id = Uuid::new_v4();
        let capability = service
            .issue_proxy_capability(Uuid::new_v4(), "stamp-1", container_id, false)
            .unwrap();

        assert_eq!(
            proxy_latency_status(
                &service,
                ProxyCapabilityQuery {
                    capability: Some(capability.token.clone()),
                },
                container_id,
                false,
            ),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            proxy_latency_status(
                &service,
                ProxyCapabilityQuery {
                    capability: Some(capability.token),
                },
                container_id,
                true,
            ),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            proxy_latency_status(
                &service,
                ProxyCapabilityQuery::default(),
                container_id,
                false,
            ),
            StatusCode::NOT_FOUND
        );
    }
}
