//! services/container.rs — ported from RSCTF `Services/Container/*`.
//!
//! Container-orchestration abstraction layer. This is a pure library module
//! (no HTTP surface). It mirrors RSCTF's `Services/Container/Manager/IContainerManager`
//! which exposes create / destroy / stats over a pluggable backend (Docker or
//! Kubernetes). Here we define the async [`ContainerManager`] trait plus two
//! implementations: a [`NoopContainerManager`] (used when no backend is
//! configured) and a real [`DockerContainerManager`] backed by the `bollard`
//! crate.
//!
//! ## Docker flow (mirrors RSCTF `DockerManager.CreateContainerAsync`)
//!
//! 1. **Connect** — [`DockerContainerManager::connect`] talks to the local
//!    Docker daemon through `bollard::Docker::connect_with_local_defaults`
//!    (honours `DOCKER_HOST` / falls back to the unix socket).
//! 2. **Create** — for each per-instance challenge we:
//!    - best-effort pull the immutable repository digest (`create_image`
//!      streaming pull; a daemon-local image ID must already be present),
//!    - create a container with the memory limit (`HostConfig.memory`), the CPU
//!      quota (`HostConfig.nano_cpus`), a `PidsLimit`, the dynamic flag injected
//!      as the `RSCTF_FLAG` env var, the challenge port exposed and published to
//!      a daemon-chosen host port (`PortBinding { host_port: "0" }`), and an
//!      installation-scoped managed labels so orphans are identifiable without
//!      one rsctf deployment reaping another deployment's containers,
//!    - start it and inspect it to read back the published host IP/port and the
//!      live lifecycle state.
//! 3. **Destroy** — force-remove by id, treating "not found" as success (the
//!    container is already gone, which is the desired end state).
//! 4. **Query** — inspect the container and map the Docker state enum to a
//!    coarse lifecycle status.
//!
//! Backends are selected at startup by [`from_env`], which returns the Docker
//! manager when a daemon is reachable and the Noop manager otherwise.
//!
//! The Kubernetes backend (`KubernetesManager`) is ported in
//! [`crate::services::k8s`] — a `KubernetesContainerManager` implementing this
//! same [`ContainerManager`] trait (Pod + Service per instance via the `kube`
//! crate); `k8s::from_env` is tried first at startup, falling back to the
//! Docker manager here. The game/A&D controllers thread `st.containers` through
//! and call create/destroy/exec/snapshot_changes for the full instance lifecycle.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use bollard::container::NetworkingConfig;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
    StartContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{EndpointSettings, HostConfig, Ipam, IpamConfig, Network, PortBinding};
use bollard::network::CreateNetworkOptions;
use bollard::Docker;
use futures::StreamExt;
use ipnet::Ipv4Net;
use rsctf_worker_protocol::GameKind;

use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};

mod backend;
mod docker;
mod logging;
mod naming;
#[cfg(test)]
mod tests;
use logging::bounded_log_config;
use naming::{container_name, map_status};

pub use backend::{
    ContainerBackendKind, ContainerLiveness, ContainerManager, ContainerStatus, FileChange,
    NoopContainerManager,
};
pub use docker::{from_env, from_env_required};

/// Label stamped on every rsctf-managed container so orphans left behind by a
/// crash can be reaped by a sweeper (mirrors RSCTF tagging containers with
/// team/challenge metadata).
const MANAGED_LABEL: &str = "rsctf.managed";
const OPERATION_LABEL: &str = "rsctf.operation";
const SCOPE_LABEL: &str = "rsctf.scope";
const DOCKER_SCOPE_ENV: &str = "RSCTF_DOCKER_SCOPE";
const JWT_SECRET_ENV: &str = "RSCTF_JWT_SECRET";

