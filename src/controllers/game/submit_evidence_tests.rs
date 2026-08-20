use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{
    claim_first_solve, grade_dynamic_answer, lock_game_timing_at_grade,
    lock_submit_caller_at_grade, AnswerResult, FINALIZE_SUBMISSION_SQL,
};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn foreign_flag_is_detected_without_an_own_instance() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_submit_evidence_{}", uuid::Uuid::new_v4().simple());
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
        r#"
        CREATE TABLE "FlagContexts" (
          id INTEGER PRIMARY KEY,
          challenge_id INTEGER NOT NULL,
          flag TEXT NOT NULL
        );
        CREATE TABLE "GameInstances" (
          id INTEGER PRIMARY KEY,
          challenge_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          flag_id INTEGER REFERENCES "FlagContexts"(id),
          UNIQUE (challenge_id, participation_id)
        );
        INSERT INTO "FlagContexts" (id, challenge_id, flag)
          VALUES (1, 30, 'flag{foreign}');
        INSERT INTO "GameInstances" (id, challenge_id, participation_id, flag_id)
          VALUES (1, 30, 20, 1);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    let (result, source) = grade_dynamic_answer(&mut transaction, 10, 30, "flag{foreign}", true)
        .await
        .unwrap();
    assert_eq!(result, AnswerResult::CheatDetected);
    assert_eq!(source, Some(20));

    let (result, source) = grade_dynamic_answer(&mut transaction, 10, 30, "flag{random}", true)
        .await
        .unwrap();
    assert_eq!(result, AnswerResult::WrongAnswer);
    assert_eq!(source, None);
    transaction.rollback().await.unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn repeated_accepted_submission_counts_one_canonical_solve() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_submit_replay_{}", uuid::Uuid::new_v4().simple());
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
        r#"
        CREATE TABLE "FirstSolves" (
          participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          submission_id INTEGER NOT NULL,
          PRIMARY KEY (participation_id, challenge_id)
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          is_enabled BOOLEAN NOT NULL,
          review_status SMALLINT NOT NULL,
          submission_limit INTEGER NOT NULL,
          deadline_utc TIMESTAMPTZ,
          disable_blood_bonus BOOLEAN NOT NULL,
          "Type" SMALLINT NOT NULL,
          shared_container_id UUID,
          solve_receipt_mode SMALLINT NOT NULL DEFAULT 0,
          variant_mode SMALLINT NOT NULL DEFAULT 0,
          submission_count INTEGER NOT NULL,
          accepted_count INTEGER NOT NULL
        );
        INSERT INTO "GameChallenges"
          (id, game_id, is_enabled, review_status, submission_limit,
           disable_blood_bonus, "Type", submission_count, accepted_count)
        VALUES (30, 1, TRUE, 0, 0, FALSE, 0, 0, 0);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    let first = claim_first_solve(&mut transaction, 10, 30, 100)
        .await
        .unwrap();
    assert!(first);
    let second = claim_first_solve(&mut transaction, 10, 30, 101)
        .await
        .unwrap();
    assert!(!second);

    for claimed in [first, second] {
        let affected = sqlx::query(FINALIZE_SUBMISSION_SQL)
            .bind(30)
            .bind(i32::from(claimed))
            .bind(1)
            .bind(0_i16)
            .bind(0)
            .bind(None::<chrono::DateTime<chrono::Utc>>)
            .bind(false)
            .bind(0_i16)
            .bind(None::<uuid::Uuid>)
            .bind(0_i16)
            .bind(0_i16)
            .execute(&mut *transaction)
            .await
            .unwrap();
        assert_eq!(affected.rows_affected(), 1);
    }
    transaction.commit().await.unwrap();

    let counters: (i32, i32) = sqlx::query_as(
        r#"SELECT submission_count, accepted_count
             FROM "GameChallenges" WHERE id = 30"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counters, (2, 1));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn submit_timestamp_is_assigned_after_the_finalization_barrier() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_submit_time_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL,
          practice_mode BOOLEAN NOT NULL,
          freeze_time_utc TIMESTAMPTZ
        );
        CREATE TABLE "SuspicionReconciliationState" (
          game_id INTEGER PRIMARY KEY,
          evidence_closed_at_utc TIMESTAMPTZ
        );
        INSERT INTO "Games"
          (id, start_time_utc, end_time_utc, practice_mode)
        VALUES
          (1, clock_timestamp() - INTERVAL '1 hour',
              clock_timestamp() + INTERVAL '1 hour', FALSE);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut finalizer = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR UPDATE"#)
        .execute(&mut *finalizer)
        .await
        .unwrap();

    let submit_pool = pool.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let queued_submit = async move {
        let mut transaction = submit_pool.begin().await.unwrap();
        started_tx.send(()).unwrap();
        let timing = lock_game_timing_at_grade(&mut transaction, 1)
            .await
            .unwrap()
            .unwrap();
        transaction.rollback().await.unwrap();
        timing.4
    };
    let release_barrier = async move {
        started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let released_at: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *finalizer)
                .await
                .unwrap();
        finalizer.commit().await.unwrap();
        released_at
    };
    let (submit_time, released_at) =
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(queued_submit, release_barrier)
        })
        .await
        .expect("queued submit resumes after the finalization barrier");
    assert!(submit_time >= released_at);

    // The opposite ordering is equally important: the finalizer must wait for
    // a producer that was admitted under the shared game fence before it scans.
    let mut admitted_producer = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR SHARE"#)
        .execute(&mut *admitted_producer)
        .await
        .unwrap();
    let barrier_pool = pool.clone();
    let (barrier_started_tx, barrier_started_rx) = tokio::sync::oneshot::channel();
    let final_barrier = async move {
        let mut transaction = barrier_pool.begin().await.unwrap();
        barrier_started_tx.send(()).unwrap();
        sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR UPDATE"#)
            .execute(&mut *transaction)
            .await
            .unwrap();
        let acquired_at: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        transaction.commit().await.unwrap();
        acquired_at
    };
    let finish_producer = async move {
        barrier_started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let committed_at: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut *admitted_producer)
                .await
                .unwrap();
        admitted_producer.commit().await.unwrap();
        committed_at
    };
    let (barrier_acquired_at, producer_committed_at) =
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(final_barrier, finish_producer)
        })
        .await
        .expect("finalization barrier drains an admitted evidence producer");
    assert!(barrier_acquired_at >= producer_committed_at);

    // Durable closure, rather than a later wall-clock reading, is the final
    // authority. Simulate a backward clock step by moving the end forward;
    // the immutable submission time is clamped to exact end and therefore
    // remains outside the strict competitive interval.
    let reopened_end: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        r#"UPDATE "Games"
              SET end_time_utc = clock_timestamp() + INTERVAL '1 hour'
            WHERE id = 1
        RETURNING end_time_utc"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "SuspicionReconciliationState"
               (game_id, evidence_closed_at_utc)
           VALUES (1, clock_timestamp())"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut closed_submit = pool.begin().await.unwrap();
    let closed_timing = lock_game_timing_at_grade(&mut closed_submit, 1)
        .await
        .unwrap()
        .unwrap();
    closed_submit.rollback().await.unwrap();
    assert_eq!(closed_timing.4, reopened_end);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn committed_membership_removal_wins_over_a_queued_submit() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_submit_membership_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    let captain = uuid::Uuid::from_u128(1);
    let caller = uuid::Uuid::from_u128(2);
    sqlx::raw_sql(
        r#"
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY, role SMALLINT NOT NULL,
          email_confirmed BOOLEAN NOT NULL, security_stamp TEXT
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          captain_id UUID NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL,
          PRIMARY KEY (team_id, user_id)
        );
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          PRIMARY KEY (user_id, game_id)
        );
        CREATE TABLE "IdentityObservations" (
          user_id UUID NOT NULL, game_id INTEGER,
          team_id INTEGER, participation_id INTEGER,
          observed_at_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Submissions" (id INTEGER PRIMARY KEY);
        CREATE TABLE "SuspicionEvaluationOutbox" (id BIGINT PRIMARY KEY);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, role, email_confirmed, security_stamp)
           VALUES ($1, 0, TRUE, 'captain-stamp'), ($2, 0, TRUE, 'caller-stamp')"#,
    )
    .bind(captain)
    .bind(caller)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id, captain_id) VALUES (7, $1)"#)
        .bind(captain)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Games" VALUES
           (11, FALSE, clock_timestamp() + interval '1 hour',
            clock_timestamp() + interval '2 hours')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (7, $1)"#)
        .bind(caller)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, game_id, team_id, status)
           VALUES (13, 11, 7, 1)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "UserParticipations"
             (user_id, game_id, team_id, participation_id)
           VALUES ($1, 11, 7, 13)"#,
    )
    .bind(caller)
    .execute(&pool)
    .await
    .unwrap();

    let mut removal = pool.begin().await.unwrap();
    crate::utils::single_flight::acquire_transaction_advisory_lock(&mut removal, "team-roster:7")
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "UserParticipations" WHERE user_id = $1"#)
        .bind(caller)
        .execute(&mut *removal)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "TeamMembers" WHERE user_id = $1"#)
        .bind(caller)
        .execute(&mut *removal)
        .await
        .unwrap();

    let submit_pool = pool.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let queued_submit = async move {
        let mut transaction = submit_pool.begin().await.unwrap();
        started_tx.send(()).unwrap();
        let allowed =
            lock_submit_caller_at_grade(&mut transaction, caller, "caller-stamp", 11, 7, 13)
                .await
                .unwrap();
        if allowed {
            sqlx::query(r#"INSERT INTO "Submissions" (id) VALUES (1)"#)
                .execute(&mut *transaction)
                .await
                .unwrap();
            sqlx::query(r#"INSERT INTO "SuspicionEvaluationOutbox" (id) VALUES (1)"#)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        transaction.commit().await.unwrap();
        allowed
    };
    let finish_removal = async move {
        started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        removal.commit().await.unwrap();
    };
    let (allowed, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(queued_submit, finish_removal)
    })
    .await
    .expect("queued submit resumes after membership removal");
    assert!(!allowed);
    let durable_rows: (i64, i64) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*)::bigint FROM "Submissions"),
                  (SELECT COUNT(*)::bigint FROM "SuspicionEvaluationOutbox")"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(durable_rows, (0, 0));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
