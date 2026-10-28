//! The HTTP event editor observes the same durable cleanup fence as executors.
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use sea_orm::SqlxPostgresConnector;
use sea_orm_migration::MigratorTrait;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    app_state::AppState, middlewares::privilege_authentication::CurrentUser,
    models::internal::configs::AppConfig, utils::enums::Role,
};

async fn request(
    router: &axum::Router,
    actor: Option<CurrentUser>,
    method: &str,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::new_v4().to_string())
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    if let Some(actor) = actor {
        request.extensions_mut().insert(actor);
    }
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn http_extension_waits_for_cleanup_and_retries_the_same_operation_after_recovery() {
    let db = crate::utils::test_database::TestDatabase::new("http_cleanup_extension").await;
    let connection = SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone());
    crate::migrations::Migrator::up(&connection, None)
        .await
        .unwrap();
    let admin = CurrentUser {
        id: Uuid::new_v4(),
        role: Role::Admin,
        name: "fixture".into(),
        security_stamp: "fixture".into(),
    };
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
        (id, user_name, normalized_user_name, email, normalized_email, email_confirmed,
         phone_number_confirmed, two_factor_enabled, lockout_enabled, access_failed_count,
         role, ip, last_signed_in_utc, last_visited_utc, register_time_utc, bio,
         real_name, std_number, exercise_visible)
        VALUES ($1, 'fixture', 'FIXTURE', 'fixture@example.test', 'FIXTURE@EXAMPLE.TEST', TRUE,
            FALSE, FALSE, FALSE, 0, 1, '', now(), now(), now(), '', '', '', TRUE)"#,
    )
    .bind(admin.id)
    .execute(&db.pool)
    .await
    .unwrap();
    let root = std::env::temp_dir().join(format!("cleanup-http-{}", Uuid::new_v4()));
    let mut config = AppConfig::default();
    config.storage_root = root.to_string_lossy().into_owned();
    let state = AppState::new(
        connection,
        Arc::new(config),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.clone())),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    let router = crate::controllers::edit::router().with_state(state.clone());
    let now = chrono::Utc::now();
    let (status, mut game) = request(
        &router,
        Some(admin.clone()),
        "POST",
        "/api/edit/games",
        &json!({"title": "Cleanup fixture", "hidden": true, "practiceMode": false,
            "start": (now - chrono::Duration::hours(2)).timestamp_millis(),
            "end": (now - chrono::Duration::hours(1)).timestamp_millis(),
            "writeupDeadline": (now + chrono::Duration::hours(2)).timestamp_millis()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{game}");
    let game_id = game["id"].as_i64().unwrap() as i32;
    let path = format!("/api/edit/games/{game_id}");
    let (status, challenge) = request(&router, Some(admin.clone()), "POST", &format!("{path}/challenges"),
        &json!({"title": "Hill", "category": "Misc", "type": "KingOfTheHill", "operationId": Uuid::new_v4()})).await;
    assert_eq!(status, StatusCode::OK, "{challenge}");
    let challenge_id = challenge["id"].as_i64().unwrap() as i32;
    sqlx::query(
        r#"INSERT INTO "KothTargets"
        (game_id, challenge_id, host, port, container_id, cleanup_lease_token, cleanup_lease_until)
        VALUES ($1, $2, '10.0.0.1', 80, 'runtime-fixture', $3, now() - interval '1 minute')"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(Uuid::new_v4())
    .execute(&db.pool)
    .await
    .unwrap();
    let original_end = game["end"].clone();
    let original_revision = game["configurationRevision"].clone();
    game["end"] = json!((now + chrono::Duration::hours(1)).timestamp_millis());
    game["operationId"] = json!(Uuid::new_v4());
    let outsider = CurrentUser {
        role: Role::User,
        id: Uuid::new_v4(),
        ..admin.clone()
    };
    for (actor, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(outsider), StatusCode::FORBIDDEN),
        (Some(admin.clone()), StatusCode::CONFLICT),
    ] {
        let (status, body) = request(&router, actor, "PUT", &path, &game).await;
        assert_eq!(status, expected, "{body}");
        if status == StatusCode::CONFLICT {
            assert_eq!(body["status"], 409);
            assert!(body["title"].as_str().unwrap().contains("cleanup"));
        }
    }
    let stored: (chrono::DateTime<chrono::Utc>, i64) =
        sqlx::query_as(r#"SELECT end_time_utc, configuration_revision FROM "Games" WHERE id = $1"#)
            .bind(game_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(json!(stored.0.timestamp_millis()), original_end);
    assert_eq!(json!(stored.1), original_revision);

    // Conclusive teardown releases the fence; the rejected operation was not
    // consumed, so this exact request can now commit once and replay once.
    sqlx::query(
        r#"UPDATE "KothTargets" SET host = '', port = 0, container_id = NULL,
        cleanup_lease_token = NULL, cleanup_lease_until = NULL WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&db.pool)
    .await
    .unwrap();
    for _ in 0..2 {
        let (status, body) = request(&router, Some(admin.clone()), "PUT", &path, &game).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["end"], game["end"]);
        assert_eq!(
            body["configurationRevision"].as_i64().unwrap(),
            stored.1 + 1
        );
    }
    drop(router);
    drop(state);
    db.close().await;
    if root.exists() {
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