/// Environment names injected into rsctf-managed challenge containers.
const FLAG_ENV: &str = "RSCTF_FLAG";
const FLAG_FILE_ENV: &str = "RSCTF_FLAG_FILE";
const FLAG_FILE_PATH: &str = "/flag";
const TEAM_ENV: &str = "RSCTF_TEAM_ID";
const DEFAULT_MAX_MEMORY_MB: i32 = 4_096;
const DEFAULT_MAX_CPU_COUNT: i32 = 8;
pub(super) const MAX_EXEC_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_SNAPSHOT_EXPORT_BYTES: usize = 64 * 1024 * 1024;
const SNAPSHOT_EXPORT_MAX_DURATION: Duration = Duration::from_secs(120);
const SNAPSHOT_EXPORT_ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONCURRENT_SNAPSHOT_EXPORTS: usize = 2;
const DOCKER_COMPETITIVE_EGRESS_ERROR: &str =
    "Docker does not safely support allowEgress=true for A&D or KotH workloads; \
     set allowEgress=false or use the Kubernetes backend with per-workload NetworkPolicy isolation";

fn snapshot_export_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_SNAPSHOT_EXPORTS))
}

fn append_snapshot_chunk(out: &mut Vec<u8>, chunk: &[u8], limit: usize) -> AppResult<()> {
    let next_len = out
        .len()
        .checked_add(chunk.len())
        .ok_or_else(|| AppError::bad_request("snapshot export size overflow"))?;
    if next_len > limit {
        return Err(AppError::bad_request(format!(
            "snapshot export exceeds the {} MiB safety limit",
            limit / (1024 * 1024)
        )));
    }
    out.try_reserve(chunk.len())
        .map_err(|_| AppError::internal("failed to reserve snapshot export buffer"))?;
    out.extend_from_slice(chunk);
    Ok(())
}

fn docker_workload_scope(explicit: Option<&str>, jwt_secret: Option<&str>) -> String {
    let (source, identity) = explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| ("explicit", value))
        .or_else(|| {
            jwt_secret
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| ("jwt", value))
        })
        // Normal startup rejects this fallback before a container backend is
        // used. Keeping it deterministic makes isolated manager tests useful.
        .unwrap_or(("development", "rsctf"));
    crate::utils::codec::sha256_str(&format!("{source}\0{identity}"))[..32].to_string()
}

fn scoped_managed_labels(scope: &str) -> HashMap<String, String> {
    HashMap::from([
        (MANAGED_LABEL.to_string(), scope.to_string()),
        (SCOPE_LABEL.to_string(), scope.to_string()),
    ])
}

fn scoped_operation_id(scope: &str, operation_id: Option<&str>) -> Option<String> {
    operation_id.map(|operation_id| format!("{scope}\0{operation_id}"))
}

fn managed_container_filters(scope: &str) -> HashMap<String, Vec<String>> {
    HashMap::from([(
        "label".to_string(),
        vec![
            format!("{MANAGED_LABEL}={scope}"),
            format!("{SCOPE_LABEL}={scope}"),
        ],
    )])
}

fn labels_match_scope(labels: Option<&HashMap<String, String>>, scope: &str) -> bool {
    labels.is_some_and(|labels| {
        labels.get(MANAGED_LABEL).map(String::as_str) == Some(scope)
            && labels.get(SCOPE_LABEL).map(String::as_str) == Some(scope)
    })
}

/// Legacy Compose-created bridges did not carry an rsctf scope label. Continue
/// to accept those after checking their exact name/subnet/internal shape, but a
/// bridge that declares ownership must belong to this installation.
fn network_scope_matches(existing: &Network, scope: &str) -> bool {
    existing
        .labels
        .as_ref()
        .and_then(|labels| labels.get(SCOPE_LABEL))
        .is_none_or(|actual| actual == scope)
}

fn bridge_network_matches(existing: &Network, subnet: Option<&str>, internal: bool) -> bool {
    let managed = existing
        .labels
        .as_ref()
        .and_then(|labels| labels.get(MANAGED_LABEL))
        .is_some();
    let subnet_matches = subnet.is_none_or(|expected| {
        let Ok(expected) = expected.parse::<Ipv4Net>() else {
            return false;
        };
        let actual: Vec<Ipv4Net> = existing
            .ipam
            .as_ref()
            .and_then(|ipam| ipam.config.as_ref())
            .into_iter()
            .flatten()
            .filter_map(|config| config.subnet.as_deref()?.parse::<Ipv4Net>().ok())
            .collect();
        actual.len() == 1 && actual[0] == expected
    });
    existing.driver.as_deref() == Some("bridge")
        && existing.internal == Some(internal)
        && (internal || managed)
        && subnet_matches
}

