use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn rejects_duplicate_and_oversized_intents_before_reservation() {
    assert!(BULK_DELETE_STEP_BUDGET < std::time::Duration::from_secs(5 * 60));
    let mut duplicate = BulkChallengeMutationRequest {
        operation_id: Uuid::new_v4(),
        expected_revision: 1,
        action: BulkChallengeAction::Enable,
        challenge_ids: vec![9, 9],
    };
    assert_eq!(
        validate_request(&mut duplicate).unwrap_err().status(),
        axum::http::StatusCode::BAD_REQUEST
    );
    let mut oversized = BulkChallengeMutationRequest {
        operation_id: Uuid::new_v4(),
        expected_revision: 1,
        action: BulkChallengeAction::Delete,
        challenge_ids: (1..=101).collect(),
    };
    assert_eq!(
        validate_request(&mut oversized).unwrap_err().status(),
        axum::http::StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn delete_dispatch_has_no_unbounded_waiter_queue() {
    let first = BULK_DELETE_SLOTS.clone().try_acquire_owned().unwrap();
    let second = BULK_DELETE_SLOTS.clone().try_acquire_owned().unwrap();
    assert!(BULK_DELETE_SLOTS.clone().try_acquire_owned().is_err());
    assert_eq!(BULK_DELETE_SLOTS.available_permits(), 0);
    drop((first, second));
    assert_eq!(
        BULK_DELETE_SLOTS.available_permits(),
        BULK_DELETE_CONCURRENCY
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn expired_desired_state_operation_has_one_replica_safe_lease_owner() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("bulk_claim_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let first_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    let second_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "BulkChallengeMutationOperations" (
             game_id INTEGER NOT NULL,
             operation_id UUID NOT NULL,
             action SMALLINT NOT NULL,
             state SMALLINT NOT NULL,
             lease_token UUID NULL,
             lease_expires_at_utc TIMESTAMPTZ NOT NULL,
             PRIMARY KEY (game_id, operation_id)
           );"#,
    )
    .execute(&first_pool)
    .await
    .unwrap();
    let operation_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "BulkChallengeMutationOperations"
             (game_id, operation_id, action, state, lease_expires_at_utc)
           VALUES (1, $1, 0, 0, clock_timestamp() - INTERVAL '1 second')"#,
    )
    .bind(operation_id)
    .execute(&first_pool)
    .await
    .unwrap();

    let first =
        claim_desired_state_operation(&first_pool, 1, operation_id, BulkChallengeAction::Enable);
    let second =
        claim_desired_state_operation(&second_pool, 1, operation_id, BulkChallengeAction::Enable);
    let (first, second) = tokio::join!(first, second);
    let owners = [first.unwrap(), second.unwrap()]
        .into_iter()
        .flatten()
        .count();
    assert_eq!(owners, 1);

    first_pool.close().await;
    second_pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
