use std::collections::HashMap;
use std::io::{Cursor, Read};

use bollard::errors::Error as BollardError;
use bollard::models::{
    HostConfig, HostConfigIsolationEnum, HostConfigLogConfig, Network, SystemInfo,
    SystemInfoDefaultAddressPools,
};
use bollard::volume::CreateVolumeOptions;
use bollard::{Docker, API_DEFAULT_VERSION};
use bytes::Bytes;
use rsctf_worker_protocol::{
    CommandErrorCode, ImageIdentity, OperatingSystem, Platform, ReplicaStatus, RuntimeEndpointKind,
    WorkerCapacity, WorkloadFence, MAX_WORKER_SLOTS, MAX_WORKLOAD_REPLICAS,
};
use uuid::Uuid;

use crate::runtime::RuntimeError;

use super::{
    LABEL_ASSIGNMENT, LABEL_GENERATION, LABEL_MANAGED, LABEL_REPLICA, LABEL_SERVICE,
    LABEL_SPEC_HASH, LABEL_WORKER, LABEL_WORKLOAD,
};

pub(super) const MAX_FLAG_ARCHIVE_BYTES: usize = 1024 * 1024;
const MAX_FLAG_ARCHIVE_ENTRIES: usize = 64;
const DAEMON_OWNER_VOLUME: &str = "rsctf-worker-owner";
const LABEL_DAEMON_OWNER: &str = "io.rsctf.worker.daemon-owner";

/// Atomically reserve one Docker daemon for one durable worker identity.
///
/// Docker serializes creation of a fixed volume name. Inspecting the durable
/// label after create-or-return closes the empty-daemon race between two fresh
/// agents without consuming an address-pool subnet.
pub(super) async fn claim_docker_daemon(
    docker: &Docker,
    worker_id: Uuid,
) -> Result<(), RuntimeError> {
    docker
        .create_volume(CreateVolumeOptions {
            name: DAEMON_OWNER_VOLUME.to_string(),
            labels: HashMap::from([(LABEL_DAEMON_OWNER.to_string(), worker_id.to_string())]),
            ..Default::default()
        })
        .await
        .map_err(|error| docker_error("claim Docker daemon ownership", error))?;
    let sentinel = docker
        .inspect_volume(DAEMON_OWNER_VOLUME)
        .await
        .map_err(|error| docker_error("verify Docker daemon ownership", error))?;
    let expected = worker_id.to_string();
    if sentinel.labels.get(LABEL_DAEMON_OWNER).map(String::as_str) != Some(expected.as_str()) {
        return Err(RuntimeError::new(
            CommandErrorCode::RuntimeUnavailable,
            "this Docker daemon is already reserved by another RSCTF worker identity",
        ));
    }
    Ok(())
}

pub(super) fn connect_docker(
    endpoint: &str,
) -> Result<(Docker, RuntimeEndpointKind), RuntimeError> {
    if endpoint == "local" {
        let docker = Docker::connect_with_local_defaults()
            .map_err(|error| docker_error("connect to local Docker", error))?;
        let kind = if cfg!(windows) {
            RuntimeEndpointKind::WindowsNamedPipe
        } else {
            RuntimeEndpointKind::UnixSocket
        };
        return Ok((docker, kind));
    }
    #[cfg(unix)]
    {
        let docker = Docker::connect_with_socket(endpoint, 120, API_DEFAULT_VERSION)
            .map_err(|error| docker_error("connect to Docker Unix socket", error))?;
        Ok((docker, RuntimeEndpointKind::UnixSocket))
    }
    #[cfg(windows)]
    {
        let docker = Docker::connect_with_named_pipe(endpoint, 120, API_DEFAULT_VERSION)
            .map_err(|error| docker_error("connect to Docker named pipe", error))?;
        Ok((docker, RuntimeEndpointKind::WindowsNamedPipe))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = endpoint;
        Err(RuntimeError::unsupported(
            "Docker local transport is unsupported on this operating system",
        ))
    }
}

