use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use rsctf_worker_protocol::{
    ControlMessage, EnsureAbsent, EnsureWorkload, ValidatedWorkloadSpec, WorkloadFence,
};
use tokio::sync::watch;
use uuid::Uuid;

use super::{WorkerError, WorkerService};
use crate::services::worker_store::{DueWorkload, WorkerStore, WorkloadDesiredState};

const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);
const DISPATCH_RETRY: Duration = Duration::from_secs(2);
const ORPHAN_SCAN_INTERVAL: Duration = Duration::from_secs(30);
const ORPHAN_GRACE: Duration = Duration::from_secs(5 * 60);
const TERMINAL_SCAN_INTERVAL: Duration = Duration::from_secs(60);
const TERMINAL_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const COMMAND_TIMEOUT_MS: u64 = 60_000;
const BATCH_SIZE: i64 = 256;
const MAINTENANCE_BATCH_SIZE: i64 = 1_000;
const MAX_MAINTENANCE_BATCHES: usize = 4;

/// Start the durable desired/observed-state reconciler on the singleton
/// network owner. The registry suppresses an already-running lifecycle command;
/// assignment and generation fences make a retry after its deadline safe.
pub fn start_reconciler(
    store: WorkerStore,
    service: Arc<WorkerService>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut orphan_scan = tokio::time::interval(ORPHAN_SCAN_INTERVAL);
        orphan_scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut terminal_scan = tokio::time::interval(TERMINAL_SCAN_INTERVAL);
        terminal_scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    expire_sessions(&store, &service).await;
                    reconcile_batch(&store, &service).await;
                }
                _ = orphan_scan.tick() => {
                    fence_orphaned_containers(&store).await;
                }
                _ = terminal_scan.tick() => {
                    prune_terminal_workloads(&store).await;
                }
            }
        }
    })
}

async fn prune_terminal_workloads(store: &WorkerStore) {
    let completed_before = Utc::now()
        - chrono::Duration::from_std(TERMINAL_RETENTION)
            .expect("worker terminal retention duration fits chrono");
    let mut deleted = 0_usize;
    for _ in 0..MAX_MAINTENANCE_BATCHES {
        match store
            .delete_terminal_workloads(completed_before, MAINTENANCE_BATCH_SIZE)
            .await
        {
            Ok(batch) => {
                deleted += batch.len();
                if batch.len() < MAINTENANCE_BATCH_SIZE as usize {
                    break;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "worker terminal workload cleanup failed");
                return;
            }
        }
    }
    if deleted > 0 {
        tracing::info!(deleted, "pruned terminal worker workload history");
    }
}

