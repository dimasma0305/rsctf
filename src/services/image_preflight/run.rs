//! Job execution: bounded pull/start concurrency, per-image deadlines, and
//! cleanup that survives timeouts, task failure, and coordinator abandonment.
//!
//! Every temporary instance carries an operation id so the local Docker
//! backend labels it (`rsctf.operation`) and can locate it after a dropped
//! create request. Instances never receive a `Containers` row, so the existing
//! orphan sweep removes any survivor once its grace period passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use uuid::Uuid;

use super::plan::{Launch, PreflightTarget, PREFLIGHT_FLAG};
use super::store::{self, ResultRecord};
use super::{bounded_error, PreflightSummary, StepStatus};
use crate::app_state::SharedState;
use crate::services::container::ContainerLiveness;
use crate::services::control_jobs::ClaimedControlJob;
use crate::utils::error::{AppError, AppResult};

/// At most this many images are pulled or started at once per replica.
const IMAGE_CONCURRENCY: usize = 2;
const PULL_DEADLINE: Duration = Duration::from_secs(5 * 60);
const START_DEADLINE: Duration = Duration::from_secs(120);
const DESTROY_DEADLINE: Duration = Duration::from_secs(60);
/// A local container must still be running this long after start; the
/// trusted-worker backend already waits for its `Ready` observation.
const LIVENESS_GRACE: Duration = Duration::from_secs(6);
const LIVENESS_POLL: Duration = Duration::from_secs(1);
/// Stays below the 14-minute control-job budget so the coordinator, not the
/// budget timeout, closes every row.
const JOB_DEADLINE: Duration = Duration::from_secs(12 * 60);
const JOB_DEADLINE_ERROR: &str = "The preflight reached its deadline before this image finished";
const CANCELLED_ERROR: &str = "The preflight was cancelled before this image ran";

#[derive(Clone, Debug)]
struct ImageGroup {
    image: String,
    backend: &'static str,
    is_worker: bool,
    launch: Launch,
    challenge_ids: Vec<i32>,
}

#[derive(Clone, Debug)]
struct GroupOutcome {
    pull: StepStatus,
    start: StepStatus,
    duration_ms: i32,
    error: Option<String>,
}

/// One temporary instance per distinct launch identity, in challenge order.
fn group_targets(targets: &[PreflightTarget]) -> Vec<ImageGroup> {
    let mut groups: Vec<(String, ImageGroup)> = Vec::new();
    for target in targets {
        let Some(launch) = target.launch.as_ref() else {
            continue;
        };
        if let Some((_, group)) = groups.iter_mut().find(|(key, _)| *key == target.group_key) {
            group.challenge_ids.push(target.challenge_id);
            continue;
        }
        groups.push((
            target.group_key.clone(),
            ImageGroup {
                image: target.image.clone(),
                backend: target.backend.as_str(),
                is_worker: target.backend.is_worker(),
                launch: launch.clone(),
                challenge_ids: vec![target.challenge_id],
            },
        ));
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

fn initial_records(targets: &[PreflightTarget]) -> Vec<ResultRecord> {
    targets
        .iter()
        .map(|target| {
            let skipped = target.skip_reason.is_some();
            ResultRecord {
                challenge_id: target.challenge_id,
                image: target.image.clone(),
                backend: target.backend.as_str(),
                pull_status: if skipped {
                    StepStatus::Skipped
                } else {
                    StepStatus::Pending
                },
                start_status: if skipped {
                    StepStatus::Skipped
                } else {
                    StepStatus::Pending
                },
                duration_ms: 0,
                error: target.skip_reason.clone(),
            }
        })
        .collect()
}

fn outcome_records(group: &ImageGroup, outcome: &GroupOutcome) -> Vec<ResultRecord> {
    group
        .challenge_ids
        .iter()
        .map(|challenge_id| ResultRecord {
            challenge_id: *challenge_id,
            image: group.image.clone(),
            backend: group.backend,
            pull_status: outcome.pull,
            start_status: outcome.start,
            duration_ms: outcome.duration_ms,
            error: outcome.error.clone(),
        })
        .collect()
}

fn elapsed_ms(started: Instant) -> i32 {
    i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)
}

/// Destroys a temporary instance on drop unless disarmed, retrying off the
/// task so a panic or cancellation in the owner cannot leak the container.
struct CleanupGuard {
    st: Option<SharedState>,
    id: String,
}

impl CleanupGuard {
    fn new(st: SharedState, id: String) -> Self {
        Self { st: Some(st), id }
    }

    fn disarm(mut self) {
        self.st = None;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        let Some(st) = self.st.take() else {
            return;
        };
        let id = std::mem::take(&mut self.id);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(container = %id, "preflight cleanup lost its runtime; orphan sweep will remove it");
            return;
        };
        handle.spawn(async move {
            for attempt in 1..=3u32 {
                tokio::time::sleep(Duration::from_secs(2 * u64::from(attempt))).await;
                match st.containers.destroy(&id).await {
                    Ok(()) | Err(AppError::NotFound(_)) => return,
                    Err(error) => {
                        tracing::warn!(container = %id, attempt, %error, "preflight cleanup retry failed");
                    }
                }
            }
            tracing::warn!(container = %id, "preflight instance left for the orphan sweep");
        });
    }
}