pub(super) fn daemon_platform(info: &SystemInfo) -> Result<Platform, RuntimeError> {
    let operating_system = match info.os_type.as_deref() {
        Some("linux") => OperatingSystem::Linux,
        Some("windows") => OperatingSystem::Windows,
        _ => {
            return Err(RuntimeError::unsupported(
                "Docker daemon did not report a supported linux/windows OS type",
            ))
        }
    };
    let architecture = match info.architecture.as_deref().map(str::trim) {
        Some("x86_64") => "amd64",
        Some("aarch64") => "arm64",
        Some(value) if !value.is_empty() => value,
        _ => {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "Docker daemon did not report its architecture",
            ))
        }
    }
    .to_string();
    let windows_build = (operating_system == OperatingSystem::Windows)
        .then(|| info.os_version.clone())
        .flatten()
        .filter(|value| !value.trim().is_empty());
    Ok(Platform {
        operating_system,
        architecture,
        windows_build,
    })
}

pub(super) fn daemon_capacity(info: &SystemInfo, network_slots: u32) -> WorkerCapacity {
    let cpus = info.ncpu.unwrap_or(1).max(1) as u64;
    let memory = info.mem_total.unwrap_or(512 * 1024 * 1024).max(1) as u64;
    WorkerCapacity {
        // Auto-detection leaves host headroom. Dedicated workers can lower it.
        cpu_millis: cpus.saturating_mul(900),
        memory_bytes: memory.saturating_mul(9) / 10,
        slots: network_slots,
    }
}

pub(super) fn workload_replica_capacity(max_network_endpoints: usize) -> u16 {
    u16::try_from(max_network_endpoints.min(MAX_WORKLOAD_REPLICAS))
        .expect("protocol workload replica limit fits in u16")
}

pub(super) fn storage_quota_supported(info: &SystemInfo) -> bool {
    match info.driver.as_deref() {
        Some("btrfs" | "zfs") => true,
        Some("overlay2") => info
            .driver_status
            .as_ref()
            .into_iter()
            .flatten()
            .any(|entry| {
                entry.first().map(String::as_str) == Some("Backing Filesystem")
                    && entry
                        .get(1)
                        .is_some_and(|value| value.eq_ignore_ascii_case("xfs"))
            }),
        _ => false,
    }
}

pub(super) fn writable_layer_storage_opt(bytes: u64) -> HashMap<String, String> {
    const MIB: u64 = 1024 * 1024;
    HashMap::from([("size".to_string(), format!("{}M", bytes.div_ceil(MIB)))])
}

pub(super) async fn docker_network_capacity(
    docker: &Docker,
    info: &SystemInfo,
    worker_id: Uuid,
) -> Result<(u32, usize), RuntimeError> {
    const DEFAULT_NETWORK_BUDGET: u64 = 24;
    const DEFAULT_NETWORK_ENDPOINTS: usize = 512;

    let configured = info.default_address_pools.as_deref().unwrap_or_default();
    let (total_networks, max_endpoints) = if configured.is_empty() {
        (DEFAULT_NETWORK_BUDGET, DEFAULT_NETWORK_ENDPOINTS)
    } else {
        let mut total = 0_u64;
        let mut endpoints = usize::MAX;
        for pool in configured {
            let Some((networks, pool_endpoints)) = address_pool_geometry(pool) else {
                return Err(RuntimeError::new(
                    CommandErrorCode::RuntimeUnavailable,
                    "Docker reported an invalid IPv4 default address pool",
                ));
            };
            total = total.saturating_add(networks);
            endpoints = endpoints.min(pool_endpoints);
        }
        (total, endpoints)
    };

    let (platform_driver, default_network) = match daemon_platform(info)?.operating_system {
        OperatingSystem::Linux => ("bridge", "bridge"),
        OperatingSystem::Windows => ("nat", "nat"),
    };
    let networks = docker
        .list_networks::<String>(None)
        .await
        .map_err(|error| docker_error("inventory Docker network capacity", error))?;
    let mut leaked_managed_networks = 0_u64;
    for network in &networks {
        let labels = network.labels.as_ref();
        if label(labels, LABEL_MANAGED) != Some("true") {
            continue;
        }
        let expected = worker_id.to_string();
        if label(labels, LABEL_WORKER) != Some(expected.as_str()) {
            return Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "this Docker daemon contains workloads owned by another RSCTF worker identity",
            ));
        }
        if is_custom_platform_network(network, platform_driver, default_network)
            && network.containers.as_ref().is_some_and(HashMap::is_empty)
        {
            // Active managed workloads are accounted by the server's durable
            // workload reservations. An explicitly empty managed network has
            // no corresponding container usage and must consume one extra
            // address-pool slot until reconciliation removes the leak.
            leaked_managed_networks = leaked_managed_networks.saturating_add(1);
        }
    }
    let external_platform_networks = networks
        .iter()
        .filter(|network| is_custom_platform_network(network, platform_driver, default_network))
        .filter(|network| {
            network
                .labels
                .as_ref()
                .and_then(|labels| labels.get(LABEL_MANAGED))
                .map(String::as_str)
                != Some("true")
        })
        .count() as u64;
    let available = total_networks
        .saturating_sub(external_platform_networks.saturating_add(leaked_managed_networks))
        .min(u64::from(MAX_WORKER_SLOTS));
    if available == 0 || max_endpoints == 0 {
        return Err(RuntimeError::new(
            CommandErrorCode::RuntimeUnavailable,
            "Docker has no bounded IPv4 address-pool capacity for isolated workloads",
        ));
    }
    Ok((available as u32, max_endpoints))
}

