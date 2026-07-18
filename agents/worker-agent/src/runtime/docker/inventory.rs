use std::collections::{BTreeMap, HashMap, HashSet};

use bollard::container::ListContainersOptions;
use futures_util::StreamExt;
use rsctf_worker_protocol::{InventoryItem, ObservedWorkloadState, ReplicaStatus, WorkloadFence};
use uuid::Uuid;

use super::support::{docker_error, label};
use super::{
    DockerRuntime, LABEL_ASSIGNMENT, LABEL_EXPECTED_REPLICAS, LABEL_GENERATION, LABEL_MANAGED,
    LABEL_SPEC_HASH, LABEL_WORKER, LABEL_WORKLOAD, MAX_CONCURRENT_READINESS_PROBES,
};
use crate::runtime::RuntimeError;

type InventoryKey = (Uuid, Uuid, u64, String);
type InventoryGroup = (Option<usize>, bool, Vec<ReplicaStatus>);

impl DockerRuntime {
    pub(super) async fn collect_inventory(&self) -> Result<Vec<InventoryItem>, RuntimeError> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{LABEL_WORKER}={}", self.worker_id),
            ],
        );
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(|error| docker_error("inventory workload containers", error))?;
        let active_workloads = containers
            .iter()
            .filter_map(|container| {
                label(container.labels.as_ref(), LABEL_WORKLOAD)
                    .and_then(|value| Uuid::parse_str(value).ok())
            })
            .collect::<HashSet<_>>();
        self.tombstones.prune_inactive(&active_workloads).await?;
        let running_container_ids = containers
            .iter()
            .filter(|container| container.state.as_deref() == Some("running"))
            .filter_map(|container| container.id.clone())
            .collect::<HashSet<_>>();
        self.ready_containers
            .retain(|container_id| running_container_ids.contains(container_id));

        let observed =
            futures_util::stream::iter(containers.into_iter().map(|container| async move {
                let labels = container.labels.as_ref();
                let workload_id =
                    label(labels, LABEL_WORKLOAD).and_then(|value| Uuid::parse_str(value).ok())?;
                let assignment_id = label(labels, LABEL_ASSIGNMENT)
                    .and_then(|value| Uuid::parse_str(value).ok())?;
                let generation =
                    label(labels, LABEL_GENERATION).and_then(|value| value.parse().ok())?;
                let spec_hash = label(labels, LABEL_SPEC_HASH)?.to_string();
                let expected_replicas = label(labels, LABEL_EXPECTED_REPLICAS)
                    .and_then(|value| value.parse::<usize>().ok());
                let fence = WorkloadFence {
                    workload_id,
                    assignment_id,
                    generation,
                };
                let status = self.replica_status_with_readiness(&container, fence).await;
                Some((fence, spec_hash, expected_replicas, status))
            }))
            .buffer_unordered(MAX_CONCURRENT_READINESS_PROBES)
            .collect::<Vec<_>>()
            .await;

        let mut grouped: BTreeMap<InventoryKey, InventoryGroup> = BTreeMap::new();
        for (fence, spec_hash, expected_replicas, status) in observed.into_iter().flatten() {
            let entry = grouped
                .entry((
                    fence.workload_id,
                    fence.assignment_id,
                    fence.generation,
                    spec_hash,
                ))
                .or_insert_with(|| (expected_replicas, expected_replicas.is_some(), Vec::new()));
            if entry.0 != expected_replicas {
                entry.1 = false;
            }
            entry.2.push(status);
        }

        Ok(grouped
            .into_iter()
            .map(
                |(
                    (workload_id, assignment_id, generation, spec_hash),
                    (expected_replicas, labels_valid, mut replicas),
                )| {
                    replicas.sort_by(|left, right| {
                        (&left.service, left.replica).cmp(&(&right.service, right.replica))
                    });
                    let state = if labels_valid
                        && expected_replicas == Some(replicas.len())
                        && replicas.iter().all(|replica| replica.ready)
                    {
                        ObservedWorkloadState::Ready
                    } else {
                        ObservedWorkloadState::Degraded
                    };
                    InventoryItem {
                        fence: WorkloadFence {
                            workload_id,
                            assignment_id,
                            generation,
                        },
                        spec_hash,
                        state,
                        replicas,
                    }
                },
            )
            .collect())
    }
}
