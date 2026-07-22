use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicU64;

use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions,
    NetworkingConfig, RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
    UploadToContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::EndpointSettings;
use bollard::network::{CreateNetworkOptions, InspectNetworkOptions, ListNetworksOptions};
use bollard::Docker;
use dashmap::{DashMap, DashSet};
use futures_util::{StreamExt, TryStreamExt};
use rsctf_worker_protocol::{
    CommandErrorCode, EnsureAbsent, EnsureWorkload, ImageIdentity, InventoryItem,
    ObservedWorkloadState, OperatingSystem, Platform, ResourceUsage, RuntimeDescriptor,
    RuntimeKind, TcpProxyRequest, WorkerCapabilities, WorkerCapacity, WorkloadFence,
    WorkloadStatus, WriteFlag,
};
use tokio::net::TcpStream;
use uuid::Uuid;

use super::{RuntimeError, RuntimeOptions, WorkerRuntime};

mod endpoints;
mod inventory;
mod support;
mod tombstones;
mod windows_acl;
use endpoints::{EndpointCacheKey, EndpointTarget};
use support::*;
use tombstones::TombstoneStore;
use windows_acl::{workload_network_driver, workload_network_options};

#[cfg(test)]
mod integration_tests;

const LABEL_MANAGED: &str = "io.rsctf.worker.managed";
const LABEL_WORKER: &str = "io.rsctf.worker.id";
const LABEL_WORKLOAD: &str = "io.rsctf.workload.id";
const LABEL_ASSIGNMENT: &str = "io.rsctf.assignment.id";
const LABEL_GENERATION: &str = "io.rsctf.workload.generation";
const LABEL_SPEC_HASH: &str = "io.rsctf.workload.spec-hash";
const LABEL_SERVICE: &str = "io.rsctf.workload.service";
const LABEL_REPLICA: &str = "io.rsctf.workload.replica";
const LABEL_CPU: &str = "io.rsctf.workload.cpu-millis";
const LABEL_MEMORY: &str = "io.rsctf.workload.memory-bytes";
const LABEL_EXPECTED_REPLICAS: &str = "io.rsctf.workload.expected-replicas";
const LABEL_PORT_PREFIX: &str = "io.rsctf.port.";
const MAX_CONCURRENT_READINESS_PROBES: usize = 32;

pub struct DockerRuntime {
    docker: Docker,
    worker_id: Uuid,
    descriptor: RuntimeDescriptor,
    platform: Platform,
    capacity: WorkerCapacity,
    max_network_endpoints: usize,
    docker_root: std::path::PathBuf,
    minimum_free_bytes: u64,
    writable_layer_bytes: Option<u64>,
    round_robin: DashMap<EndpointCacheKey, AtomicU64>,
    endpoint_cache: DashMap<EndpointCacheKey, Vec<EndpointTarget>>,
    ready_containers: DashSet<String>,
    flag_sequences: DashMap<(Uuid, Uuid, u64, String), u64>,
    tombstones: TombstoneStore,
}

struct ContainerReplicaPlan<'a> {
    fence: WorkloadFence,
    spec_hash: &'a str,
    network: &'a str,
    service: &'a rsctf_worker_protocol::ServiceSpec,
    expected_replicas: usize,
    operating_system: OperatingSystem,
}

