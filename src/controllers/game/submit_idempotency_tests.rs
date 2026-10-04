use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{
    complete_attempt, find_completed_attempt, reserve_attempt, submission_request_fingerprint,
    AppError, AppResult, AttemptReservation, SubmissionReplay, Uuid,
};

struct Fixture {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_submit_attempt_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
            CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY);
            CREATE TABLE "Submissions" (
              id SERIAL PRIMARY KEY,
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              status SMALLINT NOT NULL
            );
            CREATE TABLE "SubmissionAttempts" (
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              attempt_id UUID NOT NULL,
              request_fingerprint BYTEA NOT NULL CHECK (OCTET_LENGTH(request_fingerprint) = 32),
              submission_id INTEGER NULL REFERENCES "Submissions"(id),
              created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              PRIMARY KEY (participation_id, challenge_id, attempt_id)
            );
            CREATE UNIQUE INDEX ux_submission_attempt_submission
              ON "SubmissionAttempts"(submission_id) WHERE submission_id IS NOT NULL;
            CREATE TABLE "SolveReceipts" (
              id UUID PRIMARY KEY,
              consumed_submission_id INTEGER NULL UNIQUE REFERENCES "Submissions"(id)
            );
            CREATE TABLE "FirstSolves" (
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              submission_id INTEGER NOT NULL UNIQUE REFERENCES "Submissions"(id),
              PRIMARY KEY (participation_id, challenge_id)
            );
            CREATE TABLE "GameNotices" (
              id SERIAL PRIMARY KEY,
              submission_id INTEGER NOT NULL UNIQUE REFERENCES "Submissions"(id)
            );
            CREATE TABLE "GameEvents" (
              id SERIAL PRIMARY KEY,
              submission_id INTEGER NOT NULL UNIQUE REFERENCES "Submissions"(id)
            );
            CREATE TABLE "SubmissionEvidence" (
              id SERIAL PRIMARY KEY,
              submission_id INTEGER NOT NULL UNIQUE REFERENCES "Submissions"(id)
            );
            INSERT INTO "Participations" VALUES (7);
            INSERT INTO "GameChallenges" VALUES (11), (12);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn close(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn grade_once(
    pool: &sqlx::PgPool,
    participation_id: i32,
    challenge_id: i32,
    attempt_id: Uuid,
    fingerprint: [u8; 32],
    status: i16,
    submission_limit: i64,
    receipt_id: Option<Uuid>,
) -> AppResult<SubmissionReplay> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(participation_id)
        .bind(challenge_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    match reserve_attempt(
        &mut transaction,
        participation_id,
        challenge_id,
        attempt_id,
        &fingerprint,
    )
    .await?
    {
        AttemptReservation::Replay(replay) => {
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(replay);
        }
        AttemptReservation::Fresh => {}
    }

    let attempts: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "Submissions"
            WHERE participation_id = $1 AND challenge_id = $2"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if attempts >= submission_limit {
        return Err(AppError::bad_request("Submission limit exceeded"));
    }

    if let Some(receipt_id) = receipt_id {
        let consumed: Option<i32> = sqlx::query_scalar(
            r#"SELECT consumed_submission_id FROM "SolveReceipts"
                WHERE id = $1 FOR UPDATE"#,
        )
        .bind(receipt_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if consumed.is_some() {
            return Err(AppError::bad_request("Solve receipt is already used"));
        }
    }

    let submission_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Submissions" (participation_id, challenge_id, status)
           VALUES ($1, $2, $3) RETURNING id"#,
    )
    .bind(participation_id)
    .bind(challenge_id)
    .bind(status)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if let Some(receipt_id) = receipt_id {
        sqlx::query(
            r#"UPDATE "SolveReceipts" SET consumed_submission_id = $2
                WHERE id = $1 AND consumed_submission_id IS NULL"#,
        )
        .bind(receipt_id)
        .bind(submission_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    // Every grade has one durable publication/evidence source. Accepted grades
    // additionally claim one solve and its one blood-notice source.
    sqlx::query(r#"INSERT INTO "GameEvents" (submission_id) VALUES ($1)"#)
        .bind(submission_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"INSERT INTO "SubmissionEvidence" (submission_id) VALUES ($1)"#)
        .bind(submission_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if status == 1 {
        let claimed = sqlx::query_scalar::<_, i32>(
            r#"INSERT INTO "FirstSolves"
                 (participation_id, challenge_id, submission_id)
               VALUES ($1, $2, $3)
               ON CONFLICT (participation_id, challenge_id) DO NOTHING
               RETURNING submission_id"#,
        )
        .bind(participation_id)
        .bind(challenge_id)
        .bind(submission_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if claimed.is_some() {
            sqlx::query(r#"INSERT INTO "GameNotices" (submission_id) VALUES ($1)"#)
                .bind(submission_id)
                .execute(&mut *transaction)
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }

    complete_attempt(
        &mut transaction,
        participation_id,
        challenge_id,
        attempt_id,
        &fingerprint,
        submission_id,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(SubmissionReplay {
        submission_id,
        status,
    })
}

async fn count(pool: &sqlx::PgPool, table: &str) -> i64 {
    assert!(matches!(
        table,
        "Submissions"
            | "SubmissionAttempts"
            | "FirstSolves"
            | "GameNotices"
            | "GameEvents"
            | "SubmissionEvidence"
    ));
    sqlx::query_scalar(&format!(r#"SELECT COUNT(*)::bigint FROM "{table}""#))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_lost_response_receipt_and_one_attempt_replay_every_effect_once() {
    let fixture = Fixture::new().await;
    let user = Uuid::from_u128(70);
    let attempt = Uuid::new_v4();
    let receipt = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "SolveReceipts" (id) VALUES ($1)"#)
        .bind(receipt)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let fingerprint = submission_request_fingerprint(user, "flag{accepted}", Some("one-use"));

    let first = grade_once(
        &fixture.pool,
        7,
        11,
        attempt,
        fingerprint,
        1,
        1,
        Some(receipt),
    );
    let duplicate = grade_once(
        &fixture.pool,
        7,
        11,
        attempt,
        fingerprint,
        1,
        1,
        Some(receipt),
    );
    let (first, duplicate) = tokio::join!(first, duplicate);
    let first = first.unwrap();
    let duplicate = duplicate.unwrap();
    assert_eq!(first, duplicate);

    // Treat the first successful commit as a response lost in transit. A later
    // exact POST-style replay and the direct recovery read both return it.
    let replay = grade_once(
        &fixture.pool,
        7,
        11,
        attempt,
        fingerprint,
        1,
        1,
        Some(receipt),
    )
    .await
    .unwrap();
    assert_eq!(replay, first);
    assert_eq!(
        find_completed_attempt(&fixture.pool, 7, 11, attempt, &fingerprint)
            .await
            .unwrap(),
        Some(first)
    );

    assert_eq!(count(&fixture.pool, "Submissions").await, 1);
    assert_eq!(count(&fixture.pool, "SubmissionAttempts").await, 1);
    assert_eq!(count(&fixture.pool, "FirstSolves").await, 1);
    assert_eq!(count(&fixture.pool, "GameNotices").await, 1);
    assert_eq!(count(&fixture.pool, "GameEvents").await, 1);
    assert_eq!(count(&fixture.pool, "SubmissionEvidence").await, 1);
    let consumed: Option<i32> =
        sqlx::query_scalar(r#"SELECT consumed_submission_id FROM "SolveReceipts" WHERE id = $1"#)
            .bind(receipt)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(consumed, Some(first.submission_id));

    let second_attempt = grade_once(
        &fixture.pool,
        7,
        11,
        Uuid::new_v4(),
        submission_request_fingerprint(user, "flag{other}", None),
        2,
        1,
        None,
    )
    .await
    .expect_err("a genuinely new semantic attempt must consume the one-attempt limit");
    assert_eq!(second_attempt.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(count(&fixture.pool, "Submissions").await, 1);

    fixture.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn wrong_answer_replays_and_mismatched_payload_reuse_is_rejected() {
    let fixture = Fixture::new().await;
    let user = Uuid::from_u128(71);
    let attempt = Uuid::new_v4();
    let wrong = submission_request_fingerprint(user, "flag{wrong}", None);
    let original = grade_once(&fixture.pool, 7, 12, attempt, wrong, 2, 1, None)
        .await
        .unwrap();
    let replay = grade_once(&fixture.pool, 7, 12, attempt, wrong, 2, 1, None)
        .await
        .unwrap();
    assert_eq!(original, replay);
    assert_eq!(original.status, 2);

    let mismatch = submission_request_fingerprint(user, "flag{different}", None);
    let error = grade_once(&fixture.pool, 7, 12, attempt, mismatch, 2, 1, None)
        .await
        .expect_err("one attempt UUID cannot be rebound to different content");
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);

    assert_eq!(count(&fixture.pool, "Submissions").await, 1);
    assert_eq!(count(&fixture.pool, "SubmissionAttempts").await, 1);
    assert_eq!(count(&fixture.pool, "FirstSolves").await, 0);
    assert_eq!(count(&fixture.pool, "GameNotices").await, 0);
    assert_eq!(count(&fixture.pool, "GameEvents").await, 1);
    assert_eq!(count(&fixture.pool, "SubmissionEvidence").await, 1);

    fixture.close().await;
}