/// Requested container configuration.
///
/// Mirrors RSCTF `Models.Internal.ContainerConfig`: the challenge image, its
/// resource limits, the port the challenge exposes inside the container, any
/// injected environment variables, and the flag to bake into the environment
/// for dynamic-flag challenges.
#[derive(Debug, Clone)]
pub struct ContainerSpec {
    /// Competition semantics for routing. Network shape is not a safe proxy:
    /// admin tests and ended-game practice can omit the A&D services network.
    pub game_kind: GameKind,
    /// Immutable image reference: a repository digest or, for one Docker
    /// daemon, a content-addressed local image id.
    pub image: String,
    /// Hard memory limit in megabytes.
    pub memory_limit: i32,
    /// CPU quota expressed as a whole CPU count (0.1 CPU units in RSCTF).
    pub cpu_count: i32,
    /// Port the challenge process listens on inside the container.
    pub expose_port: i32,
    /// Additional environment variables injected at creation time.
    pub env: Vec<(String, String)>,
    /// Optional dynamic flag baked into the container environment.
    pub flag: Option<String>,
    /// A&D-over-VPN placement: the Docker network to join. When set, the container
    /// joins that network (Docker auto-assigns an IP) and publishes NO host ports —
    /// it's reachable only over the WireGuard tunnel. `ContainerInfo.ip` then
    /// carries the assigned in-VPN IP and `port` the container-internal expose port.
    pub ad_network: Option<String>,
    /// Whether an A&D/KotH container may use backend-isolated outbound access.
    /// Kubernetes enforces this with a per-workload NetworkPolicy. Docker
    /// rejects it because a shared external bridge cannot prevent east-west,
    /// private-network, or metadata access.
    pub allow_egress: bool,
    /// Stable lifecycle identity for crash-recoverable create operations. When
    /// present, a backend must adopt the matching existing workload instead of
    /// launching a second one after a retry.
    pub operation_id: Option<String>,
}

pub fn game_kind_for_challenge(challenge_type: ChallengeType) -> GameKind {
    match challenge_type {
        ChallengeType::AttackDefense => GameKind::AttackDefense,
        ChallengeType::KingOfTheHill => GameKind::KingOfTheHill,
        _ => GameKind::Jeopardy,
    }
}

impl ContainerSpec {
    /// Build the invariant placement for a platform-hosted A&D service: it joins
    /// the internal services network and is never published on a host port.
    /// Docker accepts only `allow_egress=false`; Kubernetes can enforce an
    /// allowed-egress policy per workload. Both the initial provision and every
    /// restart/reset must use this constructor.
    pub fn ad_service(
        image: String,
        memory_limit: i32,
        cpu_count: i32,
        expose_port: i32,
        team_id: i32,
        allow_egress: bool,
        flag: String,
    ) -> Self {
        Self {
            game_kind: GameKind::AttackDefense,
            image,
            memory_limit,
            cpu_count,
            expose_port,
            env: vec![(TEAM_ENV.into(), team_id.to_string())],
            flag: Some(flag),
            ad_network: Some(crate::services::ad_vpn::services_network()),
            allow_egress,
            operation_id: None,
        }
    }
}

fn configured_positive_limit(name: &str, default: i32) -> i32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

pub(crate) fn validate_container_spec(spec: &ContainerSpec) -> AppResult<()> {
    let max_memory =
        configured_positive_limit("RSCTF_CONTAINER_MAX_MEMORY_MB", DEFAULT_MAX_MEMORY_MB);
    let max_cpu = configured_positive_limit("RSCTF_CONTAINER_MAX_CPU_COUNT", DEFAULT_MAX_CPU_COUNT);
    if spec.image.trim().is_empty() {
        return Err(AppError::bad_request("container image is required"));
    }
    if !crate::services::challenge_images::is_repository_digest(&spec.image)
        && !crate::services::challenge_images::is_local_image_id(&spec.image)
        && crate::services::challenge_images::worker_local_image(&spec.image).is_none()
    {
        return Err(AppError::bad_request(
            "container image must be an immutable repository digest, Docker image id, or worker-scoped image id",
        ));
    }
    if !(1..=max_memory).contains(&spec.memory_limit) {
        return Err(AppError::bad_request(format!(
            "container memory must be between 1 and {max_memory} MB"
        )));
    }
    if !(1..=max_cpu).contains(&spec.cpu_count) {
        return Err(AppError::bad_request(format!(
            "container CPU count must be between 1 and {max_cpu}"
        )));
    }
    if !(1..=65_535).contains(&spec.expose_port) {
        return Err(AppError::bad_request(
            "container expose port must be between 1 and 65535",
        ));
    }
    Ok(())
}

