use std::str::FromStr;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt;

use super::*;
use crate::app_state::AppState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::Role;

fn request(path: &str, role: Option<Role>) -> Request<Body> {
    let mut request = Request::builder().uri(path).body(Body::empty()).unwrap();
    if let Some(role) = role {
        request.extensions_mut().insert(CurrentUser {
            id: Uuid::new_v4(),
            role,
            name: "fixture".to_owned(),
            security_stamp: "fixture-stamp".to_owned(),
        });
    }
    request
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn monitor_backfills_enforce_role_game_start_scope_and_hard_page_bound() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("monitor_feed_http_{}", Uuid::new_v4().simple());
    assert!(schema
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
            id INTEGER PRIMARY KEY,
            start_time_utc TIMESTAMPTZ NOT NULL,
            deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "AspNetUsers" (
            id UUID PRIMARY KEY,
            user_name TEXT
        );
        CREATE TABLE "Teams" (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE "GameChallenges" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            title TEXT NOT NULL
        );
        CREATE TABLE "GameEvents" (
            id INTEGER PRIMARY KEY,
            game_id INTEGER NOT NULL,
            "Type" SMALLINT NOT NULL,
            "values" JSONB NOT NULL,
            publish_time_utc TIMESTAMPTZ NOT NULL,
            user_id UUID,
            team_id INTEGER NOT NULL,
            feed_cursor BIGINT
        );
        CREATE TABLE "Submissions" (
            id INTEGER PRIMARY KEY,
            answer TEXT NOT NULL,
            status SMALLINT NOT NULL,
            submit_time_utc TIMESTAMPTZ NOT NULL,
            user_id UUID,
            team_id INTEGER NOT NULL,
            game_id INTEGER NOT NULL,
            challenge_id INTEGER NOT NULL,
            feed_cursor BIGINT
        );
        INSERT INTO "Games" (id, start_time_utc) VALUES
            (7, clock_timestamp() - interval '1 hour'),
            (8, clock_timestamp() - interval '1 hour'),
            (9, clock_timestamp() + interval '1 hour');
        INSERT INTO "Teams" (id, name) VALUES (1, 'alpha'), (2, 'other');
        INSERT INTO "GameChallenges" (id, game_id, title) VALUES
            (70, 7, 'seven'), (80, 8, 'eight');
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    for id in 1..=125_i32 {
        sqlx::query(
            r#"INSERT INTO "Submissions"
                 (id, answer, status, submit_time_utc, team_id, game_id, challenge_id, feed_cursor)
               VALUES ($1, $1::text, 1, clock_timestamp(), 1, 7, 70, $1)"#,
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "Submissions"
             (id, answer, status, submit_time_utc, team_id, game_id, challenge_id, feed_cursor)
           VALUES (1000, 'wrong-game', 1, clock_timestamp(), 2, 8, 80, 1000)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    for id in 1..=125_i32 {
        sqlx::query(
            r#"INSERT INTO "GameEvents"
                 (id, game_id, "Type", "values", publish_time_utc, team_id, feed_cursor)
               VALUES ($1, 7, $2, jsonb_build_array($1::text), clock_timestamp(), 1, $1)"#,
        )
        .bind(id)
        .bind(EventType::Normal as i16)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "GameEvents"
             (id, game_id, "Type", "values", publish_time_utc, team_id, feed_cursor)
           VALUES (1000, 8, $1, '["wrong-game"]', clock_timestamp(), 2, 1000)"#,
    )
    .bind(EventType::Normal as i16)
    .execute(&pool)
    .await
    .unwrap();

    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    let storage_root = std::env::temp_dir().join(format!("rsctf-monitor-feed-{}", Uuid::new_v4()));
    let mut config = AppConfig::default();
    config.storage_root = storage_root.to_string_lossy().into_owned();
    config.jwt_secret = "0123456789abcdef0123456789abcdef".to_owned();
    let state = AppState::new(
        database,
        Arc::new(config),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(storage_root)),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );
    let app = Router::new()
        .route("/api/game/{id}/events/backfill", get(event_backfill))
        .route(
            "/api/game/{id}/submissions/backfill",
            get(submission_backfill),
        )
        .with_state(state);

    assert_eq!(
        app.clone()
            .oneshot(request("/api/game/7/events/backfill?after=0", None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "/api/game/7/events/backfill?after=0",
                Some(Role::User),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "/api/game/9/events/backfill?after=0",
                Some(Role::Monitor),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "/api/game/404/events/backfill?after=0",
                Some(Role::Admin),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    let response = app
        .clone()
        .oneshot(request(
            "/api/game/7/events/backfill?after=0&limit=999",
            Some(Role::Monitor),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 100);
    assert_eq!(page["nextCursor"], 100);
    assert_eq!(page["hasMore"], true);
    assert!(page["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["team"] == "alpha"));

    let checkpoint = app
        .clone()
        .oneshot(request("/api/game/7/events/backfill", Some(Role::Admin)))
        .await
        .unwrap();
    let checkpoint = to_bytes(checkpoint.into_body(), 16 * 1024).await.unwrap();
    let checkpoint: serde_json::Value = serde_json::from_slice(&checkpoint).unwrap();
    assert_eq!(checkpoint["events"].as_array().unwrap().len(), 0);
    assert_eq!(checkpoint["nextCursor"], 125);

    for (role, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(Role::User), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(request("/api/game/7/submissions/backfill?after=0", role,))
                .await
                .unwrap()
                .status(),
            expected
        );
    }
    assert_eq!(
        app.clone()
            .oneshot(request(
                "/api/game/9/submissions/backfill?after=0",
                Some(Role::Monitor),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.clone()
            .oneshot(request(
                "/api/game/404/submissions/backfill?after=0",
                Some(Role::Admin),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    let submissions = app
        .clone()
        .oneshot(request(
            "/api/game/7/submissions/backfill?after=0&limit=999",
            Some(Role::Monitor),
        ))
        .await
        .unwrap();
    assert_eq!(submissions.status(), StatusCode::OK);
    let body = to_bytes(submissions.into_body(), 256 * 1024).await.unwrap();
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(page["submissions"].as_array().unwrap().len(), 100);
    assert_eq!(page["nextCursor"], 100);
    assert_eq!(page["hasMore"], true);
    assert!(page["submissions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|submission| submission["team"] == "alpha" && submission["challenge"] == "seven"));

    let checkpoint = app
        .oneshot(request(
            "/api/game/7/submissions/backfill",
            Some(Role::Admin),
        ))
        .await
        .unwrap();
    let checkpoint = to_bytes(checkpoint.into_body(), 16 * 1024).await.unwrap();
    let checkpoint: serde_json::Value = serde_json::from_slice(&checkpoint).unwrap();
    assert_eq!(checkpoint["submissions"].as_array().unwrap().len(), 0);
    assert_eq!(checkpoint["nextCursor"], 125);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
