//! Scoped job lookups that do not participate in admission or execution.

use uuid::Uuid;

use super::{database_error, ControlJobKind, ControlJobModel, JobRow, JOB_COLUMNS_QUALIFIED};
use crate::utils::error::AppResult;

pub async fn get_ad_reset_for_participation(
    pool: &sqlx::PgPool,
    game_id: i32,
    participation_id: i32,
    id: Option<Uuid>,
    operation_id: Option<Uuid>,
) -> AppResult<Option<ControlJobModel>> {
    let sql = format!(
        r#"SELECT {JOB_COLUMNS_QUALIFIED} FROM "ControlPlaneJobs" job
            WHERE job.kind = 'AdReset' AND job.game_id = $1
              AND (job.input->>'participationId')::integer = $2
              AND ($3::uuid IS NULL OR job.id = $3)
              AND ($4::uuid IS NULL OR EXISTS (
                  SELECT 1 FROM "ControlPlaneJobOperations" operation
                   WHERE operation.job_id = job.id
                     AND operation.operation_id = $4
              ))
            ORDER BY job.created_at_utc DESC, job.id DESC LIMIT 1"#
    );
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(game_id)
        .bind(participation_id)
        .bind(id)
        .bind(operation_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .map(TryInto::try_into)
        .transpose()
}

/// Most recent job of one kind for a game, regardless of its state. Terminal
/// history is bounded by the retention purge, so this stays a single-row read.
pub async fn latest_for_game(
    pool: &sqlx::PgPool,
    kind: ControlJobKind,
    game_id: i32,
) -> AppResult<Option<ControlJobModel>> {
    let sql = format!(
        r#"SELECT {JOB_COLUMNS_QUALIFIED} FROM "ControlPlaneJobs" job
            WHERE job.kind = $1 AND job.game_id = $2
            ORDER BY job.created_at_utc DESC, job.id DESC LIMIT 1"#
    );
    sqlx::query_as::<_, JobRow>(&sql)
        .bind(kind.as_str())
        .bind(game_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .map(TryInto::try_into)
        .transpose()
}