fn validate_docker_container_spec(spec: &ContainerSpec) -> AppResult<()> {
    if spec.allow_egress
        && matches!(
            spec.game_kind,
            GameKind::AttackDefense | GameKind::KingOfTheHill
        )
    {
        return Err(AppError::bad_request(DOCKER_COMPETITIVE_EGRESS_ERROR));
    }
    validate_container_spec(spec)
}

/// Runtime information about a created / running container.
///
/// Mirrors the parts of RSCTF `Models.Data.Container` that callers need to
/// reach the running instance: its backend id, the routable IP, the mapped
/// public port, and a coarse status string.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    /// Backend-assigned container id (Docker id or K8s pod name).
    pub id: String,
    /// Routable IP address the proxy/user connects to.
    pub ip: String,
    /// Publicly mapped port.
    pub port: i32,
    /// Coarse lifecycle status, e.g. `pending` / `running` / `destroyed`.
    pub status: String,
}

/// Docker-backed container manager.
///
/// Wraps a `bollard::Docker` handle and implements the full create / destroy /
/// query lifecycle against the Docker Engine API (the Rust equivalent of
/// RSCTF's `DockerManager`, which uses `Docker.DotNet`).
#[derive(Debug, Default, Clone)]
pub struct DockerContainerManager {
    /// Docker daemon endpoint (unix socket path or `tcp://host:port`).
    ///
    /// Informational only — the live connection lives in [`Self::docker`].
    pub endpoint: Option<String>,
    /// Public host/IP that exposed container ports are advertised on. When set,
    /// [`ContainerInfo::ip`] is this value (matching RSCTF `PublicEntry`).
    pub public_entry: Option<String>,
    /// Hashed installation identity shared by replicas using the same Docker
    /// daemon. It prevents one deployment's orphan sweep or operation adoption
    /// from touching another deployment's workloads.
    scope: String,
    /// Live Docker client handle populated by [`Self::connect`].
    docker: Option<Docker>,
}