impl DockerRuntime {
    pub async fn connect(
        worker_id: Uuid,
        endpoint: Option<&str>,
        state_dir: &Path,
        options: RuntimeOptions,
    ) -> Result<Self, RuntimeError> {
        let endpoint = endpoint.unwrap_or("local");
        let (docker, endpoint_kind) = connect_docker(endpoint)?;
        let docker = docker
            .negotiate_version()
            .await
            .map_err(|error| docker_error("negotiate Docker API version", error))?;
        let info = docker
            .info()
            .await
            .map_err(|error| docker_error("read Docker daemon information", error))?;
        let platform = daemon_platform(&info)?;
        if cfg!(windows) && platform.operating_system == OperatingSystem::Linux {
            return Err(RuntimeError::unsupported(
                "a Windows agent cannot safely proxy a Linux Docker daemon's private bridge addresses; run the Linux agent inside the Docker VM",
            ));
        }
        let writable_layer_bytes = if storage_quota_supported(&info) {
            Some(options.writable_layer_bytes)
        } else if options.allow_unbounded_storage {
            tracing::warn!(
                driver = ?info.driver,
                "Docker storage quotas are unavailable; unbounded writable layers were explicitly allowed"
            );
            None
        } else {
            return Err(RuntimeError::unsupported(
                "Docker storage driver cannot enforce per-container writable-layer quotas; configure overlay2 on XFS with project quotas or native Windows windowsfilter, or use --allow-unbounded-storage only for trusted development fixtures",
            ));
        };
        let docker_root = info
            .docker_root_dir
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    "Docker daemon did not report its data root",
                )
            })?;
        claim_docker_daemon(&docker, worker_id).await?;
        let (network_slots, max_network_endpoints) =
            docker_network_capacity(&docker, &info, worker_id).await?;
        let capacity = daemon_capacity(&info, network_slots);
        let descriptor = RuntimeDescriptor {
            kind: RuntimeKind::Docker,
            version: info.server_version.clone(),
            endpoint_kind: Some(endpoint_kind),
        };
        let runtime = Self {
            docker,
            worker_id,
            descriptor,
            platform,
            capacity,
            max_network_endpoints,
            docker_root,
            minimum_free_bytes: options.minimum_free_bytes,
            writable_layer_bytes,
            round_robin: DashMap::new(),
            endpoint_cache: DashMap::new(),
            ready_containers: DashSet::new(),
            flag_sequences: DashMap::new(),
            tombstones: TombstoneStore::new(state_dir),
        };
        runtime.audit_windows_endpoints().await?;
        Ok(runtime)
    }

    async fn ensure_image(&self, image: &ImageIdentity) -> Result<String, RuntimeError> {
        let image_name = match image {
            ImageIdentity::RegistryDigest { repository, digest } => {
                format!("{repository}@{digest}")
            }
            ImageIdentity::WorkerLocal {
                worker_id,
                image_id,
            } => {
                if *worker_id != self.worker_id {
                    return Err(RuntimeError::new(
                        CommandErrorCode::InvalidSpec,
                        "worker-local image belongs to a different worker",
                    ));
                }
                image_id.clone()
            }
        };

        match self.docker.inspect_image(&image_name).await {
            Ok(_) => return Ok(image_name),
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(docker_error("inspect image", error)),
        }
        if matches!(image, ImageIdentity::WorkerLocal { .. }) {
            return Err(RuntimeError::new(
                CommandErrorCode::NotFound,
                "worker-local image is not present on this Docker daemon",
            ));
        }

        let options = CreateImageOptions {
            from_image: image_name.clone(),
            ..Default::default()
        };
        self.docker
            .create_image(Some(options), None, None)
            .try_collect::<Vec<_>>()
            .await
            .map_err(|error| docker_error("pull image", error))?;
        Ok(image_name)
    }

    async fn ensure_network(
        &self,
        fence: WorkloadFence,
        spec_hash: &str,
        operating_system: OperatingSystem,
    ) -> Result<String, RuntimeError> {
        let name = network_name(fence);
        let mut filters = HashMap::new();
        filters.insert("name".to_string(), vec![name.clone()]);
        let networks = self
            .docker
            .list_networks(Some(ListNetworksOptions { filters }))
            .await
            .map_err(|error| docker_error("list workload networks", error))?;
        if networks
            .iter()
            .any(|network| network.name.as_deref() == Some(name.as_str()))
        {
            let inspected = self
                .docker
                .inspect_network(&name, None::<InspectNetworkOptions<String>>)
                .await
                .map_err(|error| docker_error("inspect workload network", error))?;
            validate_workload_network(
                &inspected,
                self.worker_id,
                fence,
                spec_hash,
                operating_system,
            )?;
            return Ok(name);
        }

        self.docker
            .create_network(CreateNetworkOptions {
                name: name.clone(),
                check_duplicate: true,
                driver: workload_network_driver(operating_system).to_string(),
                // The agent joins no external network and dials the container's
                // private address directly. This keeps challenge egress denied
                // without publishing host ports or mutating the host firewall.
                internal: operating_system == OperatingSystem::Linux,
                options: workload_network_options(operating_system),
                labels: base_labels(self.worker_id, fence, spec_hash),
                ..Default::default()
            })
            .await
            .map_err(|error| docker_error("create workload network", error))?;
        let inspected = self
            .docker
            .inspect_network(&name, None::<InspectNetworkOptions<String>>)
            .await
            .map_err(|error| docker_error("inspect created workload network", error))?;
        validate_workload_network(
            &inspected,
            self.worker_id,
            fence,
            spec_hash,
            operating_system,
        )?;
        Ok(name)
    }

    async fn existing_containers(
        &self,
        workload_id: Uuid,
    ) -> Result<Vec<bollard::models::ContainerSummary>, RuntimeError> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{LABEL_WORKER}={}", self.worker_id),
                format!("{LABEL_WORKLOAD}={workload_id}"),
            ],
        );
        self.docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(|error| docker_error("list workload containers", error))
    }

    async fn remove_containers(
        &self,
        containers: &[bollard::models::ContainerSummary],
    ) -> Result<(), RuntimeError> {
        let mut failed = Vec::new();
        for container in containers {
            let Some(id) = container.id.as_deref() else {
                continue;
            };
            self.ready_containers.remove(id);
            if let Err(error) = self
                .docker
                .remove_container(
                    id,
                    Some(RemoveContainerOptions {
                        force: true,
                        v: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                if !is_not_found(&error) {
                    failed.push(id.to_string());
                }
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(RuntimeError::new(
                CommandErrorCode::PartialFailure,
                "one or more workload containers could not be removed",
            )
            .with_failed_replicas(failed))
        }
    }

    async fn remove_assignment_networks(&self, assignment_id: Uuid) -> Result<(), RuntimeError> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{LABEL_WORKER}={}", self.worker_id),
                format!("{LABEL_ASSIGNMENT}={assignment_id}"),
            ],
        );
        for network in self
            .docker
            .list_networks(Some(ListNetworksOptions { filters }))
            .await
            .map_err(|error| docker_error("list workload networks for removal", error))?
        {
            if let Some(id) = network.id {
                if let Err(error) = self.docker.remove_network(&id).await {
                    if !is_not_found(&error) {
                        return Err(docker_error("remove workload network", error));
                    }
                }
            }
        }
        Ok(())
    }

    async fn create_container(
        &self,
        plan: ContainerReplicaPlan<'_>,
        replica: u16,
    ) -> Result<String, RuntimeError> {
        self.check_free_space().await?;
        let ContainerReplicaPlan {
            fence,
            spec_hash,
            network,
            service,
            expected_replicas,
            operating_system,
        } = plan;
        let image = self.ensure_image(&service.image).await?;
        let name = container_name(fence, &service.name, replica);
        let mut labels = base_labels(self.worker_id, fence, spec_hash);
        labels.insert(LABEL_SERVICE.to_string(), service.name.clone());
        labels.insert(LABEL_REPLICA.to_string(), replica.to_string());
        labels.insert(
            LABEL_CPU.to_string(),
            service.resources.cpu_millis.to_string(),
        );
        labels.insert(
            LABEL_MEMORY.to_string(),
            service.resources.memory_bytes.to_string(),
        );
        labels.insert(
            LABEL_EXPECTED_REPLICAS.to_string(),
            expected_replicas.to_string(),
        );
        for port in &service.ports {
            labels.insert(
                format!("io.rsctf.port.{}", port.name),
                port.container_port.to_string(),
            );
        }

        let exposed_ports = service
            .ports
            .iter()
            .map(|port| (docker_port(port.container_port), HashMap::new()))
            .collect::<HashMap<_, _>>();
        let endpoint = EndpointSettings {
            aliases: Some(vec![service.name.clone()]),
            ..Default::default()
        };
        let host_config = workload_host_config(
            operating_system,
            network,
            service.resources.cpu_millis,
            service.resources.memory_bytes,
            self.writable_layer_bytes,
        );
        let config = Config {
            image: Some(image),
            env: Some(
                service
                    .environment
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect(),
            ),
            exposed_ports: Some(exposed_ports),
            labels: Some(labels),
            host_config: Some(host_config),
            networking_config: Some(NetworkingConfig {
                endpoints_config: HashMap::from([(network.to_string(), endpoint)]),
            }),
            ..Default::default()
        };
        let created = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name,
                    platform: None,
                }),
                config,
            )
            .await
            .map_err(|error| docker_error("create workload container", error))?;
        if operating_system == OperatingSystem::Windows {
            self.secure_new_windows_container(&created.id, network)
                .await?;
        }
        self.docker
            .start_container(&created.id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|error| docker_error("start workload container", error))?;
        if operating_system == OperatingSystem::Windows {
            self.verify_started_windows_container(&created.id, network)
                .await?;
        }
        Ok(created.id)
    }

    async fn check_free_space(&self) -> Result<(), RuntimeError> {
        let root = self.docker_root.clone();
        let available = tokio::task::spawn_blocking(move || fs2::available_space(root))
            .await
            .map_err(|error| {
                RuntimeError::new(
                    CommandErrorCode::Internal,
                    format!("Docker free-space task failed: {error}"),
                )
            })?
            .map_err(|error| {
                RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    format!("read Docker data-root free space: {error}"),
                )
            })?;
        if available < self.minimum_free_bytes {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                format!(
                    "Docker data root has {available} free bytes, below the {}-byte safety floor",
                    self.minimum_free_bytes
                ),
            ));
        }
        Ok(())
    }

    async fn stop_managed_containers(&self) {
        self.endpoint_cache.clear();
        self.round_robin.clear();
        self.ready_containers.clear();
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{LABEL_WORKER}={}", self.worker_id),
            ],
        );
        let containers = match self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: false,
                filters,
                ..Default::default()
            }))
            .await
        {
            Ok(containers) => containers,
            Err(error) => {
                tracing::error!(%error, "low-disk watchdog could not list managed containers");
                return;
            }
        };
        for container in containers {
            let Some(id) = container.id else {
                continue;
            };
            if let Err(error) = self
                .docker
                .stop_container(&id, Some(StopContainerOptions { t: 5 }))
                .await
            {
                tracing::error!(container_id = %id, %error, "low-disk watchdog could not stop managed container");
            }
        }
    }

    async fn start_if_stopped(
        &self,
        container: &bollard::models::ContainerSummary,
    ) -> Result<(), RuntimeError> {
        if container.state.as_deref() == Some("running") {
            return Ok(());
        }
        let id = container.id.as_deref().ok_or_else(|| {
            RuntimeError::new(CommandErrorCode::Internal, "Docker omitted container ID")
        })?;
        self.ready_containers.remove(id);
        self.docker
            .start_container(id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|error| docker_error("start adopted workload container", error))
    }

    async fn status_for(
        &self,
        fence: WorkloadFence,
        spec_hash: String,
        expected_replicas: Option<usize>,
    ) -> Result<WorkloadStatus, RuntimeError> {
        let containers = self.existing_containers(fence.workload_id).await?;
        let matching = containers
            .into_iter()
            .filter(|container| labels_match(container.labels.as_ref(), fence, &spec_hash))
            .collect::<Vec<_>>();
        let mut replicas =
            futures_util::stream::iter(matching)
                .map(|container| async move {
                    self.replica_status_with_readiness(&container, fence).await
                })
                .buffer_unordered(MAX_CONCURRENT_READINESS_PROBES)
                .collect::<Vec<_>>()
                .await;
        replicas.sort_by(|left, right| {
            (&left.service, left.replica).cmp(&(&right.service, right.replica))
        });
        let state = if replicas.is_empty() {
            ObservedWorkloadState::Absent
        } else if replicas.iter().all(|replica| replica.ready)
            && expected_replicas.is_none_or(|expected| replicas.len() == expected)
        {
            ObservedWorkloadState::Ready
        } else {
            ObservedWorkloadState::Degraded
        };
        Ok(WorkloadStatus {
            fence,
            spec_hash,
            state,
            replicas,
            detail: None,
        })
    }

    async fn verify_guest_file(
        &self,
        container_id: &str,
        path: &str,
        expected: &[u8],
    ) -> Result<bool, RuntimeError> {
        let mut download = self.docker.download_from_container(
            container_id,
            Some(DownloadFromContainerOptions {
                path: path.to_string(),
            }),
        );
        let mut archive = Vec::new();
        while let Some(chunk) = download
            .try_next()
            .await
            .map_err(|error| docker_error("read back flag file", error))?
        {
            let next = archive.len().checked_add(chunk.len()).ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::Internal,
                    "flag verification archive length overflowed",
                )
            })?;
            if next > MAX_FLAG_ARCHIVE_BYTES {
                return Err(RuntimeError::new(
                    CommandErrorCode::InvalidSpec,
                    "flag verification archive exceeds the 1 MiB safety limit",
                ));
            }
            archive.extend_from_slice(&chunk);
        }
        archive_contains_contents(archive, expected.to_vec()).await
    }

    async fn current_status(&self, fence: WorkloadFence) -> Result<WorkloadStatus, RuntimeError> {
        let item = self
            .inventory()
            .await?
            .into_iter()
            .find(|item| item.fence == fence)
            .ok_or_else(|| RuntimeError::new(CommandErrorCode::NotFound, "workload not found"))?;
        Ok(WorkloadStatus {
            fence: item.fence,
            spec_hash: item.spec_hash,
            state: item.state,
            replicas: item.replicas,
            detail: None,
        })
    }
}

