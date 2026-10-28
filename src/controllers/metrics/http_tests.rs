use super::*;
use axum::body::{to_bytes, Body};
use axum::http::Request;
use tower::ServiceExt;

fn state() -> SharedState {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unreachable:fixture@127.0.0.1:1/metrics")
        .unwrap();
    crate::app_state::AppState::new(
        sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(pool),
        Arc::new(crate::models::internal::configs::AppConfig::default()),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(
            "/unused-metrics-fixture",
        )),
        crate::services::token::TokenService::new("metrics-fixture-with-32-byte-secret", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    )
}

#[tokio::test]
async fn metrics_http_authentication_headers_and_scrapes_do_not_require_a_database() {
    let token = "metrics-test-token-with-at-least-32-bytes";
    let state = state();
    for configured in [None, Some("short".into()), Some(token.into())] {
        let app = configured_router(configured.clone()).with_state(state.clone());
        for presented in [
            None,
            Some("Bearer wrong"),
            Some(token),
            Some(&format!("Bearer {token}")),
        ] {
            let mut request = Request::builder().uri("/metrics");
            if let Some(value) = presented {
                request = request.header(header::AUTHORIZATION, value);
            }
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                app.clone().oneshot(request.body(Body::empty()).unwrap()),
            )
            .await
            .unwrap()
            .unwrap();
            let valid = configured.as_deref() == Some(token)
                && presented == Some(&format!("Bearer {token}"));
            if valid {
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    "text/plain; version=0.0.4; charset=utf-8"
                );
                let body = to_bytes(response.into_body(), 20_000).await.unwrap();
                let text = std::str::from_utf8(&body).unwrap();
                assert!(text.contains("rsctf_control_job_reclaimed_total{kind=\"AdReset\"}"));
                assert!(!text.contains(token));
            } else {
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                assert!(to_bytes(response.into_body(), 20_000)
                    .await
                    .unwrap()
                    .is_empty());
            }
        }
    }
    assert_eq!(
        state.pg().size(),
        0,
        "scrapes must not acquire database connections"
    );
}
