use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use rsctf_worker_protocol::{
    EndpointRef, FlagTarget, GameKind, ImageIdentity, OperatingSystem, Platform, PortProtocol,
    ResourceLimits, ServicePort, ServiceSpec, ValidatedWorkloadSpec, WorkloadSpec,
};
use serde_json::json;
use uuid::Uuid;

use crate::services::challenge_images::{is_repository_digest, worker_local_image};
use crate::services::container::{
    validate_container_spec, ContainerBackendKind, ContainerInfo, ContainerLiveness,
    ContainerManager, ContainerSpec, ContainerStatus,
};
use crate::services::worker_store::{
    DefinitionUpdateOutcome, DesiredUpdateOutcome, PlaceWorkload, PlacementOutcome, PlatformOs,
    ResourceReservation, UpdateWorkload, WorkerStore, WorkerStoreError, WorkerWorkload,
    WorkloadDefinition, WorkloadDesiredState, WorkloadObservedState,
};
use crate::utils::error::{AppError, AppResult};

const HANDLE_PREFIX: &str = "rsctf-worker";
const DEFAULT_CREATE_TIMEOUT: Duration = Duration::from_secs(90);
const READY_POLL_INITIAL: Duration = Duration::from_millis(200);
const READY_POLL_MAX: Duration = Duration::from_secs(1);

