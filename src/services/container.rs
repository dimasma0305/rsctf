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
//! Docker uses `bollard` through the local daemon and preserves managed ownership.
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

use async_trait::async_trait;
use bollard::container::NetworkingConfig;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, RemoveContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{EndpointSettings, HostConfig, Ipam, IpamConfig, Network, PortBinding};
use bollard::network::CreateNetworkOptions;
use bollard::Docker;
use futures::StreamExt;
use ipnet::Ipv4Net;
use rsctf_worker_protocol::GameKind;
use std::collections::HashMap;
use std::time::Duration;

use crate::utils::enums::{ChallengeType, NetworkMode};
use crate::utils::error::{AppError, AppResult};
mod backend;
mod docker;
mod logging;
mod naming;
mod policy;
#[cfg(test)]
mod tests;
use self::docker::{
    append_snapshot_chunk, docker_network_mode, image_requests_restricted_profile, is_conflict,
    is_not_found, launch_spec_fingerprint, launch_spec_matches, restricted_profile_matches,
    restricted_tmpfs_mounts, snapshot_export_slots, stamp_restricted_profile,
    stamp_storage_quota_policy, storage_quota_policy_matches, validate_docker_container_spec,
    writable_layer_quota_supported, writable_layer_storage_option, LAUNCH_SPEC_LABEL,
    MAX_SNAPSHOT_EXPORT_BYTES, SNAPSHOT_EXPORT_ADMISSION_TIMEOUT, SNAPSHOT_EXPORT_MAX_DURATION,
};
pub use backend::{
    should_use_platform_proxy, ContainerBackendKind, ContainerExecAdmission, ContainerExecError,
    ContainerFile, ContainerLiveness, ContainerManager, ContainerStatus, FileChange,
    ManagedContainerPage, NoopContainerManager,
};
pub use docker::{from_env, from_env_required};
use logging::bounded_log_config;
use naming::{container_name, map_status};
pub(crate) use policy::validate_container_spec;
pub use policy::{
    storage_limit_or_default, validate_network_mode_value, validate_storage_limit_value,
};

/// Label stamped on every rsctf-managed container so orphans left behind by a
/// crash can be reaped by a sweeper (mirrors RSCTF tagging containers with
/// team/challenge metadata).
const MANAGED_LABEL: &str = "rsctf.managed";
const OPERATION_LABEL: &str = "rsctf.operation";
const SCOPE_LABEL: &str = "rsctf.scope";
/// Ownership metadata stamped on images built from rsctf-managed archives.
/// Pulled and legacy images deliberately carry neither label and therefore
/// remain outside the admin image-deletion boundary.
pub(crate) const IMAGE_SCOPE_LABEL: &str = "rsctf.image.scope";
pub(crate) const IMAGE_REFERENCE_LABEL: &str = "rsctf.image.ref";
const DOCKER_SCOPE_ENV: &str = "RSCTF_DOCKER_SCOPE";
const JWT_SECRET_ENV: &str = "RSCTF_JWT_SECRET";
/// Environment names injected into rsctf-managed challenge containers.
const FLAG_ENV: &str = "RSCTF_FLAG";
const FLAG_FILE_ENV: &str = "RSCTF_FLAG_FILE";
const FLAG_FILE_PATH: &str = "/flag";
const TEAM_ENV: &str = "RSCTF_TEAM_ID";
const DEFAULT_MAX_MEMORY_MB: i32 = 4_096;
const DEFAULT_MAX_CPU_COUNT: i32 = 8;
pub const DEFAULT_CONTAINER_STORAGE_MB: i32 = 512;
const DEFAULT_MAX_STORAGE_MB: i32 = 1_048_576;
pub(super) const MAX_EXEC_OUTPUT_BYTES: usize = 1024 * 1024;
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

