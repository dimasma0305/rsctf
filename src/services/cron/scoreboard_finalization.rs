//! Durable, bounded final-scoreboard closeout.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const CLAIM_BATCH_SIZE: i64 = 4;
const LEASE_SECONDS: i64 = 5 * 60;
const MATERIALIZATION_TIMEOUT_SECONDS: u64 = 4 * 60;
const MAX_ATTEMPTS: i32 = 16;
const RETRY_BASE_SECONDS: i64 = 60;
const RETRY_MAX_SECONDS: i64 = 60 * 60;

const CLAIM_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT finalization.game_id
      FROM "FinalScoreboardMaterializations" finalization
      JOIN "Games" game
        ON game.id = finalization.game_id
       AND game.end_time_utc = finalization.game_end_time_utc
       AND game.end_time_utc <= $1
       AND NOT game.practice_mode
     WHERE finalization.completed_at_utc IS NULL
       AND finalization.dead_at_utc IS NULL
       AND finalization.available_at_utc <= $1
       AND (finalization.lease_token IS NULL
            OR finalization.lease_expires_at_utc <= $1)
  ORDER BY finalization.available_at_utc, finalization.game_id
 FOR UPDATE OF finalization SKIP LOCKED
     LIMIT $4
)
UPDATE "FinalScoreboardMaterializations" finalization
   SET lease_token = $2,
       lease_expires_at_utc = $1 + ($3 * INTERVAL '1 second'),
       updated_at_utc = $1
  FROM candidates
 WHERE finalization.game_id = candidates.game_id
RETURNING finalization.game_id, finalization.game_end_time_utc,
          finalization.invalidated_at_utc, finalization.attempts
"#;

const RENEW_LEASE_SQL: &str = r#"
UPDATE "FinalScoreboardMaterializations"
   SET lease_expires_at_utc = $4 + ($5 * INTERVAL '1 second'),
       updated_at_utc = $4
 WHERE game_id = $1
   AND game_end_time_utc = $2
   AND lease_token = $3
   AND completed_at_utc IS NULL
   AND dead_at_utc IS NULL
   AND EXISTS (
       SELECT 1 FROM "Games" game
        WHERE game.id = $1
          AND game.end_time_utc = $2
          AND game.end_time_utc <= $4
          AND NOT game.practice_mode
   )
"#;

const RENEW_COMPLETION_LEASE_SQL: &str = r#"
UPDATE "FinalScoreboardMaterializations"
   SET lease_expires_at_utc = $4 + ($5 * INTERVAL '1 second'),
       updated_at_utc = $4
 WHERE game_id = $1
   AND game_end_time_utc = $2
   AND lease_token = $3
   AND invalidated_at_utc IS NOT NULL
   AND completed_at_utc IS NULL
   AND dead_at_utc IS NULL
"#;

const MARK_INVALIDATED_SQL: &str = r#"
UPDATE "FinalScoreboardMaterializations"
   SET invalidated_at_utc = $3,
       updated_at_utc = $3
 WHERE game_id = $1
   AND game_end_time_utc = $2
   AND lease_token = $4
   AND lease_expires_at_utc > $3
   AND completed_at_utc IS NULL
   AND dead_at_utc IS NULL
   AND invalidated_at_utc IS NULL
"#;

const OWNS_LEASE_SQL: &str = r#"
SELECT EXISTS (
    SELECT 1 FROM "FinalScoreboardMaterializations"
     WHERE game_id = $1
       AND game_end_time_utc = $2
       AND lease_token = $3
       AND lease_expires_at_utc > $4
       AND completed_at_utc IS NULL
       AND dead_at_utc IS NULL
)
"#;

const COMPLETE_SQL: &str = r#"
UPDATE "FinalScoreboardMaterializations" finalization
   SET completed_at_utc = $4,
       lease_token = NULL,
       lease_expires_at_utc = NULL,
       last_error = NULL,
       updated_at_utc = $4
 WHERE finalization.game_id = $1
   AND finalization.game_end_time_utc = $2
   AND finalization.lease_token = $3
   AND finalization.invalidated_at_utc IS NOT NULL
   AND finalization.completed_at_utc IS NULL
   AND finalization.dead_at_utc IS NULL
   AND EXISTS (
       SELECT 1 FROM "Games" game
        WHERE game.id = finalization.game_id
          AND game.end_time_utc = finalization.game_end_time_utc
          AND game.end_time_utc <= $4
   )
"#;

const RETRY_SQL: &str = r#"
UPDATE "FinalScoreboardMaterializations"
   SET attempts = $5,
       available_at_utc = $6,
       dead_at_utc = CASE WHEN $7 THEN $4 ELSE NULL END,
       lease_token = NULL,
       lease_expires_at_utc = NULL,
       last_error = $8,
       updated_at_utc = $4
 WHERE game_id = $1
   AND game_end_time_utc = $2
   AND lease_token = $3
   AND completed_at_utc IS NULL
   AND dead_at_utc IS NULL
"#;