#[async_trait]
impl WorkerRuntime for DockerRuntime {
    fn descriptor(&self) -> RuntimeDescriptor {
        self.descriptor.clone()
    }

    fn capabilities(&self) -> WorkerCapabilities {
        WorkerCapabilities {
            ensure_workload: true,
            write_flag: true,
            tcp_proxy: true,
            interactive_exec: false,
            inventory: true,
            local_image_build: false,
            max_data_lanes: 4,
            max_workload_replicas: workload_replica_capacity(self.max_network_endpoints),
        }
    }

    fn platform(&self) -> Platform {
        self.platform.clone()
    }

    async fn probe(&self) -> Result<(), RuntimeError> {
        self.docker
            .ping()
            .await
            .map_err(|error| docker_error("probe Docker", error))?;
        if let Err(error) = self.check_free_space().await {
            self.stop_managed_containers().await;
            return Err(error);
        }
        Ok(())
    }

    async fn capacity(&self) -> Result<WorkerCapacity, RuntimeError> {
        Ok(self.capacity)
    }

    async fn usage(&self) -> Result<ResourceUsage, RuntimeError> {
        if let Err(error) = self.check_free_space().await {
            self.stop_managed_containers().await;
            return Err(error);
        }
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec![format!("{LABEL_WORKER}={}", self.worker_id)],
        );
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters,
                ..Default::default()
            }))
            .await
            .map_err(|error| docker_error("calculate runtime usage", error))?;
        let mut workloads = std::collections::HashSet::new();
        let mut cpu = 0_u64;
        let mut memory = 0_u64;
        for container in containers {
            let labels = container.labels.as_ref();
            cpu = cpu.saturating_add(
                label(labels, LABEL_CPU)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            );
            memory = memory.saturating_add(
                label(labels, LABEL_MEMORY)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            );
            if let Some(workload) = label(labels, LABEL_WORKLOAD) {
                workloads.insert(workload.to_string());
            }
        }
        Ok(ResourceUsage {
            reserved_cpu_millis: cpu,
            reserved_memory_bytes: memory,
            running_workloads: workloads.len().min(u32::MAX as usize) as u32,
        })
    }

    async fn inventory(&self) -> Result<Vec<InventoryItem>, RuntimeError> {
        self.collect_inventory().await
    }

    async fn ensure_workload(
        &self,
        command: EnsureWorkload,
    ) -> Result<WorkloadStatus, RuntimeError> {
        self.invalidate_workload_endpoints(command.fence.workload_id);
        self.tombstones.reject_stale_present(command.fence).await?;
        let calculated_hash = command
            .spec
            .spec_hash()
            .map_err(|error| RuntimeError::new(CommandErrorCode::InvalidSpec, error.to_string()))?;
        if calculated_hash != command.spec_hash {
            return Err(RuntimeError::new(
                CommandErrorCode::SpecConflict,
                "workload spec hash does not match payload",
            ));
        }
        let replica_count = command
            .spec
            .services
            .iter()
            .map(|service| usize::from(service.replicas))
            .sum::<usize>();
        if replica_count > self.max_network_endpoints {
            return Err(RuntimeError::new(
                CommandErrorCode::InvalidSpec,
                format!(
                    "workload has {replica_count} replicas but this Docker address pool supports at most {} containers per isolated network",
                    self.max_network_endpoints
                ),
            ));
        }
        if command.spec.platform.operating_system != self.platform().operating_system
            || command.spec.platform.architecture != self.platform().architecture
        {
            return Err(RuntimeError::new(
                CommandErrorCode::InvalidSpec,
                "workload platform does not match this worker",
            ));
        }

        let existing = self.existing_containers(command.fence.workload_id).await?;
        for container in &existing {
            let labels = container.labels.as_ref();
            let assignment = label(labels, LABEL_ASSIGNMENT);
            let generation = label(labels, LABEL_GENERATION).and_then(|v| v.parse::<u64>().ok());
            if assignment != Some(command.fence.assignment_id.to_string()).as_deref() {
                return Err(RuntimeError::new(
                    CommandErrorCode::StaleAssignment,
                    "workload has a different active assignment",
                ));
            }
            if generation == Some(command.fence.generation)
                && label(labels, LABEL_SPEC_HASH) != Some(command.spec_hash.as_str())
            {
                return Err(RuntimeError::new(
                    CommandErrorCode::SpecConflict,
                    "generation already exists with a different spec hash",
                ));
            }
            if generation.is_some_and(|value| value > command.fence.generation) {
                return Err(RuntimeError::new(
                    CommandErrorCode::StaleGeneration,
                    "a newer workload generation is already present",
                ));
            }
        }

        let replacing_generation = existing.iter().any(|container| {
            label(container.labels.as_ref(), LABEL_GENERATION)
                .and_then(|value| value.parse::<u64>().ok())
                != Some(command.fence.generation)
        });
        if replacing_generation {
            self.remove_containers(&existing).await?;
            self.remove_assignment_networks(command.fence.assignment_id)
                .await?;
        }
        let network = self
            .ensure_network(
                command.fence,
                &command.spec_hash,
                command.spec.platform.operating_system,
            )
            .await?;
        let expected_replicas = command
            .spec
            .services
            .iter()
            .map(|service| usize::from(service.replicas))
            .sum::<usize>();
        let mut current = self.existing_containers(command.fence.workload_id).await?;
        for service in &command.spec.services {
            let image = image_string(&service.image);
            for replica in 0..service.replicas {
                let found = current.iter().position(|container| {
                    labels_match(container.labels.as_ref(), command.fence, &command.spec_hash)
                        && label(container.labels.as_ref(), LABEL_SERVICE)
                            == Some(service.name.as_str())
                        && label(container.labels.as_ref(), LABEL_REPLICA)
                            .and_then(|v| v.parse::<u16>().ok())
                            == Some(replica)
                        && container_image_matches(container, &service.image, &image)
                });
                if let Some(index) = found {
                    self.start_if_stopped(&current[index]).await?;
                    current.swap_remove(index);
                } else {
                    self.create_container(
                        ContainerReplicaPlan {
                            fence: command.fence,
                            spec_hash: &command.spec_hash,
                            network: &network,
                            service,
                            expected_replicas,
                            operating_system: command.spec.platform.operating_system,
                        },
                        replica,
                    )
                    .await?;
                }
            }
        }
        // Matching but no-longer-requested replicas are safe to remove.
        let surplus = current
            .into_iter()
            .filter(|container| {
                labels_match(container.labels.as_ref(), command.fence, &command.spec_hash)
            })
            .collect::<Vec<_>>();
        self.remove_containers(&surplus).await?;
        self.invalidate_workload_endpoints(command.fence.workload_id);
        self.status_for(command.fence, command.spec_hash, Some(expected_replicas))
            .await
    }

    async fn ensure_absent(&self, command: EnsureAbsent) -> Result<WorkloadStatus, RuntimeError> {
        let record_tombstone = self
            .tombstones
            .validate_absent(command.fence, &command.spec_hash)
            .await?;
        let containers = self.existing_containers(command.fence.workload_id).await?;
        let mut matching = Vec::new();
        for container in containers {
            let labels = container.labels.as_ref();
            if label(labels, LABEL_ASSIGNMENT)
                != Some(command.fence.assignment_id.to_string()).as_deref()
            {
                // Inventory cleanup may target an orphaned old assignment while
                // a replacement with the same workload UUID is already live.
                continue;
            }
            let generation = label(labels, LABEL_GENERATION)
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| {
                    RuntimeError::new(
                        CommandErrorCode::StaleGeneration,
                        "refusing to delete a workload with an invalid generation fence",
                    )
                })?;
            if generation > command.fence.generation {
                return Err(RuntimeError::new(
                    CommandErrorCode::StaleGeneration,
                    "refusing to delete a newer workload generation",
                ));
            }
            if generation == command.fence.generation
                && label(labels, LABEL_SPEC_HASH) != Some(command.spec_hash.as_str())
            {
                return Err(RuntimeError::new(
                    CommandErrorCode::SpecConflict,
                    "refusing to delete the current generation with a different spec hash",
                ));
            }
            matching.push(container);
        }
        if record_tombstone {
            self.tombstones
                .record(command.fence, &command.spec_hash)
                .await?;
        }
        self.remove_containers(&matching).await?;
        self.remove_assignment_networks(command.fence.assignment_id)
            .await?;
        self.invalidate_workload_endpoints(command.fence.workload_id);
        self.flag_sequences
            .retain(|(workload_id, _, _, _), _| *workload_id != command.fence.workload_id);
        Ok(WorkloadStatus {
            fence: command.fence,
            spec_hash: command.spec_hash,
            state: ObservedWorkloadState::Absent,
            replicas: Vec::new(),
            detail: None,
        })
    }

    async fn write_flag(&self, command: WriteFlag) -> Result<WorkloadStatus, RuntimeError> {
        let sequence_key = (
            command.fence.workload_id,
            command.fence.assignment_id,
            command.fence.generation,
            command.target.path.clone(),
        );
        if let Some(current) = self.flag_sequences.get(&sequence_key) {
            if command.flag_sequence < *current {
                return Err(RuntimeError::new(
                    CommandErrorCode::StaleFlagSequence,
                    "a newer flag sequence is already installed",
                ));
            }
            if command.flag_sequence == *current {
                return self.current_status(command.fence).await;
            }
        }
        let containers = self
            .matching_container_ids(command.fence, &command.target.service)
            .await?;
        if containers.is_empty() {
            return Err(RuntimeError::new(
                CommandErrorCode::NotFound,
                "flag target has no ready replicas",
            ));
        }
        let (parent, filename) =
            split_guest_path(&command.target.path, self.platform().operating_system)?;
        let expected = command.value.into_bytes();
        let archive = make_single_file_archive(filename, expected.clone()).await?;
        let mut failed = Vec::new();
        for (replica, container_id) in &containers {
            let written = self
                .docker
                .upload_to_container(
                    container_id,
                    Some(UploadToContainerOptions {
                        path: parent.clone(),
                        no_overwrite_dir_non_dir: String::new(),
                    }),
                    archive.clone(),
                )
                .await
                .is_ok();
            let verified = if written {
                self.verify_guest_file(container_id, &command.target.path, &expected)
                    .await
                    .unwrap_or(false)
            } else {
                false
            };
            if !verified {
                failed.push(format!("{}:{replica}", command.target.service));
            }
        }
        if !failed.is_empty() {
            return Err(RuntimeError::new(
                CommandErrorCode::PartialFailure,
                "flag write failed for one or more replicas",
            )
            .with_failed_replicas(failed));
        }
        self.flag_sequences
            .insert(sequence_key, command.flag_sequence);
        self.current_status(command.fence).await
    }

    async fn open_tcp(&self, request: &TcpProxyRequest) -> Result<TcpStream, RuntimeError> {
        let targets = self.resolve_endpoints(request, false).await?;
        let target = self.select_endpoint(request, &targets)?;
        if let Ok(stream) = Self::dial_endpoint(target.address).await {
            return Ok(stream);
        }
        self.ready_containers.remove(&target.container_id);

        // A container restart can invalidate an IP between reconciliations.
        // Drop the fenced entry and retry once from Docker's current state.
        let key = Self::endpoint_key(request);
        self.endpoint_cache.remove(&key);
        let targets = self.resolve_endpoints(request, true).await?;
        let target = self.select_endpoint(request, &targets)?;
        let stream = Self::dial_endpoint(target.address).await;
        if stream.is_err() {
            self.ready_containers.remove(&target.container_id);
        }
        stream
    }
}
