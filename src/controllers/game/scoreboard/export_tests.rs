use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use axum::body::to_bytes;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::*;
use crate::services::monitor_export::MonitorExportAdmission;
use crate::services::monitor_export::MAX_SUBMISSION_EXPORT_ROWS;

#[tokio::test(flavor = "current_thread")]
async fn workbook_builder_runs_outside_the_tokio_request_thread() {
    let request_thread = std::thread::current().id();
    let observed = Arc::new(AtomicBool::new(false));
    let task_observed = Arc::clone(&observed);
    let admission = MonitorExportAdmission::new();
    let mut permit = admission.try_begin().unwrap();
    permit.try_reserve_work(1, 1).unwrap();

    let bytes = build_xlsx_off_thread(
        (),
        permit,
        move |()| {
            task_observed.store(true, Ordering::Release);
            assert_ne!(std::thread::current().id(), request_thread);
            Ok(Vec::new())
        },
        "test workbook failed",
    )
    .await
    .unwrap();

    assert!(bytes.is_empty());
    assert!(observed.load(Ordering::Acquire));
}

#[tokio::test]
async fn cancelled_request_keeps_detached_workbook_task_admitted() {
    let admission = MonitorExportAdmission::new();
    let mut builder_permit = admission.try_begin().unwrap();
    builder_permit.try_reserve_work(1, 1).unwrap();
    let _other_slot = admission.try_begin().unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let request = tokio::spawn(build_xlsx_off_thread(
        (),
        builder_permit,
        move |()| {
            let _ = started_tx.send(());
            release_rx.recv().unwrap();
            Ok(Vec::new())
        },
        "test workbook failed",
    ));
    started_rx.await.unwrap();
    request.abort();
    let _ = request.await;

    assert!(matches!(
        admission.try_begin(),
        Err(MonitorExportAdmissionError::Busy)
    ));
    release_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if admission.try_begin().is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached workbook task did not release admission");
}

#[tokio::test]
async fn overload_responses_are_typed_and_retryable() {
    for (error, expected_status) in [
        (
            MonitorExportAdmissionError::Busy,
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            MonitorExportAdmissionError::WeightedCapacity,
            StatusCode::TOO_MANY_REQUESTS,
        ),
    ] {
        let response = export_overload_response(error);
        assert_eq!(response.status(), expected_status);
        assert_eq!(response.headers()[header::RETRY_AFTER], "3");
        let body = to_bytes(response.into_body(), 4 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["status"], expected_status.as_u16());
        assert_eq!(body["retryAfter"], EXPORT_RETRY_AFTER_SECONDS);
        assert!(body["title"].as_str().unwrap().contains("retry"));
    }
}

