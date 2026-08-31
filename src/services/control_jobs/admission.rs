use std::time::Duration;

use uuid::Uuid;

use super::{
    database_error, get_by_operation, ControlJobKind, ControlJobModel,
    MAX_OPERATION_ALIASES_PER_JOB,
};
use crate::utils::error::{AppError, AppResult};

/// A simultaneous exact retry can observe the short deployment-wide admission
/// fence before the winning transaction commits. Recover it without retaining
/// a pool connection or joining an unbounded advisory-lock waiter queue.
pub(super) async fn recover_exact_after_busy(
    pool: &sqlx::PgPool,
    kind: ControlJobKind,
    scope_key: &str,
    operation_id: Uuid,
    fingerprint: &str,
) -> AppResult<Option<ControlJobModel>> {
    const RETRY_DELAYS: [Duration; 4] = [
        Duration::ZERO,
        Duration::from_millis(5),
        Duration::from_millis(10),
        Duration::from_millis(20),
    ];
    for delay in RETRY_DELAYS {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        } else {
            tokio::task::yield_now().await;
        }
        let Some(job) = get_by_operation(pool, operation_id).await? else {
            continue;
        };
        let same_request = job.kind == kind.as_str()
            && job.scope_key == scope_key
            && (job.fingerprint == fingerprint || kind == ControlJobKind::SecurityDerivation);
        if !same_request {
            return Err(AppError::conflict(
                "Idempotency-Key was already used for a different operation",
            ));
        }
        return Ok(Some(job));
    }
    Ok(None)
}

pub(super) async fn attach_operation_alias(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job_id: Uuid,
    operation_id: Uuid,
) -> AppResult<()> {
    let aliases: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "ControlPlaneJobOperations" WHERE job_id = $1"#)
            .bind(job_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(database_error)?;
    if aliases >= MAX_OPERATION_ALIASES_PER_JOB {
        return Err(AppError::overloaded(
            "This control-plane job has reached its retry-alias bound",
            2,
        ));
    }
    sqlx::query(
        r#"INSERT INTO "ControlPlaneJobOperations" (operation_id, job_id)
            VALUES ($1, $2)"#,
    )
    .bind(operation_id)
    .bind(job_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}
