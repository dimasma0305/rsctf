use super::transport::{normal_close, transport_failure_close, BUFFER_SIZE};
use super::*;
use axum::extract::ws::{close_code, Message};

#[test]
fn proxy_client_messages_have_a_small_memory_bound() {
    assert_eq!(MAX_CLIENT_MESSAGE_SIZE, 64 * 1024);
    const { assert!(MAX_CLIENT_MESSAGE_SIZE <= BUFFER_SIZE * 16) };
}

#[test]
fn proxy_close_frames_use_explicit_generic_codes() {
    let frame = |message| match message {
        Message::Close(Some(frame)) => frame,
        _ => panic!("expected an explicit WebSocket close frame"),
    };

    let normal = frame(normal_close());
    assert_eq!(normal.code, close_code::NORMAL);
    assert!(normal.reason.is_empty());

    let unavailable = frame(endpoint_unavailable_close());
    assert_eq!(unavailable.code, close_code::AGAIN);
    assert_eq!(unavailable.reason.as_str(), "proxy endpoint unavailable");

    let failed = frame(transport_failure_close());
    assert_eq!(failed.code, close_code::ERROR);
    assert_eq!(failed.reason.as_str(), "proxy transport failed");
}

fn exercise_row(user_id: Uuid) -> ExerciseAccessRow {
    ExerciseAccessRow {
        exercise_instance_id: 41,
        exercise_id: 9,
        user_id,
        is_loaded: true,
        is_enabled: true,
        publish_time_utc: chrono::Utc::now() - chrono::Duration::minutes(1),
    }
}

#[test]
fn exercise_access_requires_exact_live_owner_and_unambiguous_identity() {
    let owner = Uuid::new_v4();
    let now = chrono::Utc::now();
    let row = exercise_row(owner);
    assert_eq!(
        authorize_exercise_access(Some(41), owner, now, std::slice::from_ref(&row)),
        Some(ExerciseAccess {
            exercise_instance_id: 41,
            exercise_id: 9,
        })
    );
    assert!(authorize_exercise_access(None, owner, now, std::slice::from_ref(&row)).is_some());
    assert!(authorize_exercise_access(Some(42), owner, now, std::slice::from_ref(&row)).is_none());
    assert!(
        authorize_exercise_access(Some(41), Uuid::new_v4(), now, std::slice::from_ref(&row))
            .is_none()
    );
    assert!(authorize_exercise_access(Some(41), owner, now, &[row.clone(), row]).is_none());
}

#[test]
fn exercise_access_rejects_unloaded_disabled_and_unpublished_instances() {
    let owner = Uuid::new_v4();
    let now = chrono::Utc::now();
    let mut row = exercise_row(owner);
    row.is_loaded = false;
    assert!(authorize_exercise_access(Some(41), owner, now, &[row.clone()]).is_none());
    row.is_loaded = true;
    row.is_enabled = false;
    assert!(authorize_exercise_access(Some(41), owner, now, &[row.clone()]).is_none());
    row.is_enabled = true;
    row.publish_time_utc = now + chrono::Duration::minutes(1);
    assert!(authorize_exercise_access(Some(41), owner, now, &[row]).is_none());
}

#[test]
fn exercise_queries_bind_both_sides_and_keep_legacy_links_revocable() {
    assert!(EXERCISE_ACCESS_SQL.contains("instance.container_id = $1"));
    assert!(EXERCISE_ACCESS_SQL.contains("$2::INTEGER IS NULL OR instance.id = $2"));
    assert!(EXERCISE_LEASE_SQL.contains("container.id = instance.container_id"));
    assert!(EXERCISE_LEASE_SQL.contains("container.exercise_instance_id IS NULL"));
    assert!(EXERCISE_LEASE_SQL.contains("container.exercise_instance_id = instance.id"));
    assert!(EXERCISE_LEASE_SQL.contains("account.security_stamp = $5"));
    assert!(EXERCISE_LEASE_SQL.contains("account.email_confirmed = TRUE"));
    assert!(EXERCISE_LEASE_SQL.contains("account.role <> $6"));
    assert!(LEGACY_EXERCISE_OWNER_SQL.contains("container_id = $1"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exercise_lease_revokes_when_the_account_session_changes() {
    use sqlx::postgres::PgPoolOptions;

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("exercise_lease_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = crate::migrations::test_pg_connect_options(&database_url)
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY, security_stamp TEXT,
          email_confirmed BOOLEAN NOT NULL, role SMALLINT NOT NULL
        );
        CREATE TABLE "ExerciseChallenges" (
          id INTEGER PRIMARY KEY, is_enabled BOOLEAN NOT NULL,
          publish_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Containers" (
          id UUID PRIMARY KEY, is_proxy BOOLEAN NOT NULL,
          game_instance_id INTEGER, exercise_instance_id INTEGER
        );
        CREATE TABLE "ExerciseInstances" (
          id INTEGER PRIMARY KEY, exercise_id INTEGER NOT NULL,
          user_id UUID NOT NULL, is_loaded BOOLEAN NOT NULL,
          container_id UUID NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let user_id = Uuid::new_v4();
    let container_id = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, 'stamp', TRUE, $2)"#)
        .bind(user_id)
        .bind(Role::User as i16)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "ExerciseChallenges"
             VALUES (9, TRUE, clock_timestamp() - interval '1 minute')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Containers" VALUES ($1, TRUE, NULL, 41)"#)
        .bind(container_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "ExerciseInstances" VALUES (41, 9, $1, TRUE, $2)"#)
        .bind(user_id)
        .bind(container_id)
        .execute(&pool)
        .await
        .unwrap();

    let live = || exercise_lease_is_valid(&pool, user_id, "stamp", 41, 9, container_id);
    assert!(live().await);

    sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = 'rotated' WHERE id = $1"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    // Established sessions poll on a five-second cadence and the shared
    // authorization result is allowed a much smaller bounded freshness
    // window. Exercise the next authoritative lease check, not a deliberately
    // fresh cached result from the preceding assertion.
    tokio::time::sleep(
        super::authorization::EXERCISE_LEASE_FRESHNESS + std::time::Duration::from_millis(25),
    )
    .await;
    assert!(!live().await);

    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET security_stamp = 'stamp', email_confirmed = FALSE
            WHERE id = $1"#,
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(!live().await);

    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email_confirmed = TRUE, role = $2
            WHERE id = $1"#,
    )
    .bind(user_id)
    .bind(Role::Banned as i16)
    .execute(&pool)
    .await
    .unwrap();
    assert!(!live().await);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