async fn fence_orphaned_containers(store: &WorkerStore) {
    let created_before = Utc::now()
        - chrono::Duration::from_std(ORPHAN_GRACE)
            .expect("worker orphan grace duration fits chrono");
    match store
        .mark_orphaned_container_workloads_absent(created_before, BATCH_SIZE)
        .await
    {
        Ok(orphaned) if !orphaned.is_empty() => {
            tracing::warn!(
                count = orphaned.len(),
                "fenced worker workloads without committed container bookkeeping"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "worker orphan cleanup failed"),
    }
}

async fn expire_sessions(store: &WorkerStore, service: &WorkerService) {
    match store.expire_sessions(BATCH_SIZE).await {
        Ok(expired) => {
            for worker_id in expired {
                service.registry().disconnect(worker_id).await;
            }
        }
        Err(error) => tracing::warn!(%error, "worker session expiry failed"),
    }
}

async fn reconcile_batch(store: &WorkerStore, service: &WorkerService) {
    let retry_before = Utc::now()
        - chrono::Duration::from_std(DISPATCH_RETRY)
            .expect("worker dispatch retry duration fits chrono");
    let due = match store.list_due_workloads(retry_before, BATCH_SIZE).await {
        Ok(due) => due,
        Err(error) => {
            tracing::warn!(%error, "worker workload reconciliation query failed");
            return;
        }
    };
    for due_workload in due {
        let message = match command_for(&due_workload) {
            Ok(message) => message,
            Err(error) => {
                tracing::error!(
                    workload_id = %due_workload.workload.id,
                    %error,
                    "stored worker workload cannot be reconciled"
                );
                continue;
            }
        };
        let dispatch = service.send(due_workload.workload.worker_id, message).await;
        if should_defer_retry(&dispatch) {
            if let Err(error) = store
                .mark_dispatched(
                    due_workload.session.fence,
                    due_workload.workload.id,
                    due_workload.workload.assignment_id,
                    due_workload.workload.generation,
                )
                .await
            {
                tracing::warn!(
                    workload_id = %due_workload.workload.id,
                    %error,
                    "failed to defer worker command retry"
                );
            }
            continue;
        }
        match dispatch {
            Err(WorkerError::Offline | WorkerError::StaleSession) => {}
            Err(error) => tracing::warn!(
                workload_id = %due_workload.workload.id,
                %error,
                "worker command dispatch failed"
            ),
            Ok(_) => unreachable!("successful dispatches defer retry"),
        }
    }
}

fn should_defer_retry(dispatch: &Result<Uuid, WorkerError>) -> bool {
    matches!(dispatch, Ok(_) | Err(WorkerError::Busy))
}

fn command_for(due: &DueWorkload) -> Result<ControlMessage, WorkerError> {
    let generation = u64::try_from(due.workload.generation)
        .map_err(|_| WorkerError::Authority("negative workload generation".into()))?;
    let fence = WorkloadFence {
        workload_id: due.workload.id,
        assignment_id: due.workload.assignment_id,
        generation,
    };
    Ok(match due.workload.desired_state {
        WorkloadDesiredState::Present => {
            let spec: ValidatedWorkloadSpec =
                serde_json::from_value(due.workload.definition.spec.clone()).map_err(|error| {
                    WorkerError::Authority(format!("invalid stored workload spec: {error}"))
                })?;
            ControlMessage::EnsureWorkload(EnsureWorkload {
                command_id: Uuid::new_v4(),
                fence,
                spec_hash: hex::encode(due.workload.definition.spec_hash_sha256),
                timeout_ms: COMMAND_TIMEOUT_MS,
                spec,
            })
        }
        WorkloadDesiredState::Absent => ControlMessage::EnsureAbsent(EnsureAbsent {
            command_id: Uuid::new_v4(),
            fence,
            spec_hash: hex::encode(due.workload.definition.spec_hash_sha256),
            timeout_ms: COMMAND_TIMEOUT_MS,
        }),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::services::worker_store::{
        PlatformOs, ResourceReservation, SessionFence, WorkerSession, WorkerWorkload,
        WorkloadDefinition, WorkloadObservedState,
    };

    fn due(desired_state: WorkloadDesiredState) -> DueWorkload {
        let workload_id = Uuid::new_v4();
        let assignment_id = Uuid::new_v4();
        let spec = json!({
            "gameKind": "jeopardy",
            "platform": {"operatingSystem": "linux", "architecture": "amd64"},
            "services": [{
                "name": "challenge",
                "image": {"type": "registryDigest", "repository": "example.invalid/c", "digest": format!("sha256:{}", "a".repeat(64))},
                "resources": {"cpuMillis": 100, "memoryBytes": 1048576},
                "replicas": 1,
                "stateless": false,
                "environment": {},
                "ports": [{"name": "service", "containerPort": 8080, "protocol": "tcp"}]
            }],
            "primaryEndpoint": {"service": "challenge", "port": "service"}
        });
        DueWorkload {
            workload: WorkerWorkload {
                id: workload_id,
                owner_kind: "container".into(),
                owner_key: "test".into(),
                worker_id: Uuid::new_v4(),
                assignment_id,
                generation: 1,
                definition: WorkloadDefinition {
                    spec,
                    spec_hash_sha256: [7; 32],
                    required_os: PlatformOs::Linux,
                    required_architecture: "amd64".into(),
                    required_runtime: "docker".into(),
                    reservation: ResourceReservation {
                        cpu_millis: 100,
                        memory_bytes: 1_048_576,
                        slots: 1,
                    },
                },
                required_labels: json!({}),
                desired_state,
                observed_state: WorkloadObservedState::Unknown,
                observed_session_epoch: None,
                observed_message: None,
                observed_at: None,
                ready_at: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            session: WorkerSession {
                fence: SessionFence {
                    worker_id: Uuid::new_v4(),
                    session_id: Uuid::new_v4(),
                    session_epoch: 1,
                },
                lease_expires_at: Utc::now(),
            },
        }
    }

    #[test]
    fn present_and_absent_commands_keep_the_same_fence() {
        let present = due(WorkloadDesiredState::Present);
        let absent = DueWorkload {
            workload: WorkerWorkload {
                desired_state: WorkloadDesiredState::Absent,
                ..present.workload.clone()
            },
            session: present.session.clone(),
        };
        let present_command = command_for(&present).unwrap();
        let absent_command = command_for(&absent).unwrap();
        assert_eq!(
            present_command.workload_fence(),
            absent_command.workload_fence()
        );
    }

    #[test]
    fn busy_dispatch_defers_retry_so_the_due_batch_can_advance() {
        assert!(should_defer_retry(&Err(WorkerError::Busy)));
        assert!(should_defer_retry(&Ok(Uuid::new_v4())));
        assert!(!should_defer_retry(&Err(WorkerError::Offline)));
        assert!(!should_defer_retry(&Err(WorkerError::StaleSession)));
    }
}