fn is_custom_platform_network(network: &Network, driver: &str, default_name: &str) -> bool {
    network.driver.as_deref() == Some(driver) && network.name.as_deref() != Some(default_name)
}

fn address_pool_geometry(pool: &SystemInfoDefaultAddressPools) -> Option<(u64, usize)> {
    let (address, base_prefix) = pool.base.as_deref()?.split_once('/')?;
    if !matches!(
        address.parse::<std::net::IpAddr>().ok()?,
        std::net::IpAddr::V4(_)
    ) {
        return None;
    }
    let base_prefix = base_prefix.parse::<u32>().ok()?;
    let subnet_prefix = u32::try_from(pool.size?).ok()?;
    if base_prefix > subnet_prefix || subnet_prefix > 30 {
        return None;
    }
    let networks = 1_u64.checked_shl(subnet_prefix - base_prefix)?;
    let addresses = 1_u64.checked_shl(32 - subnet_prefix)?;
    let endpoints = usize::try_from(addresses.saturating_sub(3)).ok()?;
    Some((networks, endpoints))
}

pub(super) fn base_labels(
    worker_id: Uuid,
    fence: WorkloadFence,
    spec_hash: &str,
) -> HashMap<String, String> {
    HashMap::from([
        (LABEL_MANAGED.to_string(), "true".to_string()),
        (LABEL_WORKER.to_string(), worker_id.to_string()),
        (LABEL_WORKLOAD.to_string(), fence.workload_id.to_string()),
        (
            LABEL_ASSIGNMENT.to_string(),
            fence.assignment_id.to_string(),
        ),
        (LABEL_GENERATION.to_string(), fence.generation.to_string()),
        (LABEL_SPEC_HASH.to_string(), spec_hash.to_string()),
    ])
}

pub(super) fn labels_match(
    labels: Option<&HashMap<String, String>>,
    fence: WorkloadFence,
    spec_hash: &str,
) -> bool {
    label(labels, LABEL_ASSIGNMENT) == Some(fence.assignment_id.to_string()).as_deref()
        && label(labels, LABEL_GENERATION) == Some(fence.generation.to_string()).as_deref()
        && label(labels, LABEL_SPEC_HASH) == Some(spec_hash)
}

pub(super) fn validate_workload_network(
    network: &Network,
    worker_id: Uuid,
    fence: WorkloadFence,
    spec_hash: &str,
    operating_system: OperatingSystem,
) -> Result<(), RuntimeError> {
    let labels = network.labels.as_ref();
    let worker = worker_id.to_string();
    let workload = fence.workload_id.to_string();
    let assignment = fence.assignment_id.to_string();
    let generation = fence.generation.to_string();
    let expected_driver = if operating_system == OperatingSystem::Windows {
        "nat"
    } else {
        "bridge"
    };
    let valid = label(labels, LABEL_MANAGED) == Some("true")
        && label(labels, LABEL_WORKER) == Some(worker.as_str())
        && label(labels, LABEL_WORKLOAD) == Some(workload.as_str())
        && label(labels, LABEL_ASSIGNMENT) == Some(assignment.as_str())
        && label(labels, LABEL_GENERATION) == Some(generation.as_str())
        && label(labels, LABEL_SPEC_HASH) == Some(spec_hash)
        && network.driver.as_deref() == Some(expected_driver)
        && network.internal == Some(true)
        && network.attachable != Some(true)
        && network.ingress != Some(true)
        && network.enable_ipv6 != Some(true)
        && valid_ipv4_ipam(network);
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::new(
            CommandErrorCode::SpecConflict,
            "workload network does not match its fenced isolation definition",
        ))
    }
}