const RELEASE_OBSOLETE_SQL: &str = r#"
UPDATE "FinalScoreboardMaterializations"
   SET lease_token = NULL,
       lease_expires_at_utc = NULL,
       updated_at_utc = $4
 WHERE game_id = $1
   AND game_end_time_utc = $2
   AND lease_token = $3
   AND completed_at_utc IS NULL
   AND dead_at_utc IS NULL
"#;

const REQUEST_REPAIR_SQL: &str = r#"
INSERT INTO "FinalScoreboardMaterializations"
       (game_id, game_end_time_utc, available_at_utc)
SELECT game.id, game.end_time_utc, clock_timestamp()
  FROM "Games" game
 WHERE game.id = $1
   AND game.end_time_utc <= clock_timestamp()
   AND NOT game.practice_mode
ON CONFLICT (game_id) DO UPDATE SET
       game_end_time_utc = EXCLUDED.game_end_time_utc,
       available_at_utc = EXCLUDED.available_at_utc,
       invalidated_at_utc = NULL,
       completed_at_utc = NULL,
       dead_at_utc = NULL,
       lease_token = CASE
           WHEN "FinalScoreboardMaterializations".lease_expires_at_utc
                > EXCLUDED.available_at_utc
           THEN "FinalScoreboardMaterializations".lease_token
           ELSE NULL
       END,
       lease_expires_at_utc = CASE
           WHEN "FinalScoreboardMaterializations".lease_expires_at_utc
                > EXCLUDED.available_at_utc
           THEN "FinalScoreboardMaterializations".lease_expires_at_utc
           ELSE NULL
       END,
       attempts = 0,
       last_error = NULL,
       updated_at_utc = EXCLUDED.available_at_utc
"#;

#[derive(Clone, Debug, sqlx::FromRow)]
struct ClaimedFinalization {
    game_id: i32,
    game_end_time_utc: DateTime<Utc>,
    invalidated_at_utc: Option<DateTime<Utc>>,
    attempts: i32,
}

struct ClaimedBatch {
    lease_token: Uuid,
    jobs: Vec<ClaimedFinalization>,
}

