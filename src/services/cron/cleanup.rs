//! Independently budgeted, non-event-critical maintenance jobs.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::app_state::SharedState;
use crate::utils::error::AppResult;

const FILE_CLEANUP_BUDGET: Duration = Duration::from_secs(8);
const IMAGE_CLEANUP_BUDGET: Duration = Duration::from_secs(15);

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

    if let Some(result) = within_budget(
        "docker_image_cleanup",
        IMAGE_CLEANUP_BUDGET,
        crate::services::image_storage::scheduled_cleanup(state),
    )
    .await
    {
        match result {
            Ok(Some(report)) if report.images_removed > 0 || report.cache_bytes_reclaimed > 0 => {
                tracing::info!(
                    images = report.images_removed,
                    image_bytes = report.image_bytes_evicted,
                    cache_bytes = report.cache_bytes_reclaimed,
                    dangling_bytes = report.dangling_bytes_reclaimed,
                    free_before = report.available_bytes_before,
                    free_after = report.available_bytes_after,
                    pressure = report.pressure_mode,
                    "cron: completed bounded Docker storage cleanup"
                );
                for message in report.messages {
                    tracing::warn!(%message, "cron: Docker storage cleanup note");
                }
            }
            Ok(Some(report)) => {
                for message in report.messages {
                    tracing::warn!(%message, "cron: Docker storage cleanup note");
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "cron: Docker storage cleanup failed"),
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
}
