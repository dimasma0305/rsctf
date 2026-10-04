use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn terminal_replay_bypasses_full_global_admission_but_new_work_fails_fast() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("flag_admission_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "FlagImportOperations" (
             challenge_id INTEGER NOT NULL,
             operation_id UUID NOT NULL,
             actor_user_id UUID NOT NULL,
             request_digest BYTEA NOT NULL,
             state SMALLINT NOT NULL DEFAULT 0,
             lease_token UUID NOT NULL,
             inserted_count INTEGER NULL,
             duplicate_count INTEGER NULL,
             lease_expires_at_utc TIMESTAMPTZ NOT NULL
               DEFAULT (clock_timestamp() + INTERVAL '5 minutes'),
             completed_at_utc TIMESTAMPTZ NULL,
             staged_attachment_ids INTEGER[] NOT NULL DEFAULT '{}',
             created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
             PRIMARY KEY (challenge_id, operation_id)
           );
           CREATE TABLE "FlagImportSlots" (
             slot_id SMALLINT PRIMARY KEY,
             lease_token UUID NULL,
             expires_at_utc TIMESTAMPTZ NULL
           );
           INSERT INTO "FlagImportSlots" (slot_id, lease_token, expires_at_utc)
           SELECT value, '00000000-0000-0000-0000-000000000001'::uuid,
                  clock_timestamp() + INTERVAL '1 hour'
             FROM generate_series(0, 3) value;"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let actor = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let digest = vec![7_u8; 32];
    sqlx::query(
        r#"INSERT INTO "FlagImportOperations"
             (challenge_id, operation_id, actor_user_id, request_digest, state,
              lease_token, inserted_count, duplicate_count, lease_expires_at_utc,
              completed_at_utc)
           VALUES (1, $1, $2, $3, 1,
                   '00000000-0000-0000-0000-000000000002'::uuid, 2, 1,
                   clock_timestamp(), clock_timestamp())"#,
    )
    .bind(operation_id)
    .bind(actor)
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap();

    let replay = reserve_flag_import(&pool, 1, actor, operation_id, &digest)
        .await
        .unwrap();
    assert!(matches!(
        replay,
        FlagImportReservation::Replayed(FlagImportResult {
            inserted: 2,
            duplicates: 1
        })
    ));
    let rejected = reserve_flag_import(&pool, 1, actor, Uuid::new_v4(), &digest)
        .await
        .unwrap_err();
    assert_eq!(rejected.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);

    sqlx::query(
        r#"UPDATE "FlagImportSlots"
              SET lease_token = NULL, expires_at_utc = NULL"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut admission_blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('rsctf:flag-import-admission', 0))")
        .execute(&mut *admission_blocker)
        .await
        .unwrap();
    let contention = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        reserve_flag_import(&pool, 1, actor, Uuid::new_v4(), &digest),
    )
    .await
    .expect("admission contention must not wait for the advisory lock")
    .unwrap_err();
    assert_eq!(
        contention.status(),
        axum::http::StatusCode::TOO_MANY_REQUESTS
    );
    admission_blocker.rollback().await.unwrap();

    let staged_operation = Uuid::new_v4();
    let staged_lease = match reserve_flag_import(&pool, 1, actor, staged_operation, &digest)
        .await
        .unwrap()
    {
        FlagImportReservation::Acquired { lease_token, .. } => lease_token,
        FlagImportReservation::Replayed(_) => panic!("new staging operation unexpectedly replayed"),
    };
    assert!(
        record_staged_flag_attachment(&pool, 1, staged_operation, staged_lease, 42)
            .await
            .unwrap()
    );
    let staged: Vec<i32> = sqlx::query_scalar(
        r#"SELECT staged_attachment_ids FROM "FlagImportOperations"
            WHERE challenge_id = 1 AND operation_id = $1"#,
    )
    .bind(staged_operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(staged, vec![42]);
    fail_staged_flag_import(&pool, 1, staged_operation, staged_lease, &[42]).await;
    let failed: (i16, Vec<i32>, bool) = sqlx::query_as(
        r#"SELECT state, staged_attachment_ids, completed_at_utc IS NOT NULL
             FROM "FlagImportOperations"
            WHERE challenge_id = 1 AND operation_id = $1"#,
    )
    .bind(staged_operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failed, (2, vec![42], true));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