async fn confirm_liveness(st: &SharedState, id: &str) -> AppResult<()> {
    if crate::services::worker::parse_worker_handle(id).is_some() {
        // The worker backend returns only after observing `Ready`.
        return Ok(());
    }
    let deadline = Instant::now() + LIVENESS_GRACE;
    loop {
        tokio::time::sleep(LIVENESS_POLL).await;
        match st.containers.inspect_liveness(id).await? {
            ContainerLiveness::Running => {}
            ContainerLiveness::Stopped => {
                return Err(AppError::unavailable(
                    "The container exited shortly after starting; inspect its entrypoint and logs.",
                ));
            }
            ContainerLiveness::Unknown => {
                if Instant::now() >= deadline {
                    return Err(AppError::unavailable(
                        "The container did not report a running state within the readiness grace period.",
                    ));
                }
            }
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
    }
}

async fn destroy_instance(st: &SharedState, guard: CleanupGuard) {
    let id = guard.id.clone();
    match tokio::time::timeout(DESTROY_DEADLINE, st.containers.destroy(&id)).await {
        Ok(Ok(())) | Ok(Err(AppError::NotFound(_))) => guard.disarm(),
        Ok(Err(error)) => {
            tracing::warn!(container = %id, %error, "preflight destroy failed; retrying in background");
            drop(guard);
        }
        Err(_) => {
            tracing::warn!(container = %id, "preflight destroy timed out; retrying in background");
            drop(guard);
        }
    }
}

/// A create request that outlived its deadline may still have produced a
/// runtime. Locate it by operation id and remove it.
async fn reclaim_dropped_create(st: &SharedState, operation_id: &str) {
    let lookup = tokio::time::timeout(
        Duration::from_secs(30),
        st.containers.find_operation_runtime(operation_id),
    )
    .await;
    if let Ok(Ok(Some(id))) = lookup {
        destroy_instance(st, CleanupGuard::new(st.clone(), id)).await;
    }
}

async fn run_group(
    st: &SharedState,
    job_id: Uuid,
    index: usize,
    group: &ImageGroup,
) -> GroupOutcome {
    let started = Instant::now();
    let mut outcome = GroupOutcome {
        pull: StepStatus::Skipped,
        start: StepStatus::Pending,
        duration_ms: 0,
        error: None,
    };
    if !group.is_worker {
        let pulled =
            match tokio::time::timeout(PULL_DEADLINE, st.containers.pull_image(&group.image)).await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(bounded_error(error.to_string())),
                Err(_) => Err(format!(
                    "The pull exceeded {} seconds",
                    PULL_DEADLINE.as_secs()
                )),
            };
        match pulled {
            Ok(()) => outcome.pull = StepStatus::Succeeded,
            Err(error) => {
                outcome.pull = StepStatus::Failed;
                outcome.start = StepStatus::Skipped;
                outcome.error = Some(error);
                outcome.duration_ms = elapsed_ms(started);
                return outcome;
            }
        }
    }

    let operation_id = format!("image-preflight:{job_id}:{index}:{}", Uuid::new_v4());
    let create = async {
        match &group.launch {
            Launch::Legacy(spec) => {
                let mut spec = spec.clone();
                spec.operation_id = Some(operation_id.clone());
                st.containers.create(spec).await
            }
            Launch::Workload { spec, proxy_only } => {
                st.containers
                    .create_workload(
                        spec.clone(),
                        Some(operation_id.clone()),
                        Some(PREFLIGHT_FLAG.to_string()),
                        *proxy_only,
                    )
                    .await
            }
        }
    };
    let result = match tokio::time::timeout(START_DEADLINE, create).await {
        Ok(Ok(info)) => {
            let guard = CleanupGuard::new(st.clone(), info.id.clone());
            let ready = confirm_liveness(st, &info.id).await;
            destroy_instance(st, guard).await;
            ready
        }
        Ok(Err(error)) => Err(error),
        Err(_) => {
            reclaim_dropped_create(st, &operation_id).await;
            Err(AppError::unavailable(format!(
                "The instance did not start within {} seconds",
                START_DEADLINE.as_secs()
            )))
        }
    };
    match result {
        Ok(()) => outcome.start = StepStatus::Succeeded,
        Err(error) => {
            outcome.start = StepStatus::Failed;
            outcome.error = Some(bounded_error(error.to_string()));
        }
    }
    outcome.duration_ms = elapsed_ms(started);
    outcome
}