fn valid_ipv4_ipam(network: &Network) -> bool {
    let Some(ipam) = network.ipam.as_ref() else {
        return false;
    };
    if ipam.driver.as_deref() != Some("default") {
        return false;
    }
    let Some(configs) = ipam.config.as_deref() else {
        return false;
    };
    if configs.len() != 1 {
        return false;
    }
    let config = &configs[0];
    let Some((network_address, prefix)) = config.subnet.as_deref().and_then(parse_ipv4_cidr) else {
        return false;
    };
    let Some(gateway) = config
        .gateway
        .as_deref()
        .and_then(|value| value.parse::<std::net::Ipv4Addr>().ok())
        .map(u32::from)
    else {
        return false;
    };
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    let broadcast = network_address | !mask;
    if gateway <= network_address || gateway >= broadcast || gateway & mask != network_address {
        return false;
    }
    config.ip_range.as_deref().is_none_or(|range| {
        parse_ipv4_cidr(range).is_some_and(|(range_address, range_prefix)| {
            range_prefix >= prefix && range_address & mask == network_address
        })
    })
}

fn parse_ipv4_cidr(value: &str) -> Option<(u32, u32)> {
    let (address, prefix) = value.split_once('/')?;
    let address = u32::from(address.parse::<std::net::Ipv4Addr>().ok()?);
    let prefix = prefix.parse::<u32>().ok()?;
    if !(1..=30).contains(&prefix) {
        return None;
    }
    let mask = u32::MAX.checked_shl(32 - prefix)?;
    (address & mask == address).then_some((address, prefix))
}

pub(super) fn label<'a>(labels: Option<&'a HashMap<String, String>>, key: &str) -> Option<&'a str> {
    labels
        .and_then(|labels| labels.get(key))
        .map(String::as_str)
}

pub(super) fn network_name(fence: WorkloadFence) -> String {
    format!(
        "rsctf-{}-{}",
        &fence.workload_id.simple().to_string()[..12],
        &fence.assignment_id.simple().to_string()[..8]
    )
}

pub(super) fn container_name(fence: WorkloadFence, service: &str, replica: u16) -> String {
    format!(
        "rsctf-{}-{service}-{replica}",
        &fence.workload_id.simple().to_string()[..12]
    )
}

pub(super) fn image_string(image: &ImageIdentity) -> String {
    match image {
        ImageIdentity::RegistryDigest { repository, digest } => format!("{repository}@{digest}"),
        ImageIdentity::WorkerLocal { image_id, .. } => image_id.clone(),
    }
}

pub(super) fn container_image_matches(
    container: &bollard::models::ContainerSummary,
    image: &ImageIdentity,
    image_name: &str,
) -> bool {
    match image {
        ImageIdentity::RegistryDigest { .. } => container.image.as_deref() == Some(image_name),
        ImageIdentity::WorkerLocal { image_id, .. } => {
            container.image_id.as_deref() == Some(image_id.as_str())
        }
    }
}

pub(super) fn docker_port(port: u16) -> String {
    format!("{port}/tcp")
}

pub(super) fn bounded_log_config() -> HostConfigLogConfig {
    HostConfigLogConfig {
        typ: Some("json-file".to_string()),
        config: Some(HashMap::from([
            ("max-size".to_string(), "5m".to_string()),
            ("max-file".to_string(), "3".to_string()),
        ])),
    }
}

