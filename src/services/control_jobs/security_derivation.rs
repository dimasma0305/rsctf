//! Generation-aware admission for anti-cheat derivation jobs.
//!
//! The reconciliation queue is the generation authority. Admission briefly
//! shares its game row, so a dirty trigger either commits before the captured
//! generation or waits until the job/operation alias is durable.

use std::time::Duration;

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{
    database_error, enqueue, kick, ClaimedControlJob, ControlJobKind, ControlJobModel, JobRow,
    SharedState, ADMISSION_BUSY_MESSAGE, JOB_COLUMNS, JOB_COLUMNS_QUALIFIED,
};
use crate::utils::error::{AppError, AppResult};

const REUSABLE_COMPLETED_GENERATION_FROM_SQL: &str = r#"
  FROM "ControlPlaneJobs" job
  JOIN "AntiCheatReconciliationQueue" queue ON queue.game_id = job.game_id
 WHERE job.kind = 'SecurityDerivation'
   AND job.game_id = $1 AND job.scope_key = $2 AND job.status = 2
   AND queue.desired_generation = $3
   AND queue.applied_generation = queue.desired_generation
   AND NOT EXISTS (
       SELECT 1 FROM "AntiCheatReconciliationSources" source
        WHERE source.game_id = queue.game_id
          AND source.dirty_version > source.applied_version
   )
   AND (job.result->>'reconciliationGeneration')::bigint
         = queue.applied_generation
 ORDER BY job.finished_at_utc DESC, job.id DESC
 LIMIT 1
 FOR SHARE OF job, queue
"#;

#[derive(Clone, Copy, Debug)]
pub(super) struct GenerationState {
    pub desired: i64,
    pub clean: bool,
}

pub(super) fn fingerprint(input: &Value) -> AppResult<String> {
    let encoded = serde_json::to_string(input)
        .map_err(|error| AppError::internal(format!("derivation fingerprint failed: {error}")))?;
    Ok(crate::utils::codec::sha256_str(&encoded))
}

pub(super) async fn bind_current_generation(
    transaction: &mut Transaction<'_, Postgres>,
    game_id: i32,
    input: &mut Value,
) -> AppResult<GenerationState> {
    let (desired, clean): (i64, bool) = sqlx::query_as(
        r#"SELECT queue.desired_generation,
                  queue.desired_generation = queue.applied_generation
                  AND NOT EXISTS (
                      SELECT 1 FROM "AntiCheatReconciliationSources" source
                       WHERE source.game_id = queue.game_id
                         AND source.dirty_version > source.applied_version
                  ) AS clean
             FROM "AntiCheatReconciliationQueue" queue
            WHERE queue.game_id = $1
            FOR SHARE OF queue"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("anti-cheat reconciliation queue not found"))?;
    let object = input
        .as_object_mut()
        .ok_or_else(|| AppError::internal("security derivation input must be an object"))?;
    object.insert("reconciliationGeneration".to_string(), Value::from(desired));
    Ok(GenerationState { desired, clean })
}

fn input_generation(input: &Value) -> Option<i64> {
    input
        .get("reconciliationGeneration")
        .and_then(Value::as_i64)
}

pub(super) async fn merge_active_generation(
    transaction: &mut Transaction<'_, Postgres>,
    row: JobRow,
    desired: i64,
    input: &Value,
    fingerprint: &str,
) -> AppResult<JobRow> {
    if input_generation(&row.input).is_some_and(|current| current >= desired) {
        return Ok(row);
    }
    let sql = format!(
        r#"UPDATE "ControlPlaneJobs"
              SET input = $2, fingerprint = $3,
                  input_revision = input_revision + 1,
                  updated_at_utc = clock_timestamp()
            WHERE id = $1 AND status IN (0, 1)
              AND input_revision < 1000000
        RETURNING {JOB_COLUMNS}"#
    );
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(row.id)
        .bind(input)
        .bind(fingerprint)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            AppError::overloaded(
                "Security derivation reached its coalesced-generation bound",
                2,
            )
        })
}

pub(super) async fn reusable_completed_generation(
    transaction: &mut Transaction<'_, Postgres>,
    game_id: i32,
    scope_key: &str,
    desired: i64,
) -> AppResult<Option<JobRow>> {
    let sql = format!("SELECT {JOB_COLUMNS_QUALIFIED} {REUSABLE_COMPLETED_GENERATION_FROM_SQL}");
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(game_id)
        .bind(scope_key)
        .bind(desired)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)
}

async fn enqueue_for_pool(
    pool: &sqlx::PgPool,
    game_id: i32,
    operation_id: Uuid,
) -> AppResult<ControlJobModel> {
    const RETRY_DELAYS: [Duration; 3] = [
        Duration::ZERO,
        Duration::from_millis(5),
        Duration::from_millis(15),
    ];
    let input = serde_json::json!({ "gameId": game_id });
    let base_fingerprint = fingerprint(&input)?;
    for (attempt, delay) in RETRY_DELAYS.into_iter().enumerate() {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        match enqueue(
            pool,
            ControlJobKind::SecurityDerivation,
            &format!("game:{game_id}"),
            game_id,
            None,
            operation_id,
            &base_fingerprint,
            input.clone(),
        )
        .await
        {
            Err(AppError::RetryableUnavailable { title, .. })
                if title == ADMISSION_BUSY_MESSAGE && attempt + 1 < RETRY_DELAYS.len() =>
            {
                continue;
            }
            result => return result,
        }
    }
    unreachable!("the bounded admission loop always returns on its final attempt")
}

