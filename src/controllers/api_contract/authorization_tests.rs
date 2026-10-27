//! Real route authorization and PostgreSQL responses for covered contracts.
use crate::{
    app_state::AppState, middlewares::privilege_authentication::CurrentUser,
    models::internal::configs::AppConfig, utils::enums::Role,
};
use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use sea_orm::SqlxPostgresConnector;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn covered_routes_keep_anonymous_and_wrong_event_managers_out() {
    let db = crate::utils::test_database::TestDatabase::new("contract_auth").await;
    sqlx::raw_sql(
        r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY);
        CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
        CREATE TABLE "GameManagers" (id SERIAL PRIMARY KEY, game_id INTEGER, user_id UUID);
        INSERT INTO "Games" VALUES (1), (2);"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::raw_sql(crate::migrations::CONTROL_JOBS_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::raw_sql(crate::migrations::CONTROL_JOB_CANCELLATION_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "ControlPlaneJobs" (id, kind, scope_key, game_id, operation_id, fingerprint)
        VALUES ($1, 'BuildBatch', 'game:1', 1, $1, $2)"#,
    )
    .bind(id)
    .bind("a".repeat(64))
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "ControlPlaneJobOperations" (operation_id, job_id) VALUES ($1, $1)"#,
    )
    .bind(id)
    .execute(&db.pool)
    .await
    .unwrap();
    let manager = Uuid::new_v4();
    let outsider = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "GameManagers" (game_id, user_id) VALUES (1, $1), (2, $2)"#)
        .bind(manager)
        .bind(outsider)
        .execute(&db.pool)
        .await
        .unwrap();
    let root = std::env::temp_dir().join(format!("contract-auth-{id}"));
    let state = AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone()),
        Arc::new(AppConfig::default()),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.clone())),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    let router = crate::controllers::edit::router().with_state(state);
    for (method, uri) in [
        (Method::GET, format!("/api/edit/jobs/{id}")),
        (Method::GET, format!("/api/edit/jobs/operations/{id}")),
        (Method::POST, format!("/api/edit/jobs/{id}")),
    ] {
        for (user, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(outsider), StatusCode::FORBIDDEN),
        ] {
            let mut request = Request::builder()
                .method(method.clone())
                .uri(&uri)
                .body(Body::empty())
                .unwrap();
            if let Some(id) = user {
                request.extensions_mut().insert(CurrentUser {
                    id,
                    role: Role::User,
                    name: "fixture".into(),
                    security_stamp: "fixture".into(),
                });
            }
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), expected, "{method} {uri}");
            let body: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 16_384).await.unwrap())
                    .unwrap();
            assert_eq!(body["status"], expected.as_u16());
        }
    }
    for (user, role) in [(manager, Role::User), (Uuid::new_v4(), Role::Admin)] {
        let mut request = Request::builder()
            .uri(format!("/api/edit/jobs/{id}"))
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(CurrentUser {
            id: user,
            role,
            name: "fixture".into(),
            security_stamp: "fixture".into(),
        });
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 16_384).await.unwrap()).unwrap();
        assert_eq!(body["id"], id.to_string());
        assert_eq!(
            body["status"], "Queued",
            "rejected cancellation must leave the job queued"
        );
        assert!(body["createdAtUtc"].is_i64());
    }
    drop(router);
    db.close().await;
    if root.exists() {
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