pub(super) fn workload_host_config(
    operating_system: OperatingSystem,
    network: &str,
    cpu_millis: u32,
    memory_bytes: u64,
    writable_layer_bytes: Option<u64>,
) -> HostConfig {
    let memory_limit = memory_bytes.min(i64::MAX as u64) as i64;
    HostConfig {
        memory: Some(memory_limit),
        // Docker otherwise permits additional swap beyond the reservation,
        // invalidating scheduler accounting under memory pressure.
        memory_swap: Some(memory_limit),
        nano_cpus: Some(i64::from(cpu_millis) * 1_000_000),
        pids_limit: (operating_system == OperatingSystem::Linux).then_some(512),
        log_config: Some(bounded_log_config()),
        network_mode: Some(network.to_string()),
        // A challenge image must not inherit Docker's default capability set.
        // Match the local Kubernetes backend by restoring only the narrow
        // capability commonly required by non-root images that listen on 80.
        cap_drop: (operating_system == OperatingSystem::Linux).then(|| vec!["ALL".to_string()]),
        cap_add: (operating_system == OperatingSystem::Linux)
            .then(|| vec!["NET_BIND_SERVICE".to_string()]),
        security_opt: (operating_system == OperatingSystem::Linux)
            .then(|| vec!["no-new-privileges:true".to_string()]),
        isolation: (operating_system == OperatingSystem::Windows)
            .then_some(HostConfigIsolationEnum::HYPERV),
        storage_opt: writable_layer_bytes.map(writable_layer_storage_opt),
        ..Default::default()
    }
}

pub(super) fn replica_status(container: &bollard::models::ContainerSummary) -> ReplicaStatus {
    let labels = container.labels.as_ref();
    ReplicaStatus {
        service: label(labels, LABEL_SERVICE)
            .unwrap_or("unknown")
            .to_string(),
        replica: label(labels, LABEL_REPLICA)
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        ready: container.state.as_deref() == Some("running"),
        runtime_id: container.id.clone(),
        detail: (container.state.as_deref() != Some("running")).then(|| {
            bounded_text(
                container
                    .status
                    .clone()
                    .unwrap_or_else(|| "not running".to_string()),
                256,
            )
        }),
    }
}

fn bounded_text(mut value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

pub(super) fn split_guest_path(
    path: &str,
    operating_system: OperatingSystem,
) -> Result<(String, String), RuntimeError> {
    let separators = if operating_system == OperatingSystem::Windows {
        ['/', '\\']
    } else {
        ['/', '/']
    };
    let Some(index) = path.rfind(separators) else {
        return Err(RuntimeError::new(
            CommandErrorCode::InvalidSpec,
            "flag path must include a parent directory",
        ));
    };
    let (parent, tail) = path.split_at(index);
    let filename = tail.trim_start_matches(['/', '\\']);
    if filename.is_empty() || filename == "." || filename == ".." {
        return Err(RuntimeError::new(
            CommandErrorCode::InvalidSpec,
            "flag filename is invalid",
        ));
    }
    let parent = if parent.is_empty() {
        "/".to_string()
    } else if operating_system == OperatingSystem::Windows && parent.ends_with(':') {
        format!("{parent}\\")
    } else {
        parent.to_string()
    };
    Ok((parent, filename.to_string()))
}

pub(super) async fn make_single_file_archive(
    filename: String,
    contents: Vec<u8>,
) -> Result<Bytes, RuntimeError> {
    tokio::task::spawn_blocking(move || {
        let mut archive = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut archive);
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, filename, Cursor::new(contents))
                .map_err(|error| {
                    RuntimeError::new(
                        CommandErrorCode::Internal,
                        format!("build flag archive: {error}"),
                    )
                })?;
            builder.finish().map_err(|error| {
                RuntimeError::new(
                    CommandErrorCode::Internal,
                    format!("finish flag archive: {error}"),
                )
            })?;
        }
        Ok(Bytes::from(archive))
    })
    .await
    .map_err(|error| {
        RuntimeError::new(
            CommandErrorCode::Internal,
            format!("flag archive task failed: {error}"),
        )
    })?
}

