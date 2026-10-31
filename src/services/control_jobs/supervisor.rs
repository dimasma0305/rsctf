//! Supervised, independently admitted control-job lanes.
//!
//! HTTP handlers only signal intent. All work is durable in PostgreSQL, so a
//! lost process-local notification is repaired by the owner's periodic poll.

use tokio::{sync::watch, task::JoinSet};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lane {
    Gameplay,
    Build,
    Maintenance,
}

impl Lane {
    pub(super) const ALL: [Self; 3] = [Self::Gameplay, Self::Build, Self::Maintenance];

    pub(super) const fn kinds(self) -> &'static [ControlJobKind] {
        match self {
            Self::Gameplay => &[ControlJobKind::AdReset, ControlJobKind::AdReconcile],
            Self::Build => &[
                ControlJobKind::ChallengeBuild,
                ControlJobKind::BuildBatch,
                ControlJobKind::WorkloadRollout,
                ControlJobKind::ImagePreflight,
            ],
            Self::Maintenance => &[
                ControlJobKind::CompetitionResume,
                ControlJobKind::VariantGeneration,
                ControlJobKind::SecurityDerivation,
            ],
        }
    }

    pub(super) const fn for_kind(kind: ControlJobKind) -> Self {
        match kind {
            ControlJobKind::AdReset | ControlJobKind::AdReconcile => Self::Gameplay,
            ControlJobKind::ChallengeBuild
            | ControlJobKind::BuildBatch
            | ControlJobKind::WorkloadRollout
            | ControlJobKind::ImagePreflight => Self::Build,
            ControlJobKind::VariantGeneration
            | ControlJobKind::SecurityDerivation
            | ControlJobKind::CompetitionResume => Self::Maintenance,
        }
    }

    pub(super) const fn slots(self) -> i64 {
        match self {
            Self::Gameplay | Self::Build => 2,
            Self::Maintenance => 1,
        }
    }

    pub(super) const fn lock_key(self) -> i64 {
        0x5253_4354_464C_4E00 + self as i64
    }
}

pub fn kick(state: SharedState) {
    state.control_job_wakeup.notify_waiters();
}

pub fn start(state: SharedState, shutdown: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut workers = JoinSet::new();
        for lane in Lane::ALL {
            for _ in 0..lane.slots() {
                workers.spawn(run_lane(state.clone(), shutdown.clone(), lane));
            }
        }
        crate::services::supervision::join_required_workers(workers, shutdown, "control-job").await;
    })
}

async fn run_lane(state: SharedState, mut shutdown: watch::Receiver<bool>, lane: Lane) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        let claimed = match claim_matching(
            state.pg(),
            lane,
            lane.kinds(),
            Duration::from_secs(15 * 60),
        )
        .await
        {
            Ok(job) => job,
            Err(error) => {
                tracing::warn!(?lane, %error, "control-job claim failed");
                None
            }
        };
        if let Some(job) = claimed {
            if *shutdown.borrow() {
                // No application code has run yet. Only this unstarted claim
                // may be released immediately; fence it against a newer owner.
                if let Err(error) = release_unstarted(state.pg(), &job).await {
                    tracing::warn!(%error, "unstarted control-job claim retained for recovery");
                }
                return;
            }
            // Complete already-claimed work during graceful drain. If the
            // process drain deadline aborts us, retain its durable lease: an
            // interrupted external operation must never be released early.
            if let Err(error) = execute_one(&state, job).await {
                tracing::warn!(?lane, %error, "control-job completion failed; lease retained");
            }
            continue;
        }
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = state.control_job_wakeup.notified() => {},
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

async fn release_unstarted(pool: &sqlx::PgPool, job: &ClaimedControlJob) -> AppResult<bool> {
    let changed = sqlx::query(
        r#"UPDATE "ControlPlaneJobs"
              SET status = CASE WHEN cancel_requested_at_utc IS NULL THEN 0 ELSE 4 END,
                  finished_at_utc = CASE WHEN cancel_requested_at_utc IS NULL
                                        THEN NULL ELSE clock_timestamp() END,
                  lease_token = NULL, lease_expires_at_utc = NULL,
                  updated_at_utc = clock_timestamp()
            WHERE id = $1 AND status = 1 AND lease_token = $2"#,
    )
    .bind(job.model.id)
    .bind(job.lease_token)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(changed == 1)
}

async fn execute_one(state: &StateHandle, job: ClaimedControlJob) -> AppResult<()> {
    use crate::services::observability as metrics;
    use std::sync::atomic::Ordering::Relaxed;
    let id = job.model.id;
    let lease_token = job.lease_token;
    if !set_progress(state.pg(), id, lease_token, 0, 1).await? {
        return Ok(());
    }
    metrics::CONTROL_JOB_STARTED.fetch_add(1, Relaxed);
    metrics::CONTROL_JOB_QUEUE_DELAY.observe(
        (chrono::Utc::now() - job.model.created_at_utc)
            .to_std()
            .unwrap_or_default(),
    );
    let _timer = metrics::CONTROL_JOB_EXECUTION.timer();
    let execution = tokio::time::timeout(
        Duration::from_secs(CONTROL_JOB_EXECUTION_BUDGET_SECONDS),
        execution::execute_claimed(state, &job),
    )
    .await;
    match execution {
        Ok(Ok(result)) => {
            if complete(state.pg(), id, lease_token, job.input_revision, result).await? {
                metrics::CONTROL_JOB_SUCCEEDED.fetch_add(1, Relaxed);
            }
        }
        Ok(Err(error)) => {
            metrics::CONTROL_JOB_FAILED.fetch_add(1, Relaxed);
            fail(
                state.pg(),
                id,
                lease_token,
                job.input_revision,
                &error.to_string(),
            )
            .await?;
        }
        Err(_) => {
            metrics::CONTROL_JOB_FAILED.fetch_add(1, Relaxed);
            fail(
                state.pg(),
                id,
                lease_token,
                job.input_revision,
                "Control-plane job exceeded its total deadline",
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "supervisor_pg_tests.rs"]
mod pg_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_cannot_consume_gameplay_or_maintenance_admission() {
        for lane in Lane::ALL {
            for kind in lane.kinds() {
                assert_eq!(Lane::for_kind(*kind), lane);
                for other in Lane::ALL.into_iter().filter(|other| *other != lane) {
                    assert_ne!(lane.lock_key(), other.lock_key());
                    assert!(!other.kinds().contains(kind));
                }
            }
        }
    }
}
