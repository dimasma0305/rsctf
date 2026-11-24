//! Fenced per-assignment workload networks.

use bollard::models::Network;
use bollard::network::{CreateNetworkOptions, InspectNetworkOptions};
use rsctf_worker_protocol::{CommandErrorCode, OperatingSystem, WorkloadFence};

use super::support::{
    base_labels, docker_error, is_not_found, label, network_name, validate_workload_network,
};
use super::windows_acl::{workload_network_driver, workload_network_options};
use super::{
    DockerRuntime, LABEL_ASSIGNMENT, LABEL_GENERATION, LABEL_MANAGED, LABEL_SPEC_HASH,
    LABEL_WORKER, LABEL_WORKLOAD,
};
use crate::runtime::RuntimeError;

impl DockerRuntime {
    pub(super) async fn assigned_network(
        &self,
        fence: WorkloadFence,
        spec_hash: &str,
    ) -> Result<Option<(Network, u64)>, RuntimeError> {
        let name = network_name(fence);
        let network = match self
            .admission
            .read(
                "inspect_network",
                self.docker
                    .inspect_network(&name, None::<InspectNetworkOptions<String>>),
            )
            .await?
        {
            Ok(network) => network,
            Err(error) if is_not_found(&error) => return Ok(None),
            Err(error) => return Err(docker_error("inspect workload network", error)),
        };
        // The Docker name uses shortened UUIDs. Only full labels identify the
        // owner, including when no container remains to validate the fence.
        let labels = network.labels.as_ref();
        if network.name.as_deref() != Some(name.as_str())
            || label(labels, LABEL_MANAGED) != Some("true")
            || label(labels, LABEL_WORKER) != Some(self.worker_id.to_string().as_str())
            || label(labels, LABEL_WORKLOAD) != Some(fence.workload_id.to_string().as_str())
            || label(labels, LABEL_ASSIGNMENT) != Some(fence.assignment_id.to_string().as_str())
        {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleAssignment,
                "workload network belongs to a different assignment",
            ));
        }
        let generation = label(labels, LABEL_GENERATION)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::SpecConflict,
                    "workload network has an invalid generation",
                )
            })?;
        if generation > fence.generation {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleGeneration,
                "a newer workload network is already present",
            ));
        }
        if generation == fence.generation && label(labels, LABEL_SPEC_HASH) != Some(spec_hash) {
            return Err(RuntimeError::new(
                CommandErrorCode::SpecConflict,
                "workload network has a different spec hash",
            ));
        }
        Ok(Some((network, generation)))
    }

    pub(super) async fn ensure_network(
        &self,
        fence: WorkloadFence,
        spec_hash: &str,
        operating_system: OperatingSystem,
        previous: Option<(Network, u64)>,
    ) -> Result<String, RuntimeError> {
        let name = network_name(fence);
        if let Some((network, generation)) = previous {
            if generation == fence.generation {
                validate_workload_network(
                    &network,
                    self.worker_id,
                    fence,
                    spec_hash,
                    operating_system,
                )?;
                return Ok(name);
            }
            self.remove_network(&network).await?;
        }

        self.admission
            .lifecycle(
                "create_network",
                self.docker.create_network(CreateNetworkOptions {
                    name: name.clone(),
                    check_duplicate: true,
                    driver: workload_network_driver(operating_system).to_string(),
                    // The agent joins no external network and dials the
                    // container's private address directly. This keeps
                    // challenge egress denied without publishing host ports
                    // or mutating the host firewall.
                    internal: operating_system == OperatingSystem::Linux,
                    options: workload_network_options(operating_system),
                    labels: base_labels(self.worker_id, fence, spec_hash),
                    ..Default::default()
                }),
            )
            .await?
            .map_err(|error| docker_error("create workload network", error))?;
        let (inspected, _) = self
            .assigned_network(fence, spec_hash)
            .await?
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::NotFound,
                    "created workload network disappeared",
                )
            })?;
        validate_workload_network(
            &inspected,
            self.worker_id,
            fence,
            spec_hash,
            operating_system,
        )?;
        Ok(name)
    }

    pub(super) async fn remove_network(&self, network: &Network) -> Result<(), RuntimeError> {
        let id = network
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::SpecConflict,
                    "workload network has no Docker ID",
                )
            })?;
        match self
            .admission
            .lifecycle("remove_network", self.docker.remove_network(id))
            .await?
        {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(docker_error("remove workload network", error)),
        }
    }
}