impl DockerContainerManager {
    /// Connect to the local Docker daemon via `connect_with_local_defaults`
    /// (honours `DOCKER_HOST`, otherwise the platform unix socket / named pipe).
    ///
    /// The optional public entry (advertised host for published ports) is read
    /// from `RSCTF_DOCKER_PUBLIC_ENTRY` if unset.
    pub fn connect() -> AppResult<Self> {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| AppError::internal(format!("failed to connect to docker daemon: {e}")))?;
        Ok(Self {
            endpoint: std::env::var("DOCKER_HOST").ok(),
            public_entry: std::env::var("RSCTF_DOCKER_PUBLIC_ENTRY").ok(),
            scope: docker_workload_scope(
                std::env::var(DOCKER_SCOPE_ENV).ok().as_deref(),
                std::env::var(JWT_SECRET_ENV).ok().as_deref(),
            ),
            docker: Some(docker),
        })
    }

    /// Borrow the live Docker handle.
    fn client(&self) -> AppResult<&Docker> {
        self.docker
            .as_ref()
            .ok_or_else(|| AppError::internal("docker manager is not connected"))
    }

    /// Probe daemon reachability with a short-timeout `ping`, driven on a
    /// dedicated thread + current-thread runtime so it is safe to call from a
    /// synchronous context regardless of whether an outer Tokio runtime is
    /// already active (avoids the "cannot start a runtime from within a
    /// runtime" panic).
    fn reachable_blocking(&self) -> bool {
        let Some(docker) = self.docker.clone() else {
            return false;
        };
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return false;
            };
            rt.block_on(async move {
                matches!(
                    tokio::time::timeout(Duration::from_secs(2), docker.ping()).await,
                    Ok(Ok(_))
                )
            })
        })
        .join()
        .unwrap_or(false)
    }

    async fn ensure_bridge_network(
        &self,
        name: &str,
        subnet: Option<&str>,
        internal: bool,
    ) -> AppResult<()> {
        let docker = self.client()?;
        if let Ok(existing) = docker
            .inspect_network(
                name,
                None::<bollard::network::InspectNetworkOptions<String>>,
            )
            .await
        {
            if bridge_network_matches(&existing, subnet, internal)
                && network_scope_matches(&existing, &self.scope)
            {
                return Ok(());
            }
            return Err(AppError::internal(format!(
                "Docker network {name} does not match the required bridge/Internal={internal}/subnet={subnet:?} configuration; recreate it before launching A&D services",
            )));
        }

        let ipam = match subnet {
            Some(subnet) => Ipam {
                config: Some(vec![IpamConfig {
                    subnet: Some(subnet.to_string()),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            None => Ipam::default(),
        };
        let opts = CreateNetworkOptions {
            name: name.to_string(),
            check_duplicate: true,
            driver: "bridge".to_string(),
            internal,
            ipam,
            labels: scoped_managed_labels(&self.scope),
            ..Default::default()
        };
        match docker.create_network(opts).await {
            Ok(_) => Ok(()),
            Err(create_error) => {
                // A concurrent provision may have won the create race.
                match docker
                    .inspect_network(
                        name,
                        None::<bollard::network::InspectNetworkOptions<String>>,
                    )
                    .await
                {
                    Ok(existing)
                        if bridge_network_matches(&existing, subnet, internal)
                            && network_scope_matches(&existing, &self.scope) =>
                    {
                        Ok(())
                    }
                    _ => Err(AppError::internal(format!(
                        "failed to create Docker network {name}: {create_error}"
                    ))),
                }
            }
        }
    }
}

/// Whether a bollard error is a Docker "404 Not Found" (container/image gone).
fn is_not_found(err: &bollard::errors::Error) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

/// Docker 409 Conflict — e.g. the container name is already taken.
fn is_conflict(err: &bollard::errors::Error) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            ..
        }
    )
}

