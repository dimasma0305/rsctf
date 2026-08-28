use std::time::Duration;

use uuid::Uuid;

use super::{get_by_operation, ControlJobKind, ControlJobModel};
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
        if job.kind != kind.as_str() || job.scope_key != scope_key || job.fingerprint != fingerprint
        {
            return Err(AppError::conflict(
                "Idempotency-Key was already used for a different operation",
            ));
        }
        return Ok(Some(job));
    }
    Ok(None)
}