fn require_jeopardy_game_kind(game_kind: GameKind) -> AppResult<()> {
    if game_kind != GameKind::Jeopardy {
        return Err(AppError::bad_request(
            "remote workers currently support Jeopardy containers only; configure a local Docker or Kubernetes backend for A&D/KotH",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerHandle {
    pub workload_id: Uuid,
    pub assignment_id: Uuid,
    pub generation: i64,
}

impl WorkerHandle {
    pub fn encode(self) -> String {
        format!(
            "{HANDLE_PREFIX}:{}:{}:{}",
            self.workload_id, self.assignment_id, self.generation
        )
    }
}

pub fn parse_worker_handle(value: &str) -> Option<WorkerHandle> {
    let mut parts = value.split(':');
    if parts.next()? != HANDLE_PREFIX {
        return None;
    }
    let handle = WorkerHandle {
        workload_id: parts.next()?.parse().ok()?,
        assignment_id: parts.next()?.parse().ok()?,
        generation: parts.next()?.parse().ok()?,
    };
    (parts.next().is_none() && handle.generation > 0).then_some(handle)
}

/// Compatibility adapter from RSCTF's existing one-container API to the
/// durable worker workload model. The wire/runtime model already supports
/// multiple services and stateless replicas; this adapter deliberately emits
/// one Jeopardy service so A&D/KotH keep their existing local lifecycle.
#[derive(Clone)]
pub struct WorkerContainerManager {
    store: WorkerStore,
    create_timeout: Duration,
}

impl WorkerContainerManager {
    pub fn new(store: WorkerStore) -> Self {
        let create_timeout = std::env::var("RSCTF_WORKER_CREATE_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_CREATE_TIMEOUT);
        Self {
            store,
            create_timeout,
        }
    }

    pub fn store(&self) -> &WorkerStore {
        &self.store
    }

    async fn current_rollout_workload(
        &self,
        handle: WorkerHandle,
    ) -> AppResult<Option<(WorkerWorkload, ValidatedWorkloadSpec)>> {
        let current = self
            .store
            .get_workload(handle.workload_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| AppError::not_found("worker workload not found"))?;
        if current.assignment_id != handle.assignment_id
            || current.desired_state != WorkloadDesiredState::Present
        {
            return Ok(None);
        }
        let current_spec =
            serde_json::from_value(current.definition.spec.clone()).map_err(|error| {
                AppError::internal(format!("invalid stored workload spec: {error}"))
            })?;
        Ok(Some((current, current_spec)))
    }

    /// Read-only safety pass used by the HTTP controller across every matched
    /// workload before the first generation is advanced. `rollout` repeats the
    /// same check to close the preflight-to-update race.
    pub async fn preflight_rollout(
        &self,
        handle: WorkerHandle,
        target: &ValidatedWorkloadSpec,
    ) -> AppResult<()> {
        let current = match self.current_rollout_workload(handle).await {
            Ok(current) => current,
            Err(AppError::NotFound(_)) => return Ok(()),
            Err(error) => return Err(error),
        };
        let Some((_, current_spec)) = current else {
            return Ok(());
        };
        ensure_stateless_rollout_transition(&current_spec, target)
    }

    /// Roll a configured aggregate definition onto one live workload without
    /// changing its durable placement or public container identity. Known
    /// request-scoped values are carried forward; the generation changes only
    /// as an internal command fence.
    pub async fn rollout(
        &self,
        handle: WorkerHandle,
        workload: ValidatedWorkloadSpec,
    ) -> AppResult<DefinitionUpdateOutcome> {
        let Some((current, current_spec)) = self.current_rollout_workload(handle).await? else {
            return Ok(DefinitionUpdateOutcome::Stale);
        };
        ensure_stateless_rollout_transition(&current_spec, &workload)?;
        let workload = specialize_rollout(workload, &current_spec)?;
        let (definition, _, exact_worker_id) = workload_definition(&workload)?;
        if exact_worker_id.is_some_and(|worker_id| worker_id != current.worker_id) {
            return Err(AppError::bad_request(
                "worker-local rollout images must belong to the workload's current worker",
            ));
        }
        self.store
            .update_workload_definition(&UpdateWorkload {
                id: current.id,
                assignment_id: current.assignment_id,
                expected_generation: current.generation,
                definition,
            })
            .await
            .map_err(store_error)
    }

    async fn wait_until_ready(&self, handle: WorkerHandle) -> AppResult<()> {
        let wait = async {
            let mut base_delay = READY_POLL_INITIAL;
            let mut attempt = 0_u64;
            loop {
                let workload = self
                    .store
                    .get_workload(handle.workload_id)
                    .await
                    .map_err(store_error)?
                    .ok_or_else(|| AppError::not_found("worker workload disappeared"))?;
                if workload.assignment_id != handle.assignment_id
                    || workload.generation != handle.generation
                {
                    return Err(AppError::conflict("worker workload was superseded"));
                }
                match workload.observed_state {
                    WorkloadObservedState::Ready
                        if workload.desired_state == WorkloadDesiredState::Present =>
                    {
                        return Ok(())
                    }
                    WorkloadObservedState::Failed => {
                        return Err(AppError::unavailable(
                            workload
                                .observed_message
                                .unwrap_or_else(|| "worker failed to start the workload".into()),
                        ));
                    }
                    WorkloadObservedState::Absent
                        if workload.desired_state == WorkloadDesiredState::Absent =>
                    {
                        return Err(AppError::conflict(
                            "worker workload was removed while it was starting",
                        ));
                    }
                    _ => {
                        tokio::time::sleep(jittered_ready_poll(
                            base_delay,
                            handle.workload_id,
                            attempt,
                        ))
                        .await;
                        base_delay = next_ready_poll(base_delay);
                        attempt = attempt.saturating_add(1);
                    }
                }
            }
        };
        tokio::time::timeout(self.create_timeout, wait)
            .await
            .map_err(|_| AppError::unavailable("timed out waiting for a worker workload"))?
    }

    async fn platform_for(
        &self,
        exact_worker_id: Option<Uuid>,
    ) -> AppResult<(Platform, PlatformOs)> {
        if let Some(worker_id) = exact_worker_id {
            let worker = self
                .store
                .get_worker(worker_id)
                .await
                .map_err(store_error)?
                .ok_or_else(|| AppError::bad_request("worker-local image owner was not found"))?;
            let os = worker.platform_os.ok_or_else(|| {
                AppError::unavailable("worker-local image owner has never connected")
            })?;
            let architecture = worker.architecture.ok_or_else(|| {
                AppError::unavailable("worker-local image owner has no platform inventory")
            })?;
            return Ok((platform(os, architecture), os));
        }

        let os = match std::env::var("RSCTF_WORKER_DEFAULT_OS")
            .unwrap_or_else(|_| "linux".into())
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "linux" => PlatformOs::Linux,
            "windows" => PlatformOs::Windows,
            _ => {
                return Err(AppError::bad_request(
                    "RSCTF_WORKER_DEFAULT_OS must be linux or windows",
                ))
            }
        };
        let architecture =
            std::env::var("RSCTF_WORKER_DEFAULT_ARCH").unwrap_or_else(|_| "amd64".into());
        Ok((platform(os, architecture), os))
    }

    async fn place_validated(
        &self,
        workload: ValidatedWorkloadSpec,
        operation_id: Option<String>,
        flag: Option<String>,
    ) -> AppResult<ContainerInfo> {
        let workload = crate::services::challenge_workloads::with_flag(workload, flag)?;
        let (definition, spec_hash_sha256, exact_worker_id) = workload_definition(&workload)?;
        let endpoint_port = workload
            .services
            .iter()
            .find(|service| service.name == workload.primary_endpoint.service)
            .and_then(|service| {
                service
                    .ports
                    .iter()
                    .find(|port| port.name == workload.primary_endpoint.port)
            })
            .map(|port| i32::from(port.container_port))
            .expect("validated workload primary endpoint exists");
        let workload_id = Uuid::new_v4();
        let assignment_id = Uuid::new_v4();
        let owner_key = operation_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| workload_id.to_string());
        let placed = self
            .store
            .place_workload(&PlaceWorkload {
                id: workload_id,
                owner_kind: "container".into(),
                owner_key,
                assignment_id,
                definition,
                exact_worker_id,
                required_labels: json!({}),
            })
            .await
            .map_err(store_error)?;
        let (workload, owns_placement) = match placed {
            PlacementOutcome::Placed(placement) => (placement.workload, true),
            PlacementOutcome::AlreadyExists(workload) => {
                if workload.definition.spec_hash_sha256 != spec_hash_sha256
                    || workload.desired_state != WorkloadDesiredState::Present
                {
                    return Err(AppError::conflict(
                        "container operation identity belongs to a different worker workload",
                    ));
                }
                (workload, false)
            }
            PlacementOutcome::NoCompatibleCapacity => {
                return Err(AppError::unavailable(
                    "no connected worker has compatible free capacity",
                ));
            }
        };
        let handle = WorkerHandle {
            workload_id: workload.id,
            assignment_id: workload.assignment_id,
            generation: workload.generation,
        };
        if let Err(error) = self.wait_until_ready(handle).await {
            // Only the caller that created the durable placement owns failure
            // cleanup. An idempotent concurrent waiter must not tear down an
            // operation it merely adopted.
            if owns_placement {
                if let Err(cleanup_error) = self
                    .store
                    .mark_desired_absent(
                        handle.workload_id,
                        handle.assignment_id,
                        handle.generation,
                    )
                    .await
                {
                    tracing::warn!(
                        workload_id = %handle.workload_id,
                        %cleanup_error,
                        "failed to fence an unsuccessful worker workload"
                    );
                }
            }
            return Err(error);
        }
        Ok(ContainerInfo {
            id: handle.encode(),
            ip: "worker".into(),
            port: endpoint_port,
            status: "running".into(),
        })
    }
}

fn next_ready_poll(current: Duration) -> Duration {
    current
        .saturating_mul(3)
        .checked_div(2)
        .unwrap_or(READY_POLL_MAX)
        .min(READY_POLL_MAX)
}

fn jittered_ready_poll(base: Duration, workload_id: Uuid, attempt: u64) -> Duration {
    let mut seed = attempt.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for chunk in workload_id.as_bytes().chunks_exact(8) {
        seed ^= u64::from_le_bytes(chunk.try_into().expect("UUID chunk is eight bytes"));
        seed = seed.rotate_left(17).wrapping_mul(0x94d0_49bb_1331_11eb);
    }
    let percent = 80 + seed % 41;
    Duration::from_millis(
        base.as_millis()
            .saturating_mul(u128::from(percent))
            .checked_div(100)
            .unwrap_or(1)
            .max(1)
            .min(u128::from(u64::MAX)) as u64,
    )
}

#[async_trait]
impl ContainerManager for WorkerContainerManager {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::Worker
    }

    fn requires_proxy(&self) -> bool {
        true
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        require_jeopardy_game_kind(spec.game_kind)?;
        validate_container_spec(&spec)?;
        if spec.ad_network.is_some() {
            return Err(AppError::bad_request(
                "remote-worker A&D/KotH networking is not enabled; use the local Docker or Kubernetes backend",
            ));
        }
        let (image, exact_worker_id) = image_identity(&spec.image)?;
        let (platform, _) = self.platform_for(exact_worker_id).await?;
        let environment = spec.env.into_iter().collect::<BTreeMap<_, _>>();
        let memory_bytes = u64::try_from(spec.memory_limit)
            .unwrap_or_default()
            .saturating_mul(1024 * 1024);
        let cpu_millis = u32::try_from(spec.cpu_count)
            .unwrap_or_default()
            .saturating_mul(1_000);
        let workload = ValidatedWorkloadSpec::try_from(WorkloadSpec {
            game_kind: GameKind::Jeopardy,
            platform,
            services: vec![ServiceSpec {
                name: "challenge".into(),
                image,
                resources: ResourceLimits {
                    cpu_millis,
                    memory_bytes,
                },
                replicas: 1,
                stateless: false,
                environment,
                ports: vec![ServicePort {
                    name: "service".into(),
                    container_port: u16::try_from(spec.expose_port)
                        .map_err(|_| AppError::bad_request("invalid container port"))?,
                    protocol: PortProtocol::Tcp,
                }],
            }],
            primary_endpoint: EndpointRef {
                service: "challenge".into(),
                port: "service".into(),
            },
            flag_target: Some(FlagTarget {
                service: "challenge".into(),
                path: "/flag".into(),
            }),
        })
        .map_err(|error| AppError::bad_request(error.to_string()))?;
        self.place_validated(workload, spec.operation_id, spec.flag)
            .await
    }

    async fn create_workload(
        &self,
        spec: ValidatedWorkloadSpec,
        operation_id: Option<String>,
        flag: Option<String>,
    ) -> AppResult<ContainerInfo> {
        require_jeopardy_game_kind(spec.game_kind)?;
        self.place_validated(spec, operation_id, flag).await
    }

    async fn destroy(&self, id: &str) -> AppResult<()> {
        let handle = parse_worker_handle(id)
            .ok_or_else(|| AppError::bad_request("invalid worker container identity"))?;
        for _ in 0..2 {
            let workload = self
                .store
                .get_workload(handle.workload_id)
                .await
                .map_err(store_error)?
                .ok_or_else(|| AppError::not_found("worker workload not found"))?;
            if workload.assignment_id != handle.assignment_id {
                return Err(AppError::not_found("worker workload was reassigned"));
            }
            if workload.desired_state == WorkloadDesiredState::Absent {
                return Ok(());
            }
            match self
                .store
                .mark_desired_absent(workload.id, workload.assignment_id, workload.generation)
                .await
                .map_err(store_error)?
            {
                DesiredUpdateOutcome::Updated { .. } => return Ok(()),
                DesiredUpdateOutcome::Stale => continue,
            }
        }
        Err(AppError::conflict(
            "worker workload changed while it was being removed",
        ))
    }

    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        let handle = parse_worker_handle(id)
            .ok_or_else(|| AppError::bad_request("invalid worker container identity"))?;
        let workload = self
            .store
            .get_workload(handle.workload_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| AppError::not_found("worker workload not found"))?;
        if workload.assignment_id != handle.assignment_id {
            return Err(AppError::not_found("worker workload was reassigned"));
        }
        let status = match workload.observed_state {
            WorkloadObservedState::Ready
                if workload.desired_state == WorkloadDesiredState::Present =>
            {
                "running"
            }
            WorkloadObservedState::Absent => "destroyed",
            WorkloadObservedState::Failed => "exited",
            WorkloadObservedState::Lost => "unknown",
            WorkloadObservedState::Unknown
            | WorkloadObservedState::Reconciling
            | WorkloadObservedState::Degraded
            | WorkloadObservedState::Ready => "pending",
        };
        Ok(ContainerStatus {
            id: id.to_string(),
            status: status.into(),
            memory_bytes: None,
            cpu_usage: None,
        })
    }

    async fn inspect_liveness(&self, id: &str) -> AppResult<ContainerLiveness> {
        let status = self.query(id).await?;
        Ok(match status.status.as_str() {
            "running" => ContainerLiveness::Running,
            "destroyed" | "exited" => ContainerLiveness::Stopped,
            _ => ContainerLiveness::Unknown,
        })
    }
}

fn specialize_rollout(
    workload: ValidatedWorkloadSpec,
    current: &ValidatedWorkloadSpec,
) -> AppResult<ValidatedWorkloadSpec> {
    let team_id = runtime_environment_value(current, "RSCTF_TEAM_ID")?;
    let flag = runtime_environment_value(current, "RSCTF_FLAG")?;
    let workload = match team_id {
        Some(team_id) => crate::services::challenge_workloads::with_environment(
            workload,
            "RSCTF_TEAM_ID",
            team_id,
        )?,
        None => workload,
    };
    crate::services::challenge_workloads::with_flag(workload, flag)
}

fn ensure_stateless_rollout_transition(
    current: &ValidatedWorkloadSpec,
    target: &ValidatedWorkloadSpec,
) -> AppResult<()> {
    crate::services::challenge_workloads::ensure_live_rollout_is_stateless(current)?;
    crate::services::challenge_workloads::ensure_live_rollout_is_stateless(target)
}

fn runtime_environment_value(
    workload: &ValidatedWorkloadSpec,
    key: &str,
) -> AppResult<Option<String>> {
    let mut value = None;
    for candidate in workload
        .services
        .iter()
        .filter_map(|service| service.environment.get(key))
    {
        if value
            .as_ref()
            .is_some_and(|current: &String| current != candidate)
        {
            return Err(AppError::internal(format!(
                "stored workload has conflicting {key} values"
            )));
        }
        value = Some(candidate.clone());
    }
    Ok(value)
}

fn workload_definition(
    workload: &ValidatedWorkloadSpec,
) -> AppResult<(WorkloadDefinition, [u8; 32], Option<Uuid>)> {
    let exact_worker_id = exact_worker(workload)?;
    let spec_hash = workload
        .spec_hash()
        .map_err(|error| AppError::internal(error.to_string()))?;
    let spec_hash_sha256: [u8; 32] = hex::decode(spec_hash)
        .map_err(|error| AppError::internal(error.to_string()))?
        .try_into()
        .map_err(|_| AppError::internal("invalid worker specification hash length"))?;
    Ok((
        WorkloadDefinition {
            spec: serde_json::to_value(workload)
                .map_err(|error| AppError::internal(error.to_string()))?,
            spec_hash_sha256,
            required_os: platform_os(workload.platform.operating_system),
            required_architecture: workload.platform.architecture.clone(),
            required_runtime: "docker".into(),
            reservation: aggregate_reservation(workload)?,
        },
        spec_hash_sha256,
        exact_worker_id,
    ))
}

fn exact_worker(workload: &ValidatedWorkloadSpec) -> AppResult<Option<Uuid>> {
    let mut exact = None;
    for service in &workload.services {
        let ImageIdentity::WorkerLocal { worker_id, .. } = &service.image else {
            continue;
        };
        if exact.is_some_and(|existing| existing != *worker_id) {
            return Err(AppError::bad_request(
                "one workload cannot use worker-local images from different workers",
            ));
        }
        exact = Some(*worker_id);
    }
    Ok(exact)
}

fn aggregate_reservation(workload: &ValidatedWorkloadSpec) -> AppResult<ResourceReservation> {
    let mut cpu_millis = 0_i64;
    let mut memory_bytes = 0_i64;
    for service in &workload.services {
        let replicas = i64::from(service.replicas);
        let service_cpu = i64::from(service.resources.cpu_millis)
            .checked_mul(replicas)
            .ok_or_else(|| AppError::bad_request("aggregate workload CPU is too large"))?;
        cpu_millis = cpu_millis
            .checked_add(service_cpu)
            .ok_or_else(|| AppError::bad_request("aggregate workload CPU is too large"))?;
        let service_memory = i64::try_from(service.resources.memory_bytes)
            .map_err(|_| AppError::bad_request("aggregate workload memory is too large"))?;
        let service_memory = service_memory
            .checked_mul(replicas)
            .ok_or_else(|| AppError::bad_request("aggregate workload memory is too large"))?;
        memory_bytes = memory_bytes
            .checked_add(service_memory)
            .ok_or_else(|| AppError::bad_request("aggregate workload memory is too large"))?;
    }
    Ok(ResourceReservation {
        cpu_millis,
        memory_bytes,
        // Docker creates one isolated network per workload; replicas consume
        // CPU, memory, and network endpoints, but not additional networks.
        slots: 1,
    })
}

fn platform_os(os: OperatingSystem) -> PlatformOs {
    match os {
        OperatingSystem::Linux => PlatformOs::Linux,
        OperatingSystem::Windows => PlatformOs::Windows,
    }
}

fn image_identity(reference: &str) -> AppResult<(ImageIdentity, Option<Uuid>)> {
    if let Some((worker_id, image_id)) = worker_local_image(reference) {
        return Ok((
            ImageIdentity::WorkerLocal {
                worker_id,
                image_id: image_id.to_string(),
            },
            Some(worker_id),
        ));
    }
    if is_repository_digest(reference) {
        let (repository, digest) = reference
            .trim()
            .rsplit_once('@')
            .expect("validated repository digest has a separator");
        return Ok((
            ImageIdentity::RegistryDigest {
                repository: repository.into(),
                digest: digest.into(),
            },
            None,
        ));
    }
    Err(AppError::bad_request(
        "worker workloads require a repository digest or worker-scoped local image",
    ))
}

fn platform(os: PlatformOs, architecture: String) -> Platform {
    Platform {
        operating_system: match os {
            PlatformOs::Linux => OperatingSystem::Linux,
            PlatformOs::Windows => OperatingSystem::Windows,
        },
        architecture,
        windows_build: None,
    }
}

fn store_error(error: WorkerStoreError) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aggregate_workload(flag_target: bool) -> ValidatedWorkloadSpec {
        let services = vec![
            ServiceSpec {
                name: "app".into(),
                image: ImageIdentity::RegistryDigest {
                    repository: "registry.example/ctf/app".into(),
                    digest: format!("sha256:{}", "a".repeat(64)),
                },
                resources: ResourceLimits {
                    cpu_millis: 500,
                    memory_bytes: 128 * 1024 * 1024,
                },
                replicas: 3,
                stateless: true,
                environment: BTreeMap::new(),
                ports: vec![ServicePort {
                    name: "http".into(),
                    container_port: 8080,
                    protocol: PortProtocol::Tcp,
                }],
            },
            ServiceSpec {
                name: "cache".into(),
                image: ImageIdentity::RegistryDigest {
                    repository: "registry.example/ctf/cache".into(),
                    digest: format!("sha256:{}", "b".repeat(64)),
                },
                resources: ResourceLimits {
                    cpu_millis: 250,
                    memory_bytes: 64 * 1024 * 1024,
                },
                replicas: 1,
                stateless: false,
                environment: BTreeMap::new(),
                ports: vec![ServicePort {
                    name: "cache".into(),
                    container_port: 6379,
                    protocol: PortProtocol::Tcp,
                }],
            },
        ];
        ValidatedWorkloadSpec::try_from(WorkloadSpec {
            game_kind: GameKind::Jeopardy,
            platform: Platform {
                operating_system: OperatingSystem::Linux,
                architecture: "amd64".into(),
                windows_build: None,
            },
            services,
            primary_endpoint: EndpointRef {
                service: "app".into(),
                port: "http".into(),
            },
            flag_target: flag_target.then(|| FlagTarget {
                service: "app".into(),
                path: "/flag".into(),
            }),
        })
        .unwrap()
    }

    fn all_stateless(workload: ValidatedWorkloadSpec) -> ValidatedWorkloadSpec {
        let mut workload = workload.into_inner();
        for service in &mut workload.services {
            service.stateless = true;
        }
        ValidatedWorkloadSpec::try_from(workload).unwrap()
    }

    #[test]
    fn handles_are_strict_and_round_trip() {
        let expected = WorkerHandle {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 7,
        };
        assert_eq!(parse_worker_handle(&expected.encode()), Some(expected));
        assert!(parse_worker_handle("rsctf-worker:not-a-uuid:x:1").is_none());
        assert!(parse_worker_handle(&format!("{}:extra", expected.encode())).is_none());
    }

    #[test]
    fn pure_remote_backend_rejects_competitive_game_kinds() {
        assert!(require_jeopardy_game_kind(GameKind::AttackDefense).is_err());
        assert!(require_jeopardy_game_kind(GameKind::KingOfTheHill).is_err());
        assert!(require_jeopardy_game_kind(GameKind::Jeopardy).is_ok());
    }

    #[test]
    fn worker_local_images_pin_placement() {
        let worker = Uuid::new_v4();
        let reference = format!("worker://{worker}/sha256:{}", "a".repeat(64));
        let (image, exact) = image_identity(&reference).unwrap();
        assert!(matches!(image, ImageIdentity::WorkerLocal { .. }));
        assert_eq!(exact, Some(worker));
    }

    #[test]
    fn aggregate_capacity_counts_replica_resources_and_one_workload_slot() {
        let reservation = aggregate_reservation(&aggregate_workload(true)).unwrap();
        assert_eq!(reservation.cpu_millis, 1_750);
        assert_eq!(reservation.memory_bytes, 448 * 1024 * 1024);
        assert_eq!(reservation.slots, 1);
    }

    #[test]
    fn runtime_flags_require_and_use_one_declared_target() {
        let missing = crate::services::challenge_workloads::with_flag(
            aggregate_workload(false),
            Some("flag".into()),
        );
        assert!(missing.is_err());

        let injected = crate::services::challenge_workloads::with_flag(
            aggregate_workload(true),
            Some("flag".into()),
        )
        .unwrap();
        let app = injected
            .services
            .iter()
            .find(|service| service.name == "app")
            .unwrap();
        let cache = injected
            .services
            .iter()
            .find(|service| service.name == "cache")
            .unwrap();
        assert_eq!(
            app.environment.get("RSCTF_FLAG").map(String::as_str),
            Some("flag")
        );
        assert!(!cache.environment.contains_key("RSCTF_FLAG"));
    }

    #[test]
    fn rollout_preserves_request_scoped_values_and_applies_replica_changes() {
        let current = crate::services::challenge_workloads::with_flag(
            crate::services::challenge_workloads::with_environment(
                aggregate_workload(true),
                "RSCTF_TEAM_ID",
                "42",
            )
            .unwrap(),
            Some("RSCTF{current}".into()),
        )
        .unwrap();
        let mut next = aggregate_workload(true).into_inner();
        next.services[0].replicas = 2;
        next.services[0]
            .environment
            .insert("CONFIGURED".into(), "new".into());
        let next = ValidatedWorkloadSpec::try_from(next).unwrap();

        let rolled = specialize_rollout(next, &current).unwrap();
        assert!(rolled.services.iter().all(|service| {
            service.environment.get("RSCTF_TEAM_ID").map(String::as_str) == Some("42")
        }));
        let app = rolled
            .services
            .iter()
            .find(|service| service.name == "app")
            .unwrap();
        assert_eq!(
            app.environment.get("RSCTF_FLAG").map(String::as_str),
            Some("RSCTF{current}")
        );
        assert_eq!(
            app.environment.get("CONFIGURED").map(String::as_str),
            Some("new")
        );
        assert_eq!(app.replicas, 2);
    }

    #[test]
    fn rollout_transition_rejects_stateful_current_or_target() {
        let stateful = aggregate_workload(true);
        let stateless = all_stateless(aggregate_workload(true));
        assert!(ensure_stateless_rollout_transition(&stateless, &stateless).is_ok());
        assert!(ensure_stateless_rollout_transition(&stateful, &stateless).is_err());
        assert!(ensure_stateless_rollout_transition(&stateless, &stateful).is_err());
    }

    #[test]
    fn readiness_polling_backs_off_to_one_second_with_stable_jitter() {
        let mut delay = READY_POLL_INITIAL;
        let mut sequence = Vec::new();
        for _ in 0..6 {
            sequence.push(delay);
            delay = next_ready_poll(delay);
        }
        assert_eq!(
            sequence,
            vec![
                Duration::from_millis(200),
                Duration::from_millis(300),
                Duration::from_millis(450),
                Duration::from_millis(675),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ]
        );

        let workload = Uuid::new_v4();
        let first = jittered_ready_poll(Duration::from_secs(1), workload, 3);
        assert_eq!(
            first,
            jittered_ready_poll(Duration::from_secs(1), workload, 3)
        );
        assert!((Duration::from_millis(800)..=Duration::from_millis(1_200)).contains(&first));
    }
}