/// Run the claimed preflight job to completion and return its summary.
pub async fn execute_job(st: &SharedState, claimed: &ClaimedControlJob) -> AppResult<Value> {
    let job = &claimed.model;
    let plan = super::plan(st, job.game_id).await?;
    if plan.fingerprint != job.fingerprint {
        return Err(AppError::conflict(
            "the enabled challenge set changed after this preflight was queued; start it again",
        ));
    }
    store::upsert_results(st.pg(), job.id, &initial_records(&plan.targets)).await?;
    let groups = group_targets(&plan.targets);
    let mut summary = PreflightSummary {
        challenges: plan.targets.len(),
        images: groups.len(),
        skipped: plan
            .targets
            .iter()
            .filter(|target| target.skip_reason.is_some())
            .count(),
        capacity: plan.capacity.clone(),
        ..PreflightSummary::default()
    };
    let total = i32::try_from(groups.len().max(1)).unwrap_or(i32::MAX);
    crate::services::control_jobs::set_progress(st.pg(), job.id, claimed.lease_token, 0, total)
        .await?;

    let gate = Arc::new(Semaphore::new(IMAGE_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for (index, group) in groups.into_iter().enumerate() {
        let gate = gate.clone();
        let st = st.clone();
        let job_id = job.id;
        tasks.spawn(async move {
            let _permit = gate.acquire_owned().await;
            let _ = store::mark_running(st.pg(), job_id, &group.challenge_ids).await;
            let outcome = run_group(&st, job_id, index, &group).await;
            (group, outcome)
        });
    }

    let deadline = tokio::time::Instant::now() + JOB_DEADLINE;
    let mut completed = 0i32;
    let mut abandoned: Option<(StepStatus, &str)> = None;
    while !tasks.is_empty() {
        if crate::services::control_jobs::cancellation_requested(
            st.pg(),
            job.id,
            claimed.lease_token,
        )
        .await?
        {
            abandoned = Some((StepStatus::Skipped, CANCELLED_ERROR));
            break;
        }
        let joined = match tokio::time::timeout_at(deadline, tasks.join_next()).await {
            Ok(joined) => joined,
            Err(_) => {
                abandoned = Some((StepStatus::Failed, JOB_DEADLINE_ERROR));
                break;
            }
        };
        let Some(joined) = joined else {
            break;
        };
        match joined {
            Ok((group, outcome)) => {
                store::upsert_results(st.pg(), job.id, &outcome_records(&group, &outcome)).await?;
                let ok =
                    outcome.pull != StepStatus::Failed && outcome.start == StepStatus::Succeeded;
                if ok {
                    summary.succeeded += group.challenge_ids.len();
                } else {
                    summary.failed += group.challenge_ids.len();
                }
            }
            Err(error) => {
                tracing::warn!(job = %job.id, %error, "image preflight task failed");
            }
        }
        completed = completed.saturating_add(1).min(total);
        crate::services::control_jobs::set_progress(
            st.pg(),
            job.id,
            claimed.lease_token,
            completed,
            total,
        )
        .await?;
    }
    if let Some((status, error)) = abandoned {
        // Running tasks keep their permits and finish their own cleanup; the
        // coordinator only stops waiting for them.
        tasks.detach_all();
        let closed = store::fail_unfinished(st.pg(), job.id, status, error).await?;
        summary.failed += usize::try_from(closed).unwrap_or(usize::MAX);
    } else {
        let closed = store::fail_unfinished(
            st.pg(),
            job.id,
            StepStatus::Failed,
            "The preflight task for this image ended without a result",
        )
        .await?;
        summary.failed += usize::try_from(closed).unwrap_or(usize::MAX);
    }
    serde_json::to_value(&summary)
        .map_err(|error| AppError::internal(format!("preflight summary failed: {error}")))
}

#[cfg(test)]
mod tests {
    use super::super::plan::Backend;
    use super::*;
    use crate::services::container::{ContainerBackendKind, ContainerSpec};
    use rsctf_worker_protocol::GameKind;

    fn spec(image: &str) -> ContainerSpec {
        ContainerSpec {
            game_kind: GameKind::Jeopardy,
            image: image.to_string(),
            memory_limit: 64,
            cpu_count: 1,
            storage_limit: 512,
            expose_port: 80,
            publish_port: true,
            proxy_only: true,
            env: Vec::new(),
            flag: None,
            ad_network: None,
            allow_egress: false,
            control_plane_callback_ports: Vec::new(),
            network_mode: crate::utils::enums::NetworkMode::Open,
            operation_id: None,
        }
    }

    fn target(id: i32, key: &str, launch: bool) -> PreflightTarget {
        PreflightTarget {
            challenge_id: id,
            image: key.to_string(),
            group_key: key.to_string(),
            backend: Backend::Local(ContainerBackendKind::Docker),
            launch: launch.then(|| Launch::Legacy(spec(key))),
            skip_reason: (!launch).then(|| "no immutable build".to_string()),
            per_instance: Default::default(),
            instances: 1,
        }
    }

    #[test]
    fn one_group_per_distinct_launch_identity_keeps_challenge_order() {
        let targets = [
            target(1, "image:a", true),
            target(2, "image:b", true),
            target(3, "image:a", true),
            target(4, "image:c", false),
        ];
        let groups = group_targets(&targets);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].challenge_ids, vec![1, 3]);
        assert_eq!(groups[1].challenge_ids, vec![2]);
        let rows = initial_records(&targets);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[3].pull_status, StepStatus::Skipped);
        assert_eq!(rows[3].start_status, StepStatus::Skipped);
        assert_eq!(rows[3].error.as_deref(), Some("no immutable build"));
        assert_eq!(rows[0].pull_status, StepStatus::Pending);
    }

    #[test]
    fn outcomes_fan_out_to_every_challenge_sharing_the_image() {
        let groups = group_targets(&[target(1, "image:a", true), target(3, "image:a", true)]);
        let outcome = GroupOutcome {
            pull: StepStatus::Succeeded,
            start: StepStatus::Failed,
            duration_ms: 1_234,
            error: Some("exited".into()),
        };
        let rows = outcome_records(&groups[0], &outcome);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.start_status == StepStatus::Failed
            && row.duration_ms == 1_234
            && row.error.as_deref() == Some("exited")));
    }

    #[test]
    fn deadlines_stay_inside_the_control_job_budget_and_orphan_grace() {
        let budget = Duration::from_secs(
            crate::services::control_jobs::CONTROL_JOB_EXECUTION_BUDGET_SECONDS,
        );
        assert!(JOB_DEADLINE < budget);
        // A live instance must be gone before the 300-second orphan sweep
        // grace could mistake it for an abandoned container.
        assert!(START_DEADLINE + LIVENESS_GRACE + DESTROY_DEADLINE < Duration::from_secs(300));
        assert!((1..=4).contains(&IMAGE_CONCURRENCY));
    }
}
