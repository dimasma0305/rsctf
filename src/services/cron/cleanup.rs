//! Independently budgeted, non-event-critical maintenance jobs.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::app_state::SharedState;
use crate::utils::error::AppResult;

const FILE_CLEANUP_BUDGET: Duration = Duration::from_secs(8);
const DATABASE_CLEANUP_BUDGET: Duration = Duration::from_secs(2);

const EPHEMERAL_OPERATION_PURGE_SQL: &str = r#"
WITH expired_credentials AS (
    SELECT operation_id
      FROM "PlayerCredentialOperations"
     WHERE expires_at <= clock_timestamp()
     ORDER BY expires_at
     LIMIT 128
), deleted_credentials AS (
    DELETE FROM "PlayerCredentialOperations" operation
     USING expired_credentials expired
     WHERE operation.operation_id = expired.operation_id
     RETURNING 1
), expired_observations AS (
    SELECT challenge_id, request_digest
      FROM "KothApiObservationOperations"
     WHERE expires_at <= clock_timestamp()
     ORDER BY expires_at
     LIMIT 128
), deleted_observations AS (
    DELETE FROM "KothApiObservationOperations" operation
     USING expired_observations expired
     WHERE operation.challenge_id = expired.challenge_id
       AND operation.request_digest = expired.request_digest
     RETURNING 1
)
SELECT (SELECT COUNT(*) FROM deleted_credentials)
     + (SELECT COUNT(*) FROM deleted_observations)
"#;

async fn purge_ephemeral_operations(state: &SharedState) -> AppResult<u64> {
    let deleted: i64 = sqlx::query_scalar(EPHEMERAL_OPERATION_PURGE_SQL)
        .fetch_one(state.pg())
        .await
        .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    Ok(deleted.max(0) as u64)
}

async fn within_budget<T>(
    job: &'static str,
    budget: Duration,
    work: impl Future<Output = AppResult<T>>,
) -> Option<AppResult<T>> {
    let started = Instant::now();
    match tokio::time::timeout(budget, work).await {
        Ok(result) => {
            tracing::debug!(
                job,
                duration_ms = started.elapsed().as_millis() as u64,
                succeeded = result.is_ok(),
                "cron: cleanup job finished within its independent budget"
            );
            Some(result)
        }
        Err(_) => {
            tracing::warn!(
                job,
                budget_ms = budget.as_millis() as u64,
                duration_ms = started.elapsed().as_millis() as u64,
                "cron: cleanup job reached its independent budget; continuing maintenance"
            );
            None
        }
    }
}

/// Run bounded cleanup after event-critical maintenance. Every job has its own
/// deadline, so a slow filesystem, object store, or Docker daemon cannot keep
/// later work from receiving a turn forever.
pub(super) async fn run(state: &SharedState) {
    if let Some(result) = within_budget(
        "ephemeral_credential_operations",
        DATABASE_CLEANUP_BUDGET,
        purge_ephemeral_operations(state),
    )
    .await
    {
        match result {
            Ok(n) if n > 0 => tracing::info!(n, "cron: purged expired credential operation(s)"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cron: credential operation purge failed"),
        }
    }

    if let Some(result) = within_budget(
        "traffic_capture_retention",
        FILE_CLEANUP_BUDGET,
        crate::services::traffic::purge_expired_captures(state, 128),
    )
    .await
    {
        match result {
            Ok(n) if n > 0 => {
                tracing::info!(n, "cron: purged expired traffic capture tree(s)")
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cron: traffic capture retention sweep failed"),
        }
    }

    if let Some(result) = within_budget(
        "ad_snapshot_retention",
        FILE_CLEANUP_BUDGET,
        crate::services::blob_refs::purge_expired_service_snapshots(
            state.pg(),
            state.storage.as_ref(),
            128,
        ),
    )
    .await
    {
        match result {
            Ok(n) if n > 0 => tracing::info!(n, "cron: purged expired A&D service snapshot(s)"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cron: A&D snapshot retention sweep failed"),
        }
    }

    if let Some(result) = within_budget(
        "deferred_blob_purge",
        FILE_CLEANUP_BUDGET,
        crate::services::blob_refs::purge_pending(state.pg(), state.storage.as_ref(), 128),
    )
    .await
    {
        match result {
            Ok(n) if n > 0 => tracing::info!(n, "cron: purged deferred blob tombstone(s)"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cron: deferred blob purge failed"),
        }
    }

    if let Some(result) = within_budget(
        "checker_revision_gc",
        FILE_CLEANUP_BUDGET,
        crate::services::git_sync::collect_stale_checker_revisions(state),
    )
    .await
    {
        match result {
            Ok(n) if n > 0 => tracing::info!(n, "cron: collected stale checker revision(s)"),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cron: checker revision GC failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_timed_out_job_cannot_hold_the_cleanup_chain() {
        let started = Instant::now();
        let result = within_budget("hung-test-job", Duration::from_millis(20), async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok::<_, crate::utils::error::AppError>(())
        })
        .await;
        assert!(result.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn ephemeral_secret_cleanup_is_index_ordered_and_bounded() {
        assert!(EPHEMERAL_OPERATION_PURGE_SQL.contains("ORDER BY expires_at"));
        assert_eq!(
            EPHEMERAL_OPERATION_PURGE_SQL.matches("LIMIT 128").count(),
            2
        );
        assert!(EPHEMERAL_OPERATION_PURGE_SQL.contains("deleted_credentials"));
        assert!(EPHEMERAL_OPERATION_PURGE_SQL.contains("deleted_observations"));
    }

    #[test]
    fn latency_sensitive_cleanup_chain_does_not_run_image_maintenance() {
        let source = include_str!("cleanup.rs");
        assert!(!source.contains("image_storage::scheduled_cleanup"));
    }
}