pub(super) async fn archive_contains_contents(
    bytes: Vec<u8>,
    expected: Vec<u8>,
) -> Result<bool, RuntimeError> {
    if bytes.len() > MAX_FLAG_ARCHIVE_BYTES {
        return Err(RuntimeError::new(
            CommandErrorCode::InvalidSpec,
            "flag verification archive exceeds the 1 MiB safety limit",
        ));
    }
    tokio::task::spawn_blocking(move || {
        let mut archive = tar::Archive::new(Cursor::new(bytes));
        let entries = archive.entries().map_err(|error| {
            RuntimeError::new(
                CommandErrorCode::Internal,
                format!("read flag verification archive: {error}"),
            )
        })?;
        for (index, entry) in entries.enumerate() {
            if index >= MAX_FLAG_ARCHIVE_ENTRIES {
                return Err(RuntimeError::new(
                    CommandErrorCode::InvalidSpec,
                    "flag verification archive contains too many entries",
                ));
            }
            let entry = entry.map_err(|error| {
                RuntimeError::new(
                    CommandErrorCode::Internal,
                    format!("read flag verification entry: {error}"),
                )
            })?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            if entry.size() != expected.len() as u64 {
                continue;
            }
            let mut contents = Vec::with_capacity(expected.len());
            entry
                .take(expected.len() as u64 + 1)
                .read_to_end(&mut contents)
                .map_err(|error| {
                    RuntimeError::new(
                        CommandErrorCode::Internal,
                        format!("read verified flag contents: {error}"),
                    )
                })?;
            if contents == expected {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await
    .map_err(|error| {
        RuntimeError::new(
            CommandErrorCode::Internal,
            format!("flag verification task failed: {error}"),
        )
    })?
}

pub(super) fn is_not_found(error: &BollardError) -> bool {
    matches!(
        error,
        BollardError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

pub(super) fn docker_error(operation: &str, error: BollardError) -> RuntimeError {
    let code = if is_not_found(&error) {
        CommandErrorCode::NotFound
    } else {
        CommandErrorCode::RuntimeUnavailable
    };
    RuntimeError::new(code, format!("{operation}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{Ipam, IpamConfig, Network};

    #[test]
    fn deterministic_names_are_bounded() {
        let fence = WorkloadFence {
            workload_id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
            assignment_id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
            generation: 1,
        };
        assert_eq!(network_name(fence), "rsctf-aaaaaaaaaaaa-bbbbbbbb");
        assert_eq!(container_name(fence, "web", 2), "rsctf-aaaaaaaaaaaa-web-2");
    }

    #[test]
    fn workload_replica_capacity_is_bounded_by_the_protocol() {
        assert_eq!(workload_replica_capacity(37), 37);
        assert_eq!(workload_replica_capacity(usize::MAX), 512);
    }

    #[test]
    fn linux_workloads_replace_defaults_with_only_bind_service() {
        let config = workload_host_config(
            OperatingSystem::Linux,
            "rsctf-test-network",
            500,
            256 * 1024 * 1024,
            Some(64 * 1024 * 1024),
        );

        assert_eq!(config.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(config.cap_add, Some(vec!["NET_BIND_SERVICE".to_string()]));
        assert_eq!(
            config.security_opt,
            Some(vec!["no-new-privileges:true".to_string()])
        );
    }

    #[test]
    fn non_linux_config_does_not_emit_linux_capability_directives() {
        let config = workload_host_config(
            OperatingSystem::Windows,
            "rsctf-test-network",
            500,
            256 * 1024 * 1024,
            Some(64 * 1024 * 1024),
        );

        assert_eq!(config.cap_drop, None);
        assert_eq!(config.cap_add, None);
        assert_eq!(config.security_opt, None);
    }

    #[test]
    fn platform_network_accounting_excludes_linux_and_windows_defaults() {
        let network = |driver: &str, name: &str| Network {
            driver: Some(driver.to_string()),
            name: Some(name.to_string()),
            ..Default::default()
        };
        assert!(!is_custom_platform_network(
            &network("bridge", "bridge"),
            "bridge",
            "bridge"
        ));
        assert!(is_custom_platform_network(
            &network("bridge", "challenge"),
            "bridge",
            "bridge"
        ));
        assert!(!is_custom_platform_network(
            &network("nat", "nat"),
            "nat",
            "nat"
        ));
        assert!(is_custom_platform_network(
            &network("nat", "challenge"),
            "nat",
            "nat"
        ));
    }

    #[test]
    fn splits_linux_and_windows_paths_without_host_semantics() {
        assert_eq!(
            split_guest_path("/run/flag", OperatingSystem::Linux).unwrap(),
            ("/run".to_string(), "flag".to_string())
        );
        assert_eq!(
            split_guest_path("C:\\ctf\\flag.txt", OperatingSystem::Windows).unwrap(),
            ("C:\\ctf".to_string(), "flag.txt".to_string())
        );
        assert_eq!(
            split_guest_path("C:\\flag.txt", OperatingSystem::Windows).unwrap(),
            ("C:\\".to_string(), "flag.txt".to_string())
        );
    }

    #[test]
    fn calculates_configured_address_pool_capacity() {
        let pool = SystemInfoDefaultAddressPools {
            base: Some("172.30.0.0/16".to_string()),
            size: Some(24),
        };
        assert_eq!(address_pool_geometry(&pool), Some((256, 253)));

        let too_small = SystemInfoDefaultAddressPools {
            base: Some("172.30.0.0/16".to_string()),
            size: Some(31),
        };
        assert_eq!(address_pool_geometry(&too_small), None);
    }

    #[test]
    fn storage_quota_preflight_is_fail_closed() {
        let xfs = SystemInfo {
            driver: Some("overlay2".to_string()),
            driver_status: Some(vec![vec![
                "Backing Filesystem".to_string(),
                "xfs".to_string(),
            ]]),
            ..Default::default()
        };
        assert!(storage_quota_supported(&xfs));

        let ext = SystemInfo {
            driver: Some("overlay2".to_string()),
            driver_status: Some(vec![vec![
                "Backing Filesystem".to_string(),
                "extfs".to_string(),
            ]]),
            ..Default::default()
        };
        assert!(!storage_quota_supported(&ext));
        assert_eq!(
            writable_layer_storage_opt(512 * 1024 * 1024),
            HashMap::from([("size".to_string(), "512M".to_string())])
        );
    }

    #[test]
    fn workload_network_validation_rejects_boundary_drift() {
        let worker_id = Uuid::new_v4();
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 3,
        };
        let mut network = Network {
            driver: Some("bridge".into()),
            enable_ipv6: Some(false),
            internal: Some(true),
            attachable: Some(false),
            ingress: Some(false),
            ipam: Some(Ipam {
                driver: Some("default".into()),
                config: Some(vec![IpamConfig {
                    subnet: Some("172.30.1.0/24".into()),
                    gateway: Some("172.30.1.1".into()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            labels: Some(base_labels(worker_id, fence, "aabb")),
            ..Default::default()
        };
        assert!(validate_workload_network(
            &network,
            worker_id,
            fence,
            "aabb",
            OperatingSystem::Linux,
        )
        .is_ok());

        network.internal = Some(false);
        assert!(validate_workload_network(
            &network,
            worker_id,
            fence,
            "aabb",
            OperatingSystem::Linux,
        )
        .is_err());
        network.internal = Some(true);
        network
            .labels
            .as_mut()
            .unwrap()
            .insert(LABEL_WORKER.into(), Uuid::new_v4().to_string());
        assert!(validate_workload_network(
            &network,
            worker_id,
            fence,
            "aabb",
            OperatingSystem::Linux,
        )
        .is_err());
    }

    #[test]
    fn derives_platform_from_the_daemon_not_the_agent_binary() {
        let info = SystemInfo {
            os_type: Some("linux".to_string()),
            architecture: Some("x86_64".to_string()),
            ..Default::default()
        };
        assert_eq!(
            daemon_platform(&info).unwrap(),
            Platform {
                operating_system: OperatingSystem::Linux,
                architecture: "amd64".to_string(),
                windows_build: None,
            }
        );
    }

    #[tokio::test]
    async fn flag_archive_parser_rejects_oversize_and_skips_large_entries() {
        assert!(archive_contains_contents(
            vec![0_u8; MAX_FLAG_ARCHIVE_BYTES + 1],
            b"flag".to_vec(),
        )
        .await
        .is_err());

        let mut archive = Vec::new();
        {
            let contents = vec![b'x'; 900 * 1024];
            let mut builder = tar::Builder::new(&mut archive);
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, "flag", Cursor::new(contents))
                .unwrap();
            builder.finish().unwrap();
        }
        assert!(archive.len() <= MAX_FLAG_ARCHIVE_BYTES);
        assert!(!archive_contains_contents(archive, b"flag".to_vec())
            .await
            .unwrap());
    }
}
