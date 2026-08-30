use sqlx::PgPool;
use uuid::Uuid;

use super::{get, ControlJobModel, ControlJobStatus};
use crate::utils::error::{AppError, AppResult};

const CANCELLABLE_RUNNING_KINDS: &str =
    "'BuildBatch', 'VariantGeneration', 'WorkloadRollout', 'AdReconcile'";

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

/// Cancel queued work immediately, or request cancellation at the next safe
/// batch boundary for workflows that can stop without leaving a runtime half
/// replaced. A running image build or service reset is deliberately fenced to
/// completion instead of pretending its external side effects were cancelled.
pub async fn request(pool: &PgPool, id: Uuid) -> AppResult<ControlJobModel> {
    let sql = format!(
        r#"UPDATE "ControlPlaneJobs"
              SET cancel_requested_at_utc = COALESCE(cancel_requested_at_utc, clock_timestamp()),
                  status = CASE WHEN status = 0 THEN 4 ELSE status END,
                  lease_token = CASE WHEN status = 0 THEN NULL ELSE lease_token END,
                  lease_expires_at_utc = CASE WHEN status = 0 THEN NULL ELSE lease_expires_at_utc END,
                  finished_at_utc = CASE WHEN status = 0 THEN clock_timestamp() ELSE finished_at_utc END,
                  updated_at_utc = clock_timestamp()
            WHERE id = $1
              AND (status = 0 OR (status = 1 AND kind IN ({CANCELLABLE_RUNNING_KINDS})))
        RETURNING id"#
    );
    let accepted = sqlx::query_scalar::<_, Uuid>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .is_some();
    let job = get(pool, id)
        .await?
        .ok_or_else(|| AppError::not_found("Control job not found"))?;
    if accepted || matches!(job.status, ControlJobStatus::Cancelled) {
        return Ok(job);
    }
    if matches!(
        job.status,
        ControlJobStatus::Succeeded | ControlJobStatus::Failed
    ) {
        return Ok(job);
    }
    Err(AppError::conflict(
        "This control job has crossed an external side-effect boundary and cannot be cancelled safely",
    ))
}

pub async fn requested(pool: &PgPool, id: Uuid, lease_token: Uuid) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1 FROM "ControlPlaneJobs"
                WHERE id = $1 AND status = 1 AND lease_token = $2
                  AND cancel_requested_at_utc IS NOT NULL
           )"#,
    )
    .bind(id)
    .bind(lease_token)
    .fetch_one(pool)
    .await
    .map_err(database_error)
}

pub async fn finish(pool: &PgPool, id: Uuid, lease_token: Uuid) -> AppResult<bool> {
    let affected = sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET status = 4, result = NULL, error = NULL,
                  lease_token = NULL, lease_expires_at_utc = NULL,
                  updated_at_utc = clock_timestamp(),
                  finished_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2
              AND cancel_requested_at_utc IS NOT NULL"#,
    )
    .bind(id)
    .bind(lease_token)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(affected == 1)
}

#[cfg(test)]
mod tests {
    use super::CANCELLABLE_RUNNING_KINDS;

    #[test]
    fn destructive_single_resource_jobs_are_not_interrupted_mid_effect() {
        assert!(CANCELLABLE_RUNNING_KINDS.contains("BuildBatch"));
        assert!(CANCELLABLE_RUNNING_KINDS.contains("VariantGeneration"));
        assert!(!CANCELLABLE_RUNNING_KINDS.contains("ChallengeBuild"));
        assert!(!CANCELLABLE_RUNNING_KINDS.contains("AdReset"));
    }
}
