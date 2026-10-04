//! Durable per-challenge preflight outcomes.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use super::StepStatus;
use crate::utils::error::{AppError, AppResult};

const MAX_RESULT_ROWS: i64 = 2_000;

#[derive(Clone, Debug)]
pub(super) struct ResultRecord {
    pub challenge_id: i32,
    pub image: String,
    pub backend: &'static str,
    pub pull_status: StepStatus,
    pub start_status: StepStatus,
    pub duration_ms: i32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImagePreflightResultModel {
    pub challenge_id: i32,
    pub challenge_title: String,
    pub image: String,
    pub backend: String,
    pub pull_status: String,
    pub start_status: String,
    pub duration_ms: i32,
    pub error: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub updated_at_utc: DateTime<Utc>,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

/// Atomic bulk upsert; a challenge deleted mid-run simply drops its row via
/// the foreign key rather than failing the whole batch.
pub(super) async fn upsert_results(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    records: &[ResultRecord],
) -> AppResult<()> {
    if records.is_empty() {
        return Ok(());
    }
    let challenge_ids = records.iter().map(|r| r.challenge_id).collect::<Vec<_>>();
    let images = records.iter().map(|r| r.image.clone()).collect::<Vec<_>>();
    let backends = records
        .iter()
        .map(|r| r.backend.to_string())
        .collect::<Vec<_>>();
    let pulls = records
        .iter()
        .map(|r| r.pull_status.as_str().to_string())
        .collect::<Vec<_>>();
    let starts = records
        .iter()
        .map(|r| r.start_status.as_str().to_string())
        .collect::<Vec<_>>();
    let durations = records
        .iter()
        .map(|r| r.duration_ms.max(0))
        .collect::<Vec<_>>();
    let errors = records.iter().map(|r| r.error.clone()).collect::<Vec<_>>();
    sqlx::query(
        r#"INSERT INTO "ImagePreflightResults"
              (job_id, challenge_id, image, backend, pull_status, start_status,
               duration_ms, error, updated_at_utc)
            SELECT $1, row.challenge_id, row.image, row.backend, row.pull_status,
                   row.start_status, row.duration_ms, row.error, clock_timestamp()
              FROM UNNEST($2::integer[], $3::text[], $4::text[], $5::text[],
                          $6::text[], $7::integer[], $8::text[])
                   AS row(challenge_id, image, backend, pull_status, start_status,
                          duration_ms, error)
              JOIN "GameChallenges" challenge ON challenge.id = row.challenge_id
            ON CONFLICT (job_id, challenge_id) DO UPDATE
               SET image = EXCLUDED.image,
                   backend = EXCLUDED.backend,
                   pull_status = EXCLUDED.pull_status,
                   start_status = EXCLUDED.start_status,
                   duration_ms = EXCLUDED.duration_ms,
                   error = EXCLUDED.error,
                   updated_at_utc = clock_timestamp()"#,
    )
    .bind(job_id)
    .bind(&challenge_ids)
    .bind(&images)
    .bind(&backends)
    .bind(&pulls)
    .bind(&starts)
    .bind(&durations)
    .bind(&errors)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

/// Move a group's rows to `Running` once its task holds a concurrency permit.
pub(super) async fn mark_running(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    challenge_ids: &[i32],
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "ImagePreflightResults"
              SET pull_status = 'Running', updated_at_utc = clock_timestamp()
            WHERE job_id = $1 AND challenge_id = ANY($2) AND pull_status = 'Pending'"#,
    )
    .bind(job_id)
    .bind(challenge_ids)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

/// Close every row the coordinator never received an outcome for.
pub(super) async fn fail_unfinished(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    status: StepStatus,
    error: &str,
) -> AppResult<u64> {
    let affected = sqlx::query(
        r#"UPDATE "ImagePreflightResults"
              SET pull_status = CASE WHEN pull_status IN ('Pending', 'Running')
                                     THEN $2 ELSE pull_status END,
                  start_status = CASE WHEN start_status IN ('Pending', 'Running')
                                      THEN $2 ELSE start_status END,
                  error = COALESCE(error, $3),
                  updated_at_utc = clock_timestamp()
            WHERE job_id = $1
              AND (pull_status IN ('Pending', 'Running')
                   OR start_status IN ('Pending', 'Running'))"#,
    )
    .bind(job_id)
    .bind(status.as_str())
    .bind(error)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(affected)
}

pub async fn load_results(
    pool: &sqlx::PgPool,
    job_id: Uuid,
) -> AppResult<Vec<ImagePreflightResultModel>> {
    sqlx::query_as::<_, ImagePreflightResultModel>(
        r#"SELECT result.challenge_id, challenge.title AS challenge_title, result.image,
                  result.backend, result.pull_status, result.start_status,
                  result.duration_ms, result.error, result.updated_at_utc
             FROM "ImagePreflightResults" result
             JOIN "GameChallenges" challenge ON challenge.id = result.challenge_id
            WHERE result.job_id = $1
            ORDER BY result.challenge_id
            LIMIT $2"#,
    )
    .bind(job_id)
    .bind(MAX_RESULT_ROWS)
    .fetch_all(pool)
    .await
    .map_err(database_error)
}