#[derive(Default)]
pub(super) struct FinalizationReport {
    pub(super) claimed: usize,
    pub(super) completed: usize,
    pub(super) retried: usize,
    pub(super) dead_lettered: usize,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

fn retry_delay(attempt: i32) -> chrono::Duration {
    let exponent = u32::try_from(attempt.saturating_sub(1).clamp(0, 6)).unwrap_or(6);
    let seconds = RETRY_BASE_SECONDS
        .saturating_mul(1_i64 << exponent)
        .min(RETRY_MAX_SECONDS);
    chrono::Duration::seconds(seconds)
}

fn bounded_error(error: impl std::fmt::Display) -> String {
    error.to_string().chars().take(256).collect()
}

async fn database_now(pool: &PgPool) -> AppResult<DateTime<Utc>> {
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(pool)
        .await
        .map_err(database_error)
}

async fn claim(pool: &PgPool, now: DateTime<Utc>) -> AppResult<ClaimedBatch> {
    let lease_token = Uuid::new_v4();
    let jobs = sqlx::query_as::<_, ClaimedFinalization>(CLAIM_SQL)
        .bind(now)
        .bind(lease_token)
        .bind(LEASE_SECONDS)
        .bind(CLAIM_BATCH_SIZE)
        .fetch_all(pool)
        .await
        .map_err(database_error)?;
    Ok(ClaimedBatch { lease_token, jobs })
}

async fn current_end(pool: &PgPool, game_id: i32) -> AppResult<Option<DateTime<Utc>>> {
    sqlx::query_scalar(r#"SELECT end_time_utc FROM "Games" WHERE id = $1"#)
        .bind(game_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)
}

async fn renew_lease(
    pool: &PgPool,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    sqlx::query(RENEW_LEASE_SQL)
        .bind(job.game_id)
        .bind(job.game_end_time_utc)
        .bind(lease_token)
        .bind(now)
        .bind(LEASE_SECONDS)
        .execute(pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(database_error)
}

async fn renew_completion_lease(
    pool: &PgPool,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    sqlx::query(RENEW_COMPLETION_LEASE_SQL)
        .bind(job.game_id)
        .bind(job.game_end_time_utc)
        .bind(lease_token)
        .bind(now)
        .bind(LEASE_SECONDS)
        .execute(pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(database_error)
}

async fn owns_lease(
    pool: &PgPool,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    sqlx::query_scalar(OWNS_LEASE_SQL)
        .bind(job.game_id)
        .bind(job.game_end_time_utc)
        .bind(lease_token)
        .bind(now)
        .fetch_one(pool)
        .await
        .map_err(database_error)
}

async fn mark_invalidated(
    pool: &PgPool,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    sqlx::query(MARK_INVALIDATED_SQL)
        .bind(job.game_id)
        .bind(job.game_end_time_utc)
        .bind(now)
        .bind(lease_token)
        .execute(pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(database_error)
}

async fn complete(
    pool: &PgPool,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    sqlx::query(COMPLETE_SQL)
        .bind(job.game_id)
        .bind(job.game_end_time_utc)
        .bind(lease_token)
        .bind(now)
        .execute(pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(database_error)
}

async fn retry(
    pool: &PgPool,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
    error: impl std::fmt::Display,
) -> AppResult<Option<bool>> {
    let attempts = job.attempts.saturating_add(1).min(MAX_ATTEMPTS);
    let dead = attempts >= MAX_ATTEMPTS;
    let available_at = now + retry_delay(attempts);
    let result = sqlx::query(RETRY_SQL)
        .bind(job.game_id)
        .bind(job.game_end_time_utc)
        .bind(lease_token)
        .bind(now)
        .bind(attempts)
        .bind(available_at)
        .bind(dead)
        .bind(bounded_error(error))
        .execute(pool)
        .await
        .map_err(database_error)?;
    Ok((result.rows_affected() == 1).then_some(dead))
}

async fn release_obsolete(
    pool: &PgPool,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(RELEASE_OBSOLETE_SQL)
        .bind(job.game_id)
        .bind(job.game_end_time_utc)
        .bind(lease_token)
        .bind(now)
        .execute(pool)
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn process_job(
    state: &SharedState,
    job: &ClaimedFinalization,
    lease_token: Uuid,
    now: DateTime<Utc>,
) -> AppResult<bool> {
    if !renew_lease(
        state.pg(),
        job,
        lease_token,
        database_now(state.pg()).await?,
    )
    .await?
    {
        return Ok(false);
    }
    if current_end(state.pg(), job.game_id).await? != Some(job.game_end_time_utc) {
        release_obsolete(state.pg(), job, lease_token, now).await?;
        return Ok(false);
    }

    if job.invalidated_at_utc.is_none() {
        crate::controllers::game::invalidate_scoreboard_render_version(state, job.game_id).await?;
        if !mark_invalidated(
            state.pg(),
            job,
            lease_token,
            database_now(state.pg()).await?,
        )
        .await?
        {
            return Ok(false);
        }
    } else if !owns_lease(
        state.pg(),
        job,
        lease_token,
        database_now(state.pg()).await?,
    )
    .await?
    {
        return Ok(false);
    }

    if !super::round_finish::refresh_score_rollups(state, job.game_id).await {
        return Err(AppError::internal("final score rollups are not settled"));
    }

    crate::controllers::game::invalidate_game_row_cache(job.game_id);
    let game = crate::controllers::game::load_game_cached(state, job.game_id).await?;
    let now = database_now(state.pg()).await?;
    if game.practice_mode || game.end_time_utc != job.game_end_time_utc || now < game.end_time_utc {
        release_obsolete(state.pg(), job, lease_token, now).await?;
        return Ok(false);
    }
    if !renew_lease(
        state.pg(),
        job,
        lease_token,
        database_now(state.pg()).await?,
    )
    .await?
    {
        return Ok(false);
    }
    let materialized = tokio::time::timeout(
        std::time::Duration::from_secs(MATERIALIZATION_TIMEOUT_SECONDS),
        crate::controllers::game::materialize_final_scoreboards(state, &game),
    )
    .await
    .map_err(|_| {
        AppError::internal("final scoreboard materialization exceeded its lease budget")
    })??;
    match materialized {
        true => {
            let now = database_now(state.pg()).await?;
            if renew_completion_lease(state.pg(), job, lease_token, now).await? {
                Ok(true)
            } else {
                release_obsolete(state.pg(), job, lease_token, now).await?;
                Ok(false)
            }
        }
        false => Err(AppError::internal(
            "final scoreboard evidence is not settled",
        )),
    }
}

pub(super) async fn materialize_pending(state: &SharedState) -> AppResult<FinalizationReport> {
    let now = database_now(state.pg()).await?;
    let batch = claim(state.pg(), now).await?;
    let mut report = FinalizationReport {
        claimed: batch.jobs.len(),
        ..FinalizationReport::default()
    };

    for job in batch.jobs {
        match process_job(state, &job, batch.lease_token, now).await {
            Ok(true) => {
                if complete(
                    state.pg(),
                    &job,
                    batch.lease_token,
                    database_now(state.pg()).await?,
                )
                .await?
                {
                    report.completed += 1;
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(game = job.game_id, %error, "final scoreboard materialization deferred");
                if let Some(dead) = retry(
                    state.pg(),
                    &job,
                    batch.lease_token,
                    database_now(state.pg()).await?,
                    &error,
                )
                .await?
                {
                    if dead {
                        report.dead_lettered += 1;
                    } else {
                        report.retried += 1;
                    }
                }
            }
        }
    }
    Ok(report)
}

/// Reset an ended game's durable closeout record. The manager-only cache flush
/// endpoint is the explicit repair path for exceptional stale final evidence.
pub(crate) async fn request_repair(pool: &PgPool, game_id: i32) -> AppResult<()> {
    sqlx::query(REQUEST_REPAIR_SQL)
        .bind(game_id)
        .execute(pool)
        .await
        .map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
#[path = "scoreboard_finalization/tests.rs"]
mod tests;
