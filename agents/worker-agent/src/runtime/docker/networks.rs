//! Fenced per-assignment workload networks.

use std::collections::HashMap;

use bollard::network::{CreateNetworkOptions, InspectNetworkOptions, ListNetworksOptions};
use rsctf_worker_protocol::{OperatingSystem, WorkloadFence};
use uuid::Uuid;

use super::support::{
    base_labels, docker_error, is_not_found, network_name, validate_workload_network,
};
use super::windows_acl::{workload_network_driver, workload_network_options};
use super::{DockerRuntime, LABEL_ASSIGNMENT, LABEL_MANAGED, LABEL_WORKER};
use crate::runtime::RuntimeError;

impl DockerRuntime {
    async fn inspect_workload_network(
        &self,
        name: &str,
        operation: &'static str,
    ) -> Result<bollard::models::Network, RuntimeError> {
        self.admission
            .read(
                "inspect_network",
                self.docker
                    .inspect_network(name, None::<InspectNetworkOptions<String>>),
            )
            .await?
            .map_err(|error| docker_error(operation, error))
    }

    pub(super) async fn ensure_network(
        &self,
        fence: WorkloadFence,
        spec_hash: &str,
        operating_system: OperatingSystem,
    ) -> Result<String, RuntimeError> {
        let name = network_name(fence);
        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name.clone()]);
        let networks = self
            .admission
            .read(
                "list_networks",
                self.docker
                    .list_networks(Some(ListNetworksOptions { filters })),
            )
            .await?
            .map_err(|error| docker_error("list workload networks", error))?;
        if networks
            .iter()
            .any(|network| network.name.as_deref() == Some(name.as_str()))
        {
            let inspected = self
                .inspect_workload_network(&name, "inspect workload network")
                .await?;
            validate_workload_network(
                &inspected,
                self.worker_id,
                fence,
                spec_hash,
                operating_system,
            )?;
            return Ok(name);
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
        let inspected = self
            .inspect_workload_network(&name, "inspect created workload network")
            .await?;
        validate_workload_network(
            &inspected,
            self.worker_id,
            fence,
            spec_hash,
            operating_system,
        )?;
        Ok(name)
    }

    pub(super) async fn remove_assignment_networks(
        &self,
        assignment_id: Uuid,
    ) -> Result<(), RuntimeError> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{LABEL_WORKER}={}", self.worker_id),
                format!("{LABEL_ASSIGNMENT}={assignment_id}"),
            ],
        );
        let networks = self
            .admission
            .read(
                "list_networks",
                self.docker
                    .list_networks(Some(ListNetworksOptions { filters })),
            )
            .await?
            .map_err(|error| docker_error("list workload networks for removal", error))?;
        for network in networks {
            if let Some(id) = network.id {
                let removed = self
                    .admission
                    .lifecycle("remove_network", self.docker.remove_network(&id))
                    .await?;
                if let Err(error) = removed {
                    if !is_not_found(&error) {
                        return Err(docker_error("remove workload network", error));
                    }
                }
            }
        }
        Ok(())
    }
}
