use std::str::FromStr;

use chrono::{DateTime, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::super::*;
use crate::utils::enums::ChallengeType;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn competing_distinct_snapshots_serialize_before_snapshot_work() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_koth_distinct_{}", uuid::Uuid::new_v4().simple());
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
        r#"CREATE TABLE "GameChallenges" (
             id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, "Type" SMALLINT NOT NULL,
             UNIQUE (game_id, id)
           );
           CREATE TABLE "KothApiObservers" (
             challenge_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
             hmac_secret TEXT NOT NULL
           );
           CREATE TABLE "KothApiObservationOperations" (
             challenge_id INTEGER NOT NULL, game_id INTEGER NOT NULL,
             request_digest BYTEA NOT NULL, signer_scope TEXT NOT NULL,
             body_digest BYTEA NOT NULL, context_hash CHAR(64) NOT NULL,
             lease_token UUID NOT NULL, lease_expires_at TIMESTAMPTZ NOT NULL,
             response JSONB NULL,
             created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             completed_at TIMESTAMPTZ NULL,
             expires_at TIMESTAMPTZ NOT NULL
               DEFAULT (clock_timestamp() + interval '10 minutes'),
             PRIMARY KEY (challenge_id, request_digest)
           );
           CREATE TABLE "KothApiSnapshots" (
             target_id INTEGER PRIMARY KEY, request_digest BYTEA NOT NULL
           );"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (9, 7, $1)"#)
        .bind(ChallengeType::KingOfTheHill as i16)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "KothApiObservers" VALUES (9, 7, 'observer-secret')"#)
        .execute(&pool)
        .await
        .unwrap();

    let first_body = [1_u8; 32];
    let second_body = [2_u8; 32];
    let first_digest = observation_request_digest(7, 9, "observer:7:9", &first_body);
    let second_digest = observation_request_digest(7, 9, "observer:7:9", &second_body);
    let first = reserve_observation(
        &pool,
        7,
        9,
        "observer:7:9",
        &"a".repeat(64),
        first_body,
        first_digest,
    )
    .await
    .unwrap();
    let ObservationReservationResult::Owner(first) = first else {
        panic!("the first distinct snapshot must reserve its own operation");
    };
    let second = reserve_observation(
        &pool,
        7,
        9,
        "observer:7:9",
        &"a".repeat(64),
        second_body,
        second_digest,
    )
    .await
    .unwrap();
    let ObservationReservationResult::Owner(second) = second else {
        panic!("the second distinct snapshot must have a distinct identity");
    };

    let mut winner = crate::utils::database::begin_sqlx_transaction(&pool)
        .await
        .unwrap();
    sqlx::query("SET LOCAL lock_timeout = '500ms'")
        .execute(&mut *winner)
        .await
        .unwrap();
    assert_eq!(
        lock_observer(&mut winner, 7, 9).await.unwrap().as_deref(),
        Some("observer-secret")
    );

    let contender_pool = pool.clone();
    let contender = tokio::spawn(async move {
        let mut transaction = crate::utils::database::begin_sqlx_transaction(&contender_pool)
            .await
            .unwrap();
        sqlx::query("SET LOCAL lock_timeout = '100ms'")
            .execute(&mut *transaction)
            .await
            .unwrap();
        let error = lock_observer(&mut transaction, 7, 9).await.unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        transaction.rollback().await.unwrap();
    });
    contender.await.unwrap();

    sqlx::query(r#"INSERT INTO "KothApiSnapshots" VALUES (3, $1)"#)
        .bind(first_digest.as_slice())
        .execute(&mut *winner)
        .await
        .unwrap();
    let accepted_at = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let response = KothObservationAcceptedModel {
        accepted: true,
        cycle_number: 4,
        reset_attempt: 1,
        round_number: 17,
        submitted_waves: 1,
        submitted_teams: 1,
        recognized_teams: 1,
        accepted_at,
    };
    sqlx::query(
        r#"UPDATE "KothApiObservationOperations"
              SET response = $4, completed_at = clock_timestamp()
            WHERE challenge_id = $1 AND request_digest = $2 AND lease_token = $3"#,
    )
    .bind(9_i32)
    .bind(first_digest.as_slice())
    .bind(first.lease_token)
    .bind(serde_json::to_value(response).unwrap())
    .execute(&mut *winner)
    .await
    .unwrap();
    winner.commit().await.unwrap();
    release_observation(&pool, 9, &second).await;

    let snapshots: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "KothApiSnapshots""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    let completed: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "KothApiObservationOperations" WHERE response IS NOT NULL"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((snapshots, completed), (1, 1));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