#[async_trait]
impl ContainerManager for DockerContainerManager {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::Docker
    }

    async fn image_exists(&self, image: &str) -> bool {
        match self.client() {
            Ok(docker) => docker.inspect_image(image).await.is_ok(),
            Err(_) => false,
        }
    }

    async fn list_managed(&self) -> Vec<String> {
        let Ok(docker) = self.client() else {
            return Vec::new();
        };
        let opts = ListContainersOptions {
            all: true,
            filters: managed_container_filters(&self.scope),
            ..Default::default()
        };
        match docker.list_containers(Some(opts)).await {
            Ok(list) => list.into_iter().filter_map(|c| c.id).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "list_managed: docker list_containers failed");
                Vec::new()
            }
        }
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        validate_docker_container_spec(&spec)?;
        let docker = self.client()?;

        // 1. Pull the immutable reference only if it is not present locally.
        // Repository digests can be fetched without changing identity. A
        // daemon-local image ID cannot be reconstructed elsewhere; a missing ID
        // therefore falls through to create and surfaces the runtime error.
        if docker.inspect_image(&spec.image).await.is_err()
            && crate::services::challenge_images::is_repository_digest(&spec.image)
        {
            let options = CreateImageOptions {
                from_image: spec.image.clone(),
                ..Default::default()
            };
            let mut pull = docker.create_image(Some(options), None, None);
            while let Some(item) = pull.next().await {
                if let Err(e) = item {
                    tracing::warn!(image = %spec.image, error = %e, "image pull reported an error (continuing)");
                    break;
                }
            }
        }

        // 2. Environment: caller-supplied vars plus the dynamic flag contract.
        let mut env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        if let Some(flag) = spec.flag.as_deref() {
            if !flag.is_empty() {
                env.push(format!("{FLAG_ENV}={flag}"));
                env.push(format!("{FLAG_FILE_ENV}={FLAG_FILE_PATH}"));
            }
        }

        // 3. Port publishing: expose the challenge port. For a normal container we
        // bind it to host port "0" so the daemon picks a free ephemeral port. For
        // an A&D-over-VPN container (`ad_network`) we publish NO host port — the
        // service is reachable only via its in-VPN IP over the tunnel.
        let port_key = format!("{}/tcp", spec.expose_port);
        let exposed_ports: HashMap<String, HashMap<(), ()>> =
            HashMap::from([(port_key.clone(), HashMap::new())]);
        let port_bindings: Option<HashMap<String, Option<Vec<PortBinding>>>> =
            if spec.ad_network.is_some() {
                None
            } else {
                Some(HashMap::from([(
                    port_key.clone(),
                    Some(vec![PortBinding {
                        host_ip: Some("0.0.0.0".to_string()),
                        host_port: Some("0".to_string()),
                    }]),
                )]))
            };

        // 4. Resource limits: memory (MB → bytes), CPU quota (whole cores →
        // nano-cpus), and a pids cap to blunt fork bombs.
        let host_config = HostConfig {
            memory: Some(i64::from(spec.memory_limit) * 1024 * 1024),
            nano_cpus: Some(i64::from(spec.cpu_count) * 1_000_000_000),
            pids_limit: Some(512),
            log_config: Some(bounded_log_config()),
            port_bindings,
            ..Default::default()
        };

        let mut labels = scoped_managed_labels(&self.scope);
        if let Some(operation_id) = spec.operation_id.as_ref() {
            labels.insert(OPERATION_LABEL.to_string(), operation_id.clone());
        }

        // A&D-over-VPN always attaches only to the internal services bridge.
        // Docker allowEgress is rejected above: adding a shared external bridge
        // would permit cross-workload, private-network, and metadata access.
        let networking_config = if let Some(net) = spec.ad_network.as_ref() {
            let services_cidr = crate::services::ad_vpn::services_cidr();
            self.ensure_bridge_network(net, Some(services_cidr.as_str()), true)
                .await?;
            Some(NetworkingConfig {
                endpoints_config: HashMap::from([(net.clone(), EndpointSettings::default())]),
            })
        } else {
            None
        };

        let config = Config {
            image: Some(spec.image.clone()),
            env: Some(env),
            exposed_ports: Some(exposed_ports),
            labels: Some(labels),
            host_config: Some(host_config),
            networking_config,
            ..Default::default()
        };

        // 5. Create with a readable unique name. Never remove a 409 holder: without
        // an ownership proof it may be another user's live challenge container.
        let scoped_operation = scoped_operation_id(&self.scope, spec.operation_id.as_deref());
        let mut name = container_name(&spec.image, &spec.env, scoped_operation.as_deref());
        let (id, adopted) = match docker
            .create_container(
                Some(CreateContainerOptions::<String> {
                    name: name.clone(),
                    ..Default::default()
                }),
                config.clone(),
            )
            .await
        {
            Ok(created) => (created.id, false),
            Err(e) if is_conflict(&e) && spec.operation_id.is_some() => {
                let existing = docker
                    .inspect_container(&name, None)
                    .await
                    .map_err(|inspect| {
                        AppError::internal(format!(
                        "container operation {name} conflicted but could not be adopted: {inspect}"
                    ))
                    })?;
                let expected_operation = spec.operation_id.as_deref();
                let actual_operation = existing
                    .config
                    .as_ref()
                    .and_then(|config| config.labels.as_ref())
                    .and_then(|labels| labels.get(OPERATION_LABEL))
                    .map(String::as_str);
                let actual_image = existing
                    .config
                    .as_ref()
                    .and_then(|config| config.image.as_deref());
                let scope_matches = existing
                    .config
                    .as_ref()
                    .and_then(|config| config.labels.as_ref())
                    .is_some_and(|labels| labels_match_scope(Some(labels), &self.scope));
                if !scope_matches
                    || actual_operation != expected_operation
                    || actual_image != Some(spec.image.as_str())
                {
                    return Err(AppError::conflict(
                        "container operation identity is owned by a different workload",
                    ));
                }
                let id = existing.id.ok_or_else(|| {
                    AppError::internal("adopted container has no backend identity")
                })?;
                (id, true)
            }
            Err(e) if is_conflict(&e) => {
                name = container_name(&spec.image, &spec.env, None);
                let created = docker
                    .create_container(
                        Some(CreateContainerOptions::<String> {
                            name,
                            ..Default::default()
                        }),
                        config,
                    )
                    .await
                    .map_err(|e| AppError::internal(format!("failed to create container: {e}")))?;
                (created.id, false)
            }
            Err(e) => {
                return Err(AppError::internal(format!(
                    "failed to create container: {e}"
                )));
            }
        };
        // 6. Start.
        let already_running = adopted
            && docker
                .inspect_container(&id, None)
                .await
                .ok()
                .and_then(|info| info.state)
                .and_then(|state| state.running)
                == Some(true);
        if !already_running {
            if let Err(e) = docker
                .start_container(&id, None::<StartContainerOptions<String>>)
                .await
            {
                // Best-effort cleanup so a failed start doesn't leak a container.
                if !adopted {
                    let _ = docker
                        .remove_container(
                            &id,
                            Some(RemoveContainerOptions {
                                v: false,
                                force: true,
                                link: false,
                            }),
                        )
                        .await;
                }
                return Err(AppError::internal(format!(
                    "failed to start container: {e}"
                )));
            }
        }

        // 7. Inspect to read back state + the published host port.
        let info = docker
            .inspect_container(&id, None)
            .await
            .map_err(|e| AppError::internal(format!("failed to inspect container: {e}")))?;

        let status = map_status(info.state.as_ref().and_then(|s| s.status));

        // A&D-over-VPN: the endpoint is the container's in-VPN IP + internal port,
        // not a published host port. Read the Docker-assigned IP back from the net.
        if let Some(net) = &spec.ad_network {
            let ip = info
                .network_settings
                .as_ref()
                .and_then(|ns| ns.networks.as_ref())
                .and_then(|nets| nets.get(net))
                .and_then(|ep| ep.ip_address.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or_default();
            return Ok(ContainerInfo {
                id,
                ip,
                port: spec.expose_port,
                status: status.to_string(),
            });
        }

        // Published host port for the challenge's exposed port.
        let binding = info
            .network_settings
            .as_ref()
            .and_then(|ns| ns.ports.as_ref())
            .and_then(|ports| ports.get(&port_key))
            .and_then(|v| v.as_ref())
            .and_then(|v| v.first());

        let port = binding
            .and_then(|b| b.host_port.as_deref())
            .and_then(|p| p.parse::<i32>().ok())
            .unwrap_or(spec.expose_port);

        // Routable IP: prefer the configured public entry, then the binding's
        // host IP (unless it's the wildcard), then loopback. We deliberately do
        // NOT surface the container's *internal* network IP as the primary
        // endpoint — with published ports the reachable address is host-side.
        let ip = self
            .public_entry
            .clone()
            .or_else(|| {
                binding
                    .and_then(|b| b.host_ip.clone())
                    .filter(|h| !h.is_empty() && h != "0.0.0.0")
            })
            .unwrap_or_else(|| "127.0.0.1".to_string());

        Ok(ContainerInfo {
            id,
            ip,
            port,
            status: status.to_string(),
        })
    }

    async fn destroy(&self, id: &str) -> AppResult<()> {
        let docker = self.client()?;
        match docker
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    v: false,
                    force: true,
                    link: false,
                }),
            )
            .await
        {
            Ok(()) => Ok(()),
            // Already gone — that's the desired end state, treat as success.
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(AppError::internal(format!(
                "failed to remove container: {e}"
            ))),
        }
    }

    async fn ensure_network(&self, name: &str, subnet: &str) -> AppResult<()> {
        self.ensure_bridge_network(name, Some(subnet), true).await
    }

    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        let docker = self.client()?;
        let info = docker.inspect_container(id, None).await.map_err(|e| {
            if is_not_found(&e) {
                AppError::not_found(format!("container not found: {id}"))
            } else {
                AppError::internal(format!("failed to inspect container: {e}"))
            }
        })?;

        let status = map_status(info.state.as_ref().and_then(|s| s.status));

        // Resource sample from the Docker stats API. Degrades to `None` on any
        // failure (daemon gone, container stopped, malformed frame) so a stats
        // hiccup never turns a successful lifecycle query into an error.
        let (memory_bytes, cpu_usage) = self.sample_stats(id).await;

        Ok(ContainerStatus {
            id: id.to_string(),
            status: status.to_string(),
            memory_bytes,
            cpu_usage,
        })
    }

    /// Inspect-only liveness — no stats stream (unlike [`query`]).
    async fn inspect_liveness(&self, id: &str) -> AppResult<ContainerLiveness> {
        let docker = self.client()?;
        match docker.inspect_container(id, None).await {
            Ok(info) => Ok(docker::docker_liveness(
                info.state.as_ref().and_then(|state| state.status),
            )),
            Err(error) if is_not_found(&error) => Ok(ContainerLiveness::Stopped),
            Err(error) => Err(AppError::internal(format!(
                "failed to inspect container liveness: {error}"
            ))),
        }
    }

    /// RSCTF A&D snapshot diff: the container's filesystem changes vs its image,
    /// from the Docker `changes` API (`docker diff`).
    async fn snapshot_changes(&self, id: &str) -> AppResult<Vec<FileChange>> {
        let docker = self.client()?;
        let changes = docker.container_changes(id).await.map_err(|e| {
            if is_not_found(&e) {
                AppError::not_found(format!("container not found: {id}"))
            } else {
                AppError::internal(format!("failed to read container changes: {e}"))
            }
        })?;
        Ok(changes
            .unwrap_or_default()
            .into_iter()
            .map(|c| FileChange {
                path: c.path,
                // Docker Kind: 0 = Modified, 1 = Added, 2 = Deleted.
                kind: match c.kind as i64 {
                    0 => "Modified",
                    1 => "Added",
                    2 => "Deleted",
                    _ => "Unknown",
                }
                .to_string(),
            })
            .collect())
    }

    /// Exec a command in the container (KotH token plant/read-back), returning
    /// the combined output.
    async fn exec(&self, id: &str, cmd: Vec<String>) -> AppResult<String> {
        let docker = self.client()?;
        let exec = docker
            .create_exec(
                id,
                bollard::exec::CreateExecOptions {
                    cmd: Some(cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| AppError::internal(format!("create_exec: {e}")))?;
        let mut out = String::new();
        if let bollard::exec::StartExecResults::Attached { mut output, .. } = docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| AppError::internal(format!("start_exec: {e}")))?
        {
            while let Some(chunk) = output.next().await {
                if let Ok(msg) = chunk {
                    let rendered = msg.to_string();
                    if out.len().saturating_add(rendered.len()) > MAX_EXEC_OUTPUT_BYTES {
                        return Err(AppError::internal("container exec output exceeded 1 MiB"));
                    }
                    out.push_str(&rendered);
                }
            }
        }
        Ok(out)
    }

    /// Export the container's filesystem via the Docker `export` endpoint
    /// (`docker export`), folding the streamed TAR into a byte buffer. Used to
    /// serve the A&D post-game snapshot; the archive is uncompressed TAR.
    async fn export(&self, id: &str) -> AppResult<Vec<u8>> {
        let docker = self.client()?;
        let _permit = tokio::time::timeout(
            SNAPSHOT_EXPORT_ADMISSION_TIMEOUT,
            snapshot_export_slots().acquire(),
        )
        .await
        .map_err(|_| AppError::unavailable("snapshot export capacity is busy; retry shortly"))?
        .map_err(|_| AppError::unavailable("snapshot export service is shutting down"))?;

        tokio::time::timeout(SNAPSHOT_EXPORT_MAX_DURATION, async {
            let mut stream = docker.export_container(id);
            let mut out = Vec::new();
            while let Some(chunk) = stream.next().await {
                let bytes = chunk.map_err(|e| {
                    if is_not_found(&e) {
                        AppError::not_found(format!("container not found: {id}"))
                    } else {
                        AppError::internal(format!("failed to export container: {e}"))
                    }
                })?;
                append_snapshot_chunk(&mut out, &bytes, MAX_SNAPSHOT_EXPORT_BYTES)?;
            }
            Ok(out)
        })
        .await
        .map_err(|_| AppError::unavailable("snapshot export exceeded its 120 second limit"))?
    }
}