/// Stable, non-secret installation identity shared by replicas that address
/// one Docker daemon. Image administration uses the same boundary as managed
/// containers and networks so two databases cannot claim each other's tags.
pub(crate) fn docker_installation_scope() -> String {
    docker_workload_scope(
        std::env::var(DOCKER_SCOPE_ENV).ok().as_deref(),
        std::env::var(JWT_SECRET_ENV).ok().as_deref(),
    )
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

pub(crate) fn managed_container_filters(scope: &str) -> HashMap<String, Vec<String>> {
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

fn bridge_network_matches(
    existing: &Network,
    subnet: Option<&str>,
    internal: bool,
    disable_icc: bool,
) -> bool {
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
    let icc_matches = !disable_icc
        || existing.options.as_ref().is_some_and(|options| {
            options
                .get("com.docker.network.bridge.enable_icc")
                .is_some_and(|value| value.eq_ignore_ascii_case("false"))
        });
    existing.driver.as_deref() == Some("bridge")
        && existing.internal == Some(internal)
        && (internal || managed)
        && subnet_matches
        && icc_matches
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
    /// Hard writable-layer limit in mebibytes.
    pub storage_limit: i32,
    /// Port the challenge process listens on inside the container.
    pub expose_port: i32,
    /// Whether the backend may publish the exposed port outside the workload.
    /// Interactive inspectors disable this because their sole entry point is
    /// the authenticated exec hub. Docker also gives a no-publish workload no
    /// network when `ad_network` is absent, preventing default-bridge egress.
    pub publish_port: bool,
    /// Bind a published Jeopardy port only to the configured private proxy entry.
    pub proxy_only: bool,
    /// Additional environment variables injected at creation time.
    pub env: Vec<(String, String)>,
    /// Optional dynamic flag baked into the container environment.
    pub flag: Option<String>,
    /// A&D-over-VPN Docker network. It publishes no host ports and exposes the
    /// assigned VPN address and internal port through `ContainerInfo`.
    pub ad_network: Option<String>,
    /// Whether an A&D/KotH container may use backend-isolated outbound access.
    /// Kubernetes enforces this with a per-workload NetworkPolicy. Docker
    /// rejects it because a shared external bridge cannot prevent east-west,
    /// private-network, or metadata access.
    pub allow_egress: bool,
    /// Service and target-pod ports for narrow Kubernetes callback egress.
    pub control_plane_callback_ports: Vec<i32>,
    /// Author-selected network isolation for legacy container definitions.
    pub network_mode: NetworkMode,
    /// Stable lifecycle identity for crash-recoverable create operations. When
    /// present, a backend must adopt the matching existing workload instead of
    /// launching a second one after a retry.
    pub operation_id: Option<String>,
}

/// Resource ceilings shared by every container backend.
#[derive(Debug, Clone, Copy)]
pub struct ContainerResourceLimits {
    pub memory_limit: i32,
    pub cpu_count: i32,
    pub storage_limit: i32,
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
        resources: ContainerResourceLimits,
        expose_port: i32,
        team_id: i32,
        allow_egress: bool,
        flag: String,
    ) -> Self {
        Self {
            game_kind: GameKind::AttackDefense,
            image,
            memory_limit: resources.memory_limit,
            cpu_count: resources.cpu_count,
            storage_limit: resources.storage_limit,
            expose_port,
            publish_port: true,
            proxy_only: false,
            env: vec![(TEAM_ENV.into(), team_id.to_string())],
            flag: Some(flag),
            ad_network: Some(crate::services::ad_vpn::services_network()),
            allow_egress,
            control_plane_callback_ports: Vec::new(),
            network_mode: NetworkMode::Open,
            operation_id: None,
        }
    }
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
    /// Private host-side address used only by the authenticated platform proxy.
    proxy_bind: Option<std::net::Ipv4Addr>,
    /// Hashed installation identity shared by replicas using the same Docker
    /// daemon. It prevents one deployment's orphan sweep or operation adoption
    /// from touching another deployment's workloads.
    scope: String,
    /// Cached daemon capability used by both the create path and admin warning.
    storage_quota_enforced: std::sync::Arc<std::sync::OnceLock<bool>>,
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
            proxy_bind: docker::configured_proxy_bind()?,
            scope: docker_installation_scope(),
            storage_quota_enforced: Default::default(),
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

    async fn detect_storage_quota_enforcement(&self) -> AppResult<bool> {
        if let Some(enforced) = self.storage_quota_enforced.get() {
            return Ok(*enforced);
        }
        let daemon_info = self.client()?.info().await.map_err(|error| {
            AppError::unavailable(format!(
                "could not inspect Docker writable-layer quota support: {error}"
            ))
        })?;
        let enforced = writable_layer_quota_supported(&daemon_info);
        if self.storage_quota_enforced.set(enforced).is_ok() && !enforced {
            let backing_filesystem = daemon_info
                .driver_status
                .as_ref()
                .and_then(|rows| {
                    rows.iter().find(|row| {
                        row.first()
                            .is_some_and(|key| key.eq_ignore_ascii_case("Backing Filesystem"))
                    })
                })
                .and_then(|row| row.get(1))
                .map(String::as_str)
                .unwrap_or("unknown");
            tracing::warn!(
                driver = daemon_info.driver.as_deref().unwrap_or("unknown"),
                backing_filesystem,
                "Docker cannot enforce writable-layer quotas; configured storage limits will be retained but instances will use unbounded writable layers"
            );
        }
        Ok(*self.storage_quota_enforced.get().unwrap_or(&enforced))
    }

    async fn ensure_bridge_network(
        &self,
        name: &str,
        subnet: Option<&str>,
        internal: bool,
        disable_icc: bool,
    ) -> AppResult<()> {
        let docker = self.client()?;
        if let Ok(existing) = docker
            .inspect_network(
                name,
                None::<bollard::network::InspectNetworkOptions<String>>,
            )
            .await
        {
            if bridge_network_matches(&existing, subnet, internal, disable_icc)
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
        let options = if disable_icc {
            HashMap::from([(
                "com.docker.network.bridge.enable_icc".to_string(),
                "false".to_string(),
            )])
        } else {
            HashMap::new()
        };
        let opts = CreateNetworkOptions {
            name: name.to_string(),
            check_duplicate: true,
            driver: "bridge".to_string(),
            internal,
            ipam,
            labels: scoped_managed_labels(&self.scope),
            options,
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
                        if bridge_network_matches(&existing, subnet, internal, disable_icc)
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

#[async_trait]
impl ContainerManager for DockerContainerManager {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::Docker
    }

    async fn storage_quota_enforced(&self) -> Option<bool> {
        self.detect_storage_quota_enforcement().await.ok()
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

    async fn list_managed_page(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> crate::services::container::ManagedContainerPage {
        let Ok(docker) = self.client() else {
            return Default::default();
        };
        let mut filters = managed_container_filters(&self.scope);
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            filters.insert("before".to_string(), vec![cursor.to_string()]);
        }
        let limit = limit.clamp(1, 512);
        let opts = ListContainersOptions {
            all: true,
            limit: Some(isize::try_from(limit).unwrap_or(512)),
            filters,
            ..Default::default()
        };
        match docker.list_containers(Some(opts)).await {
            Ok(list) => {
                let ids = list
                    .into_iter()
                    .filter_map(|container| container.id)
                    .collect::<Vec<_>>();
                let next_cursor = (ids.len() == limit).then(|| ids.last().cloned()).flatten();
                crate::services::container::ManagedContainerPage { ids, next_cursor }
            }
            Err(error) => {
                tracing::warn!(%error, "bounded managed-container inventory failed");
                Default::default()
            }
        }
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        validate_docker_container_spec(&spec)?;
        let docker = self.client()?;
        let storage_quota_enforced = self.detect_storage_quota_enforcement().await?;
        let storage_opt = writable_layer_storage_option(storage_quota_enforced, spec.storage_limit);
        let launch_fingerprint = launch_spec_fingerprint(&spec);

        // 1. Pull an absent repository digest without changing identity. A
        // vanished daemon-local ID must have been repaired from its trusted
        // archive before this boundary; surface a retryable infrastructure
        // response if a prune races the final create instead of leaking a 500.
        let inspected_image = match docker.inspect_image(&spec.image).await {
            Ok(image) => image,
            Err(_) if crate::services::challenge_images::is_repository_digest(&spec.image) => {
                let options = CreateImageOptions {
                    from_image: spec.image.clone(),
                    ..Default::default()
                };
                let mut pull = docker.create_image(Some(options), None, None);
                while let Some(item) = pull.next().await {
                    if let Err(error) = item {
                        tracing::warn!(image = %spec.image, %error, "immutable image pull failed");
                        break;
                    }
                }
                docker.inspect_image(&spec.image).await.map_err(|error| {
                    tracing::error!(image = %spec.image, %error, "immutable repository image remains unavailable after pull");
                    AppError::unavailable(
                        "The challenge image could not be pulled by the container host. Retry later or ask an administrator to rebuild it.",
                    )
                })?
            }
            Err(error) => {
                tracing::error!(image = %spec.image, %error, "immutable challenge image is unavailable at container create");
                return Err(AppError::unavailable(
                    "The challenge image is unavailable on this container host. Retry later or ask an administrator to rebuild it.",
                ));
            }
        };
        let restricted_profile = image_requests_restricted_profile(&inspected_image);

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
        let exposed_ports = spec
            .publish_port
            .then(|| HashMap::from([(port_key.clone(), HashMap::new())]));
        let port_bindings: Option<HashMap<String, Option<Vec<PortBinding>>>> =
            docker::published_bind_ip(&spec, self.proxy_bind)?.map(|host_ip| {
                HashMap::from([(
                    port_key.clone(),
                    Some(vec![PortBinding {
                        host_ip: Some(host_ip),
                        host_port: Some("0".to_string()),
                    }]),
                )])
            });

        // 4. Resource limits: memory (MB → bytes), CPU quota (whole cores →
        // nano-cpus), and a pids cap to blunt fork bombs.
        let host_config = HostConfig {
            memory: Some(i64::from(spec.memory_limit) * 1024 * 1024),
            nano_cpus: Some(i64::from(spec.cpu_count) * 1_000_000_000),
            pids_limit: Some(512),
            storage_opt,
            cap_drop: restricted_profile.then(|| vec!["ALL".to_string()]),
            readonly_rootfs: restricted_profile.then_some(true),
            security_opt: restricted_profile.then(|| vec!["no-new-privileges:true".to_string()]),
            tmpfs: restricted_profile.then(restricted_tmpfs_mounts),
            log_config: Some(bounded_log_config()),
            port_bindings,
            network_mode: docker_network_mode(&spec),
            ..Default::default()
        };

        let mut labels = scoped_managed_labels(&self.scope);
        labels.insert(LAUNCH_SPEC_LABEL.to_string(), launch_fingerprint.clone());
        stamp_restricted_profile(&mut labels, restricted_profile);
        stamp_storage_quota_policy(&mut labels, storage_quota_enforced);
        if let Some(operation_id) = spec.operation_id.as_ref() {
            labels.insert(OPERATION_LABEL.to_string(), operation_id.clone());
        }

        // A&D-over-VPN always attaches only to the internal services bridge.
        // Docker allowEgress is rejected above: adding a shared external bridge
        // would permit cross-workload, private-network, and metadata access.
        let networking_config = if let Some(net) = spec.ad_network.as_ref() {
            let services_cidr = crate::services::ad_vpn::services_cidr();
            self.ensure_bridge_network(net, Some(services_cidr.as_str()), true, false)
                .await?;
            Some(NetworkingConfig {
                endpoints_config: HashMap::from([(net.clone(), EndpointSettings::default())]),
            })
        } else if spec.network_mode == NetworkMode::Isolated && spec.publish_port {
            let network = format!("rsctf-isolated-{}", &self.scope[..12]);
            self.ensure_bridge_network(&network, None, true, true)
                .await?;
            Some(NetworkingConfig {
                endpoints_config: HashMap::from([(network, EndpointSettings::default())]),
            })
        } else {
            None
        };

        let config = Config {
            image: Some(spec.image.clone()),
            env: Some(env),
            exposed_ports,
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
                    || !launch_spec_matches(&existing, &launch_fingerprint)
                    || !restricted_profile_matches(&existing, restricted_profile)
                    || !storage_quota_policy_matches(&existing, storage_quota_enforced)
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
        // 6. Start, reconciling an adopter that won the concurrent start race.
        self.start_or_reconcile_container(docker, &id, spec.operation_id.is_some(), adopted)
            .await?;

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

        let published_port = binding
            .and_then(|b| b.host_port.as_deref())
            .and_then(|p| p.parse::<i32>().ok());
        if spec.proxy_only && published_port.is_none() {
            return Err(AppError::unavailable(
                "Docker did not allocate the private PlatformProxy port",
            ));
        }
        let port = published_port.unwrap_or(spec.expose_port);

        // Routable IP: prefer the configured public entry, then the binding's
        // host IP (unless it's the wildcard), then loopback. We deliberately do
        // NOT surface the container's *internal* network IP as the primary
        // endpoint — with published ports the reachable address is host-side.
        let ip = docker::advertised_endpoint_ip(
            &spec,
            self.public_entry.as_deref(),
            binding.and_then(|binding| binding.host_ip.as_deref()),
            self.proxy_bind,
        )?;

        Ok(ContainerInfo {
            id,
            ip,
            port,
            status: status.to_string(),
        })
    }

    async fn destroy(&self, id: &str) -> AppResult<()> {
        let docker = self.client()?;
        let Some(info) = self.inspect_scoped_container(docker, id).await? else {
            return Ok(());
        };
        let canonical_id = info
            .id
            .as_deref()
            .ok_or_else(|| AppError::internal("inspected container has no backend identity"))?;
        match docker
            .remove_container(
                canonical_id,
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
        self.ensure_bridge_network(name, Some(subnet), true, false)
            .await
    }

    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        let docker = self.client()?;
        let info = self
            .inspect_scoped_container(docker, id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("container not found: {id}")))?;

        let status = map_status(info.state.as_ref().and_then(|s| s.status));

        // Resource sample from the Docker stats API. Degrades to `None` on any
        // failure (daemon gone, container stopped, malformed frame) so a stats
        // hiccup never turns a successful lifecycle query into an error.
        let canonical_id = info
            .id
            .as_deref()
            .ok_or_else(|| AppError::internal("inspected container has no backend identity"))?;
        let (memory_bytes, cpu_usage) = self.sample_stats(canonical_id).await;

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
        match self.inspect_scoped_container(docker, id).await? {
            Some(info) => Ok(docker::docker_liveness(
                info.state.as_ref().and_then(|state| state.status),
            )),
            None => Ok(ContainerLiveness::Stopped),
        }
    }

    /// RSCTF A&D snapshot diff: the container's filesystem changes vs its image,
    /// from the Docker `changes` API (`docker diff`).
    async fn snapshot_changes(&self, id: &str) -> AppResult<Vec<FileChange>> {
        let docker = self.client()?;
        let info = self
            .inspect_scoped_container(docker, id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("container not found: {id}")))?;
        let canonical_id = info
            .id
            .as_deref()
            .ok_or_else(|| AppError::internal("inspected container has no backend identity"))?;
        let changes = docker.container_changes(canonical_id).await.map_err(|e| {
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

    async fn read_file(&self, id: &str, path: &str, limit: usize) -> AppResult<ContainerFile> {
        self.read_bounded_file(id, path, limit).await
    }

    /// Exec a command in the container (KotH token plant/read-back), returning
    /// the combined output.
    async fn exec(&self, id: &str, cmd: Vec<String>) -> AppResult<String> {
        self.exec_with_attribution(id, cmd, ContainerExecAdmission::default())
            .await
            .map_err(ContainerExecError::into_app_error)
    }

    async fn exec_classified(
        &self,
        id: &str,
        cmd: Vec<String>,
        admission: ContainerExecAdmission,
    ) -> Result<String, ContainerExecError> {
        self.exec_with_attribution(id, cmd, admission).await
    }

    async fn resolve_interactive_exec_target(&self, id: &str) -> AppResult<String> {
        let docker = self.client()?;
        let info = self
            .inspect_scoped_container(docker, id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("container not found: {id}")))?;
        info.id
            .ok_or_else(|| AppError::internal("inspected container has no backend identity"))
    }

    /// Export the container's filesystem via the Docker `export` endpoint
    /// (`docker export`), folding the streamed TAR into a byte buffer. Used to
    /// serve the A&D post-game snapshot; the archive is uncompressed TAR.
    async fn export(&self, id: &str) -> AppResult<Vec<u8>> {
        let docker = self.client()?;
        let info = self
            .inspect_scoped_container(docker, id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("container not found: {id}")))?;
        let canonical_id = info
            .id
            .as_deref()
            .ok_or_else(|| AppError::internal("inspected container has no backend identity"))?;
        let _permit = tokio::time::timeout(
            SNAPSHOT_EXPORT_ADMISSION_TIMEOUT,
            snapshot_export_slots().acquire(),
        )
        .await
        .map_err(|_| AppError::unavailable("snapshot export capacity is busy; retry shortly"))?
        .map_err(|_| AppError::unavailable("snapshot export service is shutting down"))?;

        tokio::time::timeout(SNAPSHOT_EXPORT_MAX_DURATION, async {
            let mut stream = docker.export_container(canonical_id);
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
