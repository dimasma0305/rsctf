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
        .route("/hub/admin/negotiate", post(signalr::admin_negotiate))
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
        Some(_) => StatusCode::FORBIDDEN.into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;
    use crate::app_state::AppState;
    use crate::middlewares::privilege_authentication::CurrentUser;
    use crate::models::internal::configs::AppConfig;
    use crate::services::cache::InMemoryCache;
    use crate::services::container::NoopContainerManager;
    use crate::services::token::TokenService;
    use crate::storage::LocalBlobStorage;

    const NEGOTIATE_ROUTES: [&str; 2] = ["/hub/admin/negotiate", "/hub/containerExec/negotiate"];

    fn test_state() -> SharedState {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool);
        let storage_root = std::env::temp_dir().join(format!(
            "rsctf-organizer-hub-auth-{}",
            Uuid::new_v4().simple()
        ));

        AppState::new(
            database,
            Arc::new(AppConfig::default()),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(storage_root)),
            TokenService::new("0123456789abcdef0123456789abcdef", 60),
            Arc::new(NoopContainerManager),
        )
    }

    fn test_app() -> Router {
        Router::new()
            .merge(router())
            .merge(crate::hubs::container::router())
            .with_state(test_state())
    }

    async fn negotiate(path: &str, role: Option<Role>) -> Response {
        let mut request = Request::post(path).body(Body::empty()).unwrap();
        if let Some(role) = role {
            request.extensions_mut().insert(CurrentUser {
                id: Uuid::new_v4(),
                role,
                name: "organizer-hub-test".to_string(),
            });
        }
        test_app().oneshot(request).await.unwrap()
    }

    #[tokio::test]
    async fn organizer_negotiate_routes_reject_anonymous_and_non_admin_users() {
        for path in NEGOTIATE_ROUTES {
            assert_eq!(
                negotiate(path, None).await.status(),
                StatusCode::UNAUTHORIZED,
                "anonymous {path}"
            );
            for role in [Role::User, Role::Monitor] {
                assert_eq!(
                    negotiate(path, Some(role)).await.status(),
                    StatusCode::FORBIDDEN,
                    "role {role:?} on {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn organizer_negotiate_routes_preserve_the_signalr_contract_for_admins() {
        for path in NEGOTIATE_ROUTES {
            let response = negotiate(path, Some(Role::Admin)).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            let body = to_bytes(response.into_body(), 4_096).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

            assert_eq!(value["negotiateVersion"], 1, "{path}");
            assert_eq!(value["connectionId"], value["connectionToken"], "{path}");
            assert!(
                Uuid::parse_str(value["connectionId"].as_str().unwrap()).is_ok(),
                "{path}"
            );
            assert_eq!(
                value["availableTransports"],
                serde_json::json!([{
                    "transport": "WebSockets",
                    "transferFormats": ["Text", "Binary"]
                }]),
                "{path}"
            );
        }
    }
}