#[test]
fn submission_status_labels_preserve_the_existing_sheet_contract() {
    assert_eq!(
        answer_result_str(AnswerResult::NotFound as i16),
        "Not Found"
    );
    assert_eq!(
        answer_result_str(AnswerResult::FlagSubmitted as i16),
        "Submitted"
    );
    assert_eq!(answer_result_str(AnswerResult::Accepted as i16), "Accepted");
    assert_eq!(
        answer_result_str(AnswerResult::WrongAnswer as i16),
        "Wrong Answer"
    );
    assert_eq!(
        answer_result_str(AnswerResult::CheatDetected as i16),
        "Cheat Detected"
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn large_postgres_submission_export_is_paged_ordered_and_row_complete() {
    const ROWS: i32 = 5_123;

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("monitor_export_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .expect("connect isolated schema");

    sqlx::raw_sql(
        r#"
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
        CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, user_name TEXT);
        CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, title TEXT NOT NULL);
        CREATE TABLE "Submissions" (
          id INTEGER PRIMARY KEY,
          answer TEXT NOT NULL,
          status SMALLINT NOT NULL,
          submit_time_utc TIMESTAMPTZ NOT NULL,
          user_id UUID,
          team_id INTEGER NOT NULL,
          game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL
        );
        CREATE INDEX ix_submissions_game_time
          ON "Submissions"(game_id, submit_time_utc DESC, id DESC);
        INSERT INTO "Teams" VALUES (1, 'Export Team');
        INSERT INTO "GameChallenges" VALUES (1, 'Paged Challenge');
        "#,
    )
    .execute(&pool)
    .await
    .expect("create export fixture tables");
    let user_id = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, 'monitor-export-user')"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Submissions"
             (id, answer, status, submit_time_utc, user_id, team_id, game_id, challenge_id)
           SELECT value,
                  'flag{' || value::text || '}',
                  (value % 4)::smallint,
                  TIMESTAMPTZ '2026-01-01 00:00:00Z' + value * INTERVAL '1 millisecond',
                  $1, 1, 7, 1
             FROM generate_series(1, $2) value"#,
    )
    .bind(user_id)
    .bind(ROWS)
    .execute(&pool)
    .await
    .expect("seed large submission history");

    let admission = MonitorExportAdmission::new();
    let mut permit = admission.try_begin().unwrap();
    let rows = tokio::time::timeout(
        Duration::from_secs(10),
        load_submission_export_snapshot(&pool, 7, &mut permit),
    )
    .await
    .expect("bounded PostgreSQL export timed out")
    .expect("bounded PostgreSQL export failed");
    assert_eq!(rows.len(), ROWS as usize);
    assert_eq!(rows.first().unwrap().id, ROWS);
    assert_eq!(rows.last().unwrap().id, 1);
    assert_eq!(
        rows.iter().map(|row| row.id).collect::<HashSet<_>>().len(),
        ROWS as usize
    );
    assert!(rows
        .windows(2)
        .all(|pair| pair[0].submit_time_utc > pair[1].submit_time_utc));
    assert!(rows.iter().all(|row| {
        row.team_name.as_deref() == Some("Export Team")
            && row.user_name.as_deref() == Some("monitor-export-user")
            && row.challenge_title.as_deref() == Some("Paged Challenge")
    }));

    let workbook = tokio::time::timeout(
        Duration::from_secs(10),
        build_xlsx_off_thread(rows, permit, build_submission_xlsx, "test workbook failed"),
    )
    .await
    .expect("bounded XLSX build timed out")
    .expect("bounded XLSX build failed");
    let mut archive = zip::ZipArchive::new(Cursor::new(workbook)).unwrap();
    let mut worksheet = String::new();
    archive
        .by_name("xl/worksheets/sheet1.xml")
        .unwrap()
        .read_to_string(&mut worksheet)
        .unwrap();
    assert_eq!(worksheet.matches("<row ").count(), ROWS as usize + 1);
    assert!(worksheet.contains(&format!("A1:F{}", ROWS + 1)));

    sqlx::query(
        r#"INSERT INTO "Submissions"
             (id, answer, status, submit_time_utc, user_id, team_id, game_id, challenge_id)
           SELECT value,
                  'flag{' || value::text || '}',
                  (value % 4)::smallint,
                  TIMESTAMPTZ '2026-01-01 00:00:00Z' + value * INTERVAL '1 millisecond',
                  $1, 1, 7, 1
             FROM generate_series($2 + 1, $3) value"#,
    )
    .bind(user_id)
    .bind(ROWS)
    .bind(i32::try_from(MAX_SUBMISSION_EXPORT_ROWS + 1).unwrap())
    .execute(&pool)
    .await
    .expect("grow fixture past the explicit row bound");
    let mut over_limit_permit = admission.try_begin().unwrap();
    let over_limit = load_submission_export_snapshot(&pool, 7, &mut over_limit_permit)
        .await
        .expect_err("oversized submission history must be rejected before paging rows");
    assert!(matches!(
        over_limit,
        SubmissionSnapshotError::Application(AppError::PayloadTooLarge(_))
    ));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
