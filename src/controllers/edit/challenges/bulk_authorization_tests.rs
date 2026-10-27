//! Authorization remains enforced when bulk mutations bypass the HTTP adapter.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use sea_orm::SqlxPostgresConnector;
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    app_state::AppState,
    middlewares::privilege_authentication::CurrentUser,
    models::internal::configs::AppConfig,
    services::bulk_challenges::{self, BulkChallengeAction, BulkChallengeMutationRequest},
    utils::enums::Role,
};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn http_and_application_boundaries_reject_wrong_event_actors_before_admission() {
    let db = crate::utils::test_database::TestDatabase::new("bulk_auth").await;
    sqlx::raw_sql(
        r#"CREATE TABLE "GameManagers" (
        id SERIAL PRIMARY KEY, game_id INTEGER, user_id UUID
    );"#,
    )
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
    let root = std::env::temp_dir().join(format!("bulk-auth-{}", Uuid::new_v4()));
    let state = AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone()),
        Arc::new(AppConfig::default()),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.clone())),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    let router = crate::controllers::edit::router().with_state(state.clone());
    // Invalid content distinguishes authorization from validation. No mutation
    // tables exist: unauthorized actors must never reach queue admission.
    let intent = BulkChallengeMutationRequest {
        operation_id: Uuid::nil(),
        expected_revision: 0,
        action: BulkChallengeAction::Delete,
        challenge_ids: vec![],
    };
    for (actor, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some((outsider, Role::User)), StatusCode::FORBIDDEN),
        (Some((Uuid::new_v4(), Role::User)), StatusCode::FORBIDDEN),
        (Some((manager, Role::User)), StatusCode::BAD_REQUEST),
        (Some((Uuid::new_v4(), Role::Admin)), StatusCode::BAD_REQUEST),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/edit/games/1/challenges/bulk")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&intent).unwrap()))
            .unwrap();
        if let Some((id, role)) = actor {
            let principal = CurrentUser {
                id,
                role,
                name: "fixture".into(),
                security_stamp: "fixture".into(),
            };
            let error = bulk_challenges::mutate_challenges_bulk(
                state.clone(),
                principal.clone(),
                1,
                intent.clone(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.status(), expected, "application authorization");
            request.extensions_mut().insert(principal);
        }
        let response = router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected, "HTTP authorization");
    }
    drop(router);
    drop(state);
    db.close().await;
    if root.exists() {
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