pub async fn request(
    state: &SharedState,
    game_id: i32,
    operation_id: Uuid,
) -> AppResult<ControlJobModel> {
    let job = enqueue_for_pool(state.pg(), game_id, operation_id).await?;
    kick(state.clone());
    Ok(job)
}

pub(super) async fn applied_generation(pool: &sqlx::PgPool, game_id: i32) -> AppResult<i64> {
    sqlx::query_scalar(
        r#"SELECT applied_generation
             FROM "AntiCheatReconciliationQueue" WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("anti-cheat reconciliation queue not found"))
}

/// Keep the same externally visible job active while a captured source needs
/// another bounded cursor page. `complete` observes the revision fence and
/// returns the job to its durable queue after this execution relinquishes it.
pub(super) async fn continue_if_dirty(
    pool: &sqlx::PgPool,
    job: &ClaimedControlJob,
) -> AppResult<bool> {
    let continued = sqlx::query_scalar::<_, bool>(
        r#"UPDATE "ControlPlaneJobs" job
              SET input_revision = input_revision + 1,
                  updated_at_utc = clock_timestamp()
             FROM "AntiCheatReconciliationQueue" queue
            WHERE job.id = $1 AND job.status = 1 AND job.lease_token = $2
              AND job.input_revision = $3 AND job.input_revision < 1000000
              AND queue.game_id = $4
              AND (
                   queue.desired_generation > queue.applied_generation
                   OR (queue.final_requested_at_utc IS NOT NULL
                       AND queue.final_applied_at_utc IS NULL)
                   OR EXISTS (
                       SELECT 1 FROM "AntiCheatReconciliationSources" source
                        WHERE source.game_id = queue.game_id
                          AND source.dirty_version > source.applied_version
                   )
              )
        RETURNING TRUE"#,
    )
    .bind(job.model.id)
    .bind(job.lease_token)
    .bind(job.input_revision)
    .bind(job.model.game_id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .unwrap_or(false);
    Ok(continued)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_reuse_is_generation_bound_and_requires_a_clean_queue() {
        assert!(REUSABLE_COMPLETED_GENERATION_FROM_SQL
            .contains("queue.applied_generation = queue.desired_generation"));
        assert!(REUSABLE_COMPLETED_GENERATION_FROM_SQL
            .contains("source.dirty_version > source.applied_version"));
        assert!(REUSABLE_COMPLETED_GENERATION_FROM_SQL
            .contains("(job.result->>'reconciliationGeneration')::bigint"));
        assert!(REUSABLE_COMPLETED_GENERATION_FROM_SQL.contains("FOR SHARE OF job, queue"));
    }

    #[tokio::test]
    #[ignore = "requires migrated disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_two_operations_share_active_and_completed_generation() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .unwrap();
        let game_id: i32 = sqlx::query_scalar(r#"SELECT id FROM "Games" ORDER BY id LIMIT 1"#)
            .fetch_one(&pool)
            .await
            .expect("the disposable database needs one game");
        sqlx::query(
            r#"DELETE FROM "ControlPlaneJobs"
                WHERE kind = 'SecurityDerivation' AND game_id = $1"#,
        )
        .bind(game_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"UPDATE "AntiCheatReconciliationSources"
                  SET applied_version = dirty_version WHERE game_id = $1"#,
        )
        .bind(game_id)
        .execute(&pool)
        .await
        .unwrap();
        let generation: i64 = sqlx::query_scalar(
            r#"UPDATE "AntiCheatReconciliationQueue"
                  SET applied_generation = desired_generation,
                      lease_token = NULL, lease_expires_at_utc = NULL
                WHERE game_id = $1 RETURNING desired_generation"#,
        )
        .bind(game_id)
        .fetch_one(&pool)
        .await
        .unwrap();

        let first_operation = Uuid::new_v4();
        let second_operation = Uuid::new_v4();
        let (first, second) = tokio::join!(
            enqueue_for_pool(&pool, game_id, first_operation),
            enqueue_for_pool(&pool, game_id, second_operation),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.id, second.id);
        let aliases: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "ControlPlaneJobOperations" WHERE job_id = $1"#,
        )
        .bind(first.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(aliases, 2);

        sqlx::query(
            r#"UPDATE "ControlPlaneJobs"
                  SET status = 2, progress_current = progress_total,
                      result = jsonb_build_object(
                          'inserted', 0, 'reconciliationGeneration', $2::bigint
                      ),
                      finished_at_utc = clock_timestamp(),
                      updated_at_utc = clock_timestamp()
                WHERE id = $1"#,
        )
        .bind(first.id)
        .bind(generation)
        .execute(&pool)
        .await
        .unwrap();
        let third = enqueue_for_pool(&pool, game_id, Uuid::new_v4())
            .await
            .unwrap();
        assert_eq!(third.id, first.id);
        assert_eq!(third.status, super::super::ControlJobStatus::Succeeded);

        sqlx::query(r#"DELETE FROM "ControlPlaneJobs" WHERE id = $1"#)
            .bind(first.id)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
