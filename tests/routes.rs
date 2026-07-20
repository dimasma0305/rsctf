//! Constructing a router runs every controller's route registration, and axum
//! panics on conflicting routes. This test builds the full merged router (and
//! each controller in isolation), so route collisions are caught in CI without
//! needing a database or a running server.

#[test]
fn api_router_builds_without_route_conflicts() {
    // Panics here (E.g. "Overlapping method route") if any two routes collide.
    let _ = rsctf::server::api_router();
    let _ = rsctf::server::web_api_router();
    let _ = rsctf::server::stateful_api_router();
}

#[test]
fn each_controller_router_builds() {
    let _ = rsctf::controllers::account::router();
    let _ = rsctf::controllers::team::router();
    let _ = rsctf::controllers::game::router();
    let _ = rsctf::controllers::game::web_router();
    let _ = rsctf::controllers::edit::router();
    let _ = rsctf::controllers::admin::router();
    let _ = rsctf::controllers::info::router();
    let _ = rsctf::controllers::assets::router();
    let _ = rsctf::controllers::api_token::router();
    let _ = rsctf::controllers::workers::router();
    let _ = rsctf::controllers::game::ad::router();
    let _ = rsctf::controllers::game::ad::web_router();
    let _ = rsctf::controllers::game::ad::stateful_router();
    let _ = rsctf::controllers::game::koth::router();
    let _ = rsctf::controllers::game::koth::web_router();
    let _ = rsctf::controllers::game::koth::stateful_router();
    let _ = rsctf::controllers::admin::ad::router();
    let _ = rsctf::hubs::monitor::router();
    let _ = rsctf::hubs::user::router();
    let _ = rsctf::hubs::admin::router();
}

#[cfg(test)]
mod koth_recovery_ownership {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;
    use uuid::Uuid;

    use rsctf::app_state::{AppState, SharedState};
    use rsctf::models::internal::configs::{AppConfig, RuntimeRole};
    use rsctf::services::cache::InMemoryCache;
    use rsctf::services::container::NoopContainerManager;
    use rsctf::services::token::TokenService;
    use rsctf::storage::LocalBlobStorage;

    const LEGACY: &str = "/api/edit/games/17/ad/koth/23/recover";
    const STATEFUL: &str = "/api/stateful/edit/games/17/ad/koth/23/recover";

    fn test_state(role: RuntimeRole) -> SharedState {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool);
        let root = std::env::temp_dir().join(format!(
            "rsctf-koth-recovery-route-{}",
            Uuid::new_v4().simple()
        ));
        let mut config = AppConfig::default();
        config.runtime_role = role;
        AppState::new(
            database,
            Arc::new(config),
            Arc::new(InMemoryCache::new()),
            Arc::new(LocalBlobStorage::new(root)),
            TokenService::new("0123456789abcdef0123456789abcdef", 60),
            Arc::new(NoopContainerManager),
        )
    }

    async fn post(
        router: axum::Router<SharedState>,
        role: RuntimeRole,
        path: &str,
    ) -> axum::response::Response {
        router
            .with_state(test_state(role))
            .oneshot(Request::post(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn web_exposes_only_the_legacy_method_preserving_redirect() {
        let legacy = post(
            rsctf::controllers::game::koth::web_router(),
            RuntimeRole::Web,
            LEGACY,
        )
        .await;
        assert_eq!(legacy.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(legacy.headers()[header::LOCATION], STATEFUL);

        let fixed = post(
            rsctf::controllers::game::koth::web_router(),
            RuntimeRole::Web,
            STATEFUL,
        )
        .await;
        assert_eq!(fixed.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn privileged_and_monolithic_routers_own_both_recovery_paths() {
        for (router, role) in [
            (
                rsctf::controllers::game::koth::stateful_router(),
                RuntimeRole::Control,
            ),
            (rsctf::controllers::game::koth::router(), RuntimeRole::All),
        ] {
            for path in [LEGACY, STATEFUL] {
                let response = post(router.clone(), role, path).await;
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
            }
        }
    }
}
