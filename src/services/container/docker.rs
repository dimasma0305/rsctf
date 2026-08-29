use bollard::container::{
    ListContainersOptions, RemoveContainerOptions, StartContainerOptions, StatsOptions,
};
use bollard::models::{
    ContainerInspectResponse, ContainerStateStatusEnum, ImageInspect, SystemInfo,
};
use bollard::Docker;
use futures::StreamExt;
use rsctf_worker_protocol::GameKind;

use super::{
    labels_match_scope, ContainerExecAdmission, ContainerExecError, ContainerLiveness,
    ContainerManager, ContainerSpec, DockerContainerManager, NoopContainerManager, MANAGED_LABEL,
    MAX_EXEC_OUTPUT_BYTES, OPERATION_LABEL, SCOPE_LABEL,
};
use crate::utils::error::{AppError, AppResult};

pub(super) const LAUNCH_SPEC_LABEL: &str = "rsctf.launch-spec";
pub(super) const STORAGE_QUOTA_LABEL: &str = "rsctf.storage-quota";
const STORAGE_QUOTA_ENFORCED: &str = "enforced";
const STORAGE_QUOTA_FALLBACK: &str = "unbounded-fallback";
/// Organizer-controlled image opt-in for workloads that are known to work
/// with an immutable root filesystem and no Linux capabilities. Keeping this
/// explicit preserves pwn/KotH images whose intended gameplay needs setuid or
/// a narrow capability while allowing ordinary services to use a restricted
/// runtime profile.
pub(super) const RESTRICTED_IMAGE_PROFILE_LABEL: &str = "org.rsctf.security-profile";
pub(super) const RESTRICTED_IMAGE_PROFILE: &str = "restricted-v1";
pub(super) const RESTRICTED_TMPFS_PATH: &str = "/tmp";
pub(super) const RESTRICTED_TMPFS_OPTIONS: &str = "rw,nosuid,nodev,noexec,size=268435456,mode=1777";
const COMPETITIVE_EGRESS_ERROR: &str =
    "Docker does not safely support allowEgress=true for A&D or KotH workloads; \
     set allowEgress=false or use the Kubernetes backend with per-workload NetworkPolicy isolation";
const MAX_CONCURRENT_SNAPSHOT_EXPORTS: usize = 1;
pub(super) const MAX_SNAPSHOT_EXPORT_BYTES: usize = 512 * 1024 * 1024;
pub(super) const SNAPSHOT_EXPORT_MAX_DURATION: std::time::Duration =
    std::time::Duration::from_secs(120);
pub(super) const SNAPSHOT_EXPORT_ADMISSION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);

/// Export + compression temporarily holds the raw TAR and compressed archive.
/// One capture at a time keeps the control replica inside its memory budget.
pub(super) fn snapshot_export_slots() -> &'static tokio::sync::Semaphore {
    static SLOTS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_SNAPSHOT_EXPORTS))
}

/// Bound an untrusted Docker export before compression allocates another copy.
pub(super) fn append_snapshot_chunk(
    out: &mut Vec<u8>,
    chunk: &[u8],
    limit: usize,
) -> AppResult<()> {
    let next_len = out
        .len()
        .checked_add(chunk.len())
        .ok_or_else(|| AppError::bad_request("snapshot export size overflow"))?;
    if next_len > limit {
        return Err(AppError::payload_too_large(format!(
            "snapshot export exceeds the {} MiB safety limit",
            limit / (1024 * 1024)
        )));
    }
    out.try_reserve(chunk.len())
        .map_err(|_| AppError::internal("failed to reserve snapshot export buffer"))?;
    out.extend_from_slice(chunk);
    Ok(())
}

pub(super) fn validate_docker_container_spec(spec: &ContainerSpec) -> AppResult<()> {
    if spec.allow_egress
        && matches!(
            spec.game_kind,
            GameKind::AttackDefense | GameKind::KingOfTheHill
        )
    {
        return Err(AppError::bad_request(COMPETITIVE_EGRESS_ERROR));
    }
    super::validate_container_spec(spec)
}

pub(super) fn image_requests_restricted_profile(image: &ImageInspect) -> bool {
    image
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(RESTRICTED_IMAGE_PROFILE_LABEL))
        .is_some_and(|profile| profile == RESTRICTED_IMAGE_PROFILE)
}

pub(super) fn stamp_restricted_profile(
    labels: &mut std::collections::HashMap<String, String>,
    restricted: bool,
) {
    if restricted {
        labels.insert(
            RESTRICTED_IMAGE_PROFILE_LABEL.into(),
            RESTRICTED_IMAGE_PROFILE.into(),
        );
    } else {
        labels.remove(RESTRICTED_IMAGE_PROFILE_LABEL);
    }
}

pub(super) fn restricted_tmpfs_mounts() -> std::collections::HashMap<String, String> {
    std::collections::HashMap::from([(
        RESTRICTED_TMPFS_PATH.to_string(),
        RESTRICTED_TMPFS_OPTIONS.to_string(),
    )])
}

pub(super) fn restricted_profile_matches(
    container: &ContainerInspectResponse,
    expected: bool,
) -> bool {
    if !expected {
        return true;
    }
    let Some(config) = container.host_config.as_ref() else {
        return false;
    };
    config.readonly_rootfs == Some(true)
        && config
            .cap_drop
            .as_ref()
            .is_some_and(|caps| caps.iter().any(|capability| capability == "ALL"))
        && config.security_opt.as_ref().is_some_and(|options| {
            options
                .iter()
                .any(|option| option == "no-new-privileges:true")
        })
        && config.tmpfs.as_ref().is_some_and(|mounts| {
            mounts.len() == 1
                && mounts
                    .get(RESTRICTED_TMPFS_PATH)
                    .is_some_and(|options| options == RESTRICTED_TMPFS_OPTIONS)
        })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerLaunchSpec<'a> {
    revision: u8,
    game_kind: GameKind,
    image: &'a str,
    memory_limit: i32,
    cpu_count: i32,
    storage_limit: i32,
    expose_port: i32,
    #[serde(skip_serializing_if = "is_true")]
    publish_port: bool,
    #[serde(skip_serializing_if = "is_false")]
    proxy_only: bool,
    env: &'a [(String, String)],
    flag: Option<&'a str>,
    ad_network: Option<&'a str>,
    allow_egress: bool,
    control_plane_callback_ports: &'a [i32],
    network_mode: crate::utils::enums::NetworkMode,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerLaunchSpecV4<'a> {
    revision: u8,
    game_kind: GameKind,
    image: &'a str,
    memory_limit: i32,
    cpu_count: i32,
    storage_limit: i32,
    expose_port: i32,
    #[serde(skip_serializing_if = "is_true")]
    publish_port: bool,
    #[serde(skip_serializing_if = "is_false")]
    proxy_only: bool,
    env: &'a [(String, String)],
    flag: Option<&'a str>,
    ad_network: Option<&'a str>,
    allow_egress: bool,
    network_mode: crate::utils::enums::NetworkMode,
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(super) const PROXY_BIND_REQUIRED: &str =
    "PlatformProxy requires RSCTF_DOCKER_PROXY_BIND to be a private IPv4 address reachable by rsctf";

pub(super) fn parse_proxy_bind(value: &str) -> AppResult<std::net::Ipv4Addr> {
    let ip = value
        .parse::<std::net::Ipv4Addr>()
        .map_err(|_| AppError::bad_request("RSCTF_DOCKER_PROXY_BIND must be an IPv4 address"))?;
    let octets = ip.octets();
    let is_rfc1918 = octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168);
    if !is_rfc1918 {
        return Err(AppError::bad_request(
            "RSCTF_DOCKER_PROXY_BIND must be an RFC1918 IPv4 address reachable by rsctf",
        ));
    }
    Ok(ip)
}

pub(super) fn configured_proxy_bind() -> AppResult<Option<std::net::Ipv4Addr>> {
    std::env::var("RSCTF_DOCKER_PROXY_BIND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| parse_proxy_bind(value.trim()))
        .transpose()
}

pub(super) fn published_bind_ip(
    spec: &ContainerSpec,
    proxy_bind: Option<std::net::Ipv4Addr>,
) -> AppResult<Option<String>> {
    if spec.ad_network.is_some() || !spec.publish_port {
        return Ok(None);
    }
    if spec.proxy_only {
        return proxy_bind
            .map(|ip| Some(ip.to_string()))
            .ok_or_else(|| AppError::unavailable(PROXY_BIND_REQUIRED));
    }
    Ok(Some("0.0.0.0".to_string()))
}

pub(super) fn advertised_endpoint_ip(
    spec: &ContainerSpec,
    public_entry: Option<&str>,
    inspected_bind: Option<&str>,
    proxy_bind: Option<std::net::Ipv4Addr>,
) -> AppResult<String> {
    if spec.proxy_only {
        let expected = proxy_bind.ok_or_else(|| AppError::unavailable(PROXY_BIND_REQUIRED))?;
        if inspected_bind.and_then(|value| value.parse().ok()) != Some(expected) {
            return Err(AppError::unavailable(
                "Docker did not publish the PlatformProxy port on the configured private interface",
            ));
        }
        return Ok(expected.to_string());
    }
    Ok(public_entry
        .map(str::to_string)
        .or_else(|| {
            inspected_bind
                .filter(|host| !host.is_empty() && *host != "0.0.0.0")
                .map(str::to_string)
        })
        .unwrap_or_else(|| "127.0.0.1".to_string()))
}

/// A no-publish local workload is reachable only through authenticated exec.
/// Docker's default bridge would still grant outbound and east-west access, so
/// attach no network unless the caller selected an explicit internal network.
pub(super) fn docker_network_mode(spec: &ContainerSpec) -> Option<String> {
    (!spec.publish_port && spec.ad_network.is_none()).then(|| "none".to_string())
}

pub(super) fn writable_layer_quota_supported(info: &SystemInfo) -> bool {
    match info
        .driver
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("btrfs" | "devicemapper" | "windowsfilter" | "zfs") => true,
        Some("overlay2") => info.driver_status.as_ref().is_some_and(|rows| {
            rows.iter().any(|row| {
                row.first()
                    .is_some_and(|key| key.eq_ignore_ascii_case("Backing Filesystem"))
                    && row
                        .get(1)
                        .is_some_and(|value| value.eq_ignore_ascii_case("xfs"))
            })
        }),
        _ => false,
    }
}

pub(super) fn writable_layer_storage_opt(
    storage_limit: i32,
) -> std::collections::HashMap<String, String> {
    std::collections::HashMap::from([("size".to_string(), format!("{storage_limit}M"))])
}

pub(super) fn writable_layer_storage_option(
    enforced: bool,
    storage_limit: i32,
) -> Option<std::collections::HashMap<String, String>> {
    enforced.then(|| writable_layer_storage_opt(storage_limit))
}

pub(super) fn stamp_storage_quota_policy(
    labels: &mut std::collections::HashMap<String, String>,
    enforced: bool,
) {
    labels.insert(
        STORAGE_QUOTA_LABEL.to_string(),
        if enforced {
            STORAGE_QUOTA_ENFORCED
        } else {
            STORAGE_QUOTA_FALLBACK
        }
        .to_string(),
    );
}

pub(super) fn storage_quota_policy_matches(
    container: &ContainerInspectResponse,
    enforced: bool,
) -> bool {
    let expected = if enforced {
        STORAGE_QUOTA_ENFORCED
    } else {
        STORAGE_QUOTA_FALLBACK
    };
    // Before this label existed, Docker creation failed closed unless a quota
    // was enforced. Those legacy containers are safe to adopt only when the
    // current daemon also enforces the limit.
    container
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(STORAGE_QUOTA_LABEL))
        .map_or(enforced, |actual| actual == expected)
}

/// Hash every launch-affecting caller input into a non-secret identity label.
/// Operation and installation identities have their own labels and deliberately
/// do not affect whether a crash retry represents the same workload.
pub(crate) fn launch_spec_fingerprint(spec: &ContainerSpec) -> String {
    let canonical = DockerLaunchSpec {
        // v5 adds control-plane callback policy ports. Older workloads must
        // never be adopted because they do not prove the same egress policy.
        revision: 5,
        game_kind: spec.game_kind,
        image: &spec.image,
        memory_limit: spec.memory_limit,
        cpu_count: spec.cpu_count,
        storage_limit: spec.storage_limit,
        expose_port: spec.expose_port,
        publish_port: spec.publish_port,
        proxy_only: spec.proxy_only,
        env: &spec.env,
        flag: spec.flag.as_deref(),
        ad_network: spec.ad_network.as_deref(),
        allow_egress: spec.allow_egress,
        control_plane_callback_ports: &spec.control_plane_callback_ports,
        network_mode: spec.network_mode,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("the fixed container launch identity is always JSON serializable");
    crate::utils::codec::sha256_hex(&bytes)
}

pub(super) fn launch_spec_matches(
    info: &ContainerInspectResponse,
    expected_fingerprint: &str,
) -> bool {
    info.config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(LAUNCH_SPEC_LABEL))
        .map(String::as_str)
        == Some(expected_fingerprint)
}

fn legacy_v4_launch_spec_matches(info: &ContainerInspectResponse, spec: &ContainerSpec) -> bool {
    if !spec.control_plane_callback_ports.is_empty() {
        return false;
    }
    let canonical = DockerLaunchSpecV4 {
        revision: 4,
        game_kind: spec.game_kind,
        image: &spec.image,
        memory_limit: spec.memory_limit,
        cpu_count: spec.cpu_count,
        storage_limit: spec.storage_limit,
        expose_port: spec.expose_port,
        publish_port: spec.publish_port,
        proxy_only: spec.proxy_only,
        env: &spec.env,
        flag: spec.flag.as_deref(),
        ad_network: spec.ad_network.as_deref(),
        allow_egress: spec.allow_egress,
        network_mode: spec.network_mode,
    };
    let fingerprint = crate::utils::codec::sha256_hex(
        &serde_json::to_vec(&canonical).expect("the legacy launch identity serializes"),
    );
    launch_spec_matches(info, &fingerprint)
}

fn operation_container_filters(
    scope: &str,
    operation_id: &str,
) -> std::collections::HashMap<String, Vec<String>> {
    std::collections::HashMap::from([(
        "label".to_string(),
        vec![
            format!("{MANAGED_LABEL}={scope}"),
            format!("{SCOPE_LABEL}={scope}"),
            format!("{OPERATION_LABEL}={operation_id}"),
        ],
    )])
}

pub(super) fn validate_operation_container(
    existing: &ContainerInspectResponse,
    scope: &str,
    spec: &ContainerSpec,
    launch_fingerprint: &str,
    restricted_profile: bool,
    storage_quota_enforced: bool,
) -> AppResult<String> {
    let labels = existing
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref());
    let actual_operation = labels
        .and_then(|labels| labels.get(OPERATION_LABEL))
        .map(String::as_str);
    let actual_image = existing
        .config
        .as_ref()
        .and_then(|config| config.image.as_deref());
    if !labels_match_scope(labels, scope)
        || actual_operation != spec.operation_id.as_deref()
        || actual_image != Some(spec.image.as_str())
        || !(launch_spec_matches(existing, launch_fingerprint)
            || legacy_v4_launch_spec_matches(existing, spec))
        || !restricted_profile_matches(existing, restricted_profile)
        || !storage_quota_policy_matches(existing, storage_quota_enforced)
    {
        return Err(AppError::conflict(
            "container operation identity is owned by a different workload",
        ));
    }
    existing
        .id
        .clone()
        .ok_or_else(|| AppError::internal("adopted container has no backend identity"))
}

/// Find a response-lost workload by immutable operation labels rather than by
/// name. Older replicas used an image-prefixed name, so name-only conflict
/// handling can otherwise duplicate a workload during a rolling deployment.
pub(super) async fn discover_operation_container(
    docker: &Docker,
    scope: &str,
    spec: &ContainerSpec,
    launch_fingerprint: &str,
    restricted_profile: bool,
    storage_quota_enforced: bool,
) -> AppResult<Option<String>> {
    let Some(operation_id) = spec.operation_id.as_deref() else {
        return Ok(None);
    };
    let candidates = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            limit: Some(2),
            filters: operation_container_filters(scope, operation_id),
            ..Default::default()
        }))
        .await
        .map_err(|error| {
            AppError::internal(format!(
                "failed to discover an existing container operation: {error}"
            ))
        })?;
    if candidates.len() > 1 {
        return Err(AppError::conflict(
            "multiple containers claim the same operation identity",
        ));
    }
    let Some(id) = candidates
        .into_iter()
        .next()
        .and_then(|container| container.id)
    else {
        return Ok(None);
    };
    let existing = docker.inspect_container(&id, None).await.map_err(|error| {
        AppError::internal(format!(
            "container operation {id} was discovered but could not be inspected: {error}"
        ))
    })?;
    validate_operation_container(
        &existing,
        scope,
        spec,
        launch_fingerprint,
        restricted_profile,
        storage_quota_enforced,
    )
    .map(Some)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FailedStartAction {
    TreatAsStarted,
    RetainForRetry,
    RemoveOwned,
}

fn container_is_running(info: &ContainerInspectResponse) -> bool {
    info.state.as_ref().and_then(|state| state.status) == Some(ContainerStateStatusEnum::RUNNING)
}

fn docker_exec_target_error(error: &bollard::errors::Error) -> bool {
    matches!(
        error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404 | 409,
            ..
        }
    )
}

fn participant_exec_error(context: &str, error: impl std::fmt::Display) -> ContainerExecError {
    ContainerExecError::Participant(AppError::internal(format!("{context}: {error}")))
}

fn platform_exec_error(context: &str, error: impl std::fmt::Display) -> ContainerExecError {
    ContainerExecError::Platform(AppError::internal(format!("{context}: {error}")))
}

type DockerExecOutput = std::pin::Pin<
    Box<
        dyn futures::Stream<Item = Result<bollard::container::LogOutput, bollard::errors::Error>>
            + Send,
    >,
>;

fn attached_exec_output(
    started: bollard::exec::StartExecResults,
    admission: &ContainerExecAdmission,
) -> Result<DockerExecOutput, ContainerExecError> {
    let bollard::exec::StartExecResults::Attached { output, .. } = started else {
        return Err(ContainerExecError::Platform(AppError::internal(
            "Docker exec unexpectedly started without an attached process",
        )));
    };
    admission.mark_admitted();
    Ok(output)
}

impl DockerContainerManager {
    /// Execute with scoring attribution. Docker's structured 404/409 responses
    /// and an inspected non-running target belong to that service. Transport,
    /// malformed-response, and otherwise-unattributed daemon failures belong
    /// to the platform. A failed start is participant-owned only when Docker's
    /// exec record proves a non-zero process result; no daemon message parsing
    /// is used.
    pub(super) async fn exec_with_attribution(
        &self,
        id: &str,
        cmd: Vec<String>,
        admission: ContainerExecAdmission,
    ) -> Result<String, ContainerExecError> {
        let docker = self.client().map_err(ContainerExecError::Platform)?;
        let info = match docker.inspect_container(id, None).await {
            Ok(info) => info,
            Err(error) if is_not_found(&error) => {
                return Err(ContainerExecError::Participant(AppError::not_found(
                    format!("container not found: {id}"),
                )));
            }
            Err(error) => return Err(platform_exec_error("inspect container", error)),
        };
        verify_container_scope(&info, &self.scope).map_err(ContainerExecError::Platform)?;
        if !container_is_running(&info) {
            return Err(ContainerExecError::Participant(AppError::conflict(
                "container is not running",
            )));
        }
        let canonical_id = info.id.as_deref().ok_or_else(|| {
            ContainerExecError::Platform(AppError::internal(
                "inspected container has no backend identity",
            ))
        })?;
        let exec = docker
            .create_exec(
                canonical_id,
                bollard::exec::CreateExecOptions {
                    cmd: Some(cmd),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| {
                if docker_exec_target_error(&error) {
                    participant_exec_error("create_exec", error)
                } else {
                    platform_exec_error("create_exec", error)
                }
            })?;
        let started = match docker.start_exec(&exec.id, None).await {
            Ok(started) => started,
            Err(error) => {
                let process_failed = if docker_exec_target_error(&error) {
                    true
                } else {
                    docker.inspect_exec(&exec.id).await.is_ok_and(|state| {
                        state.running == Some(false)
                            && state.exit_code.is_some_and(|code| code != 0)
                    })
                };
                return Err(if process_failed {
                    participant_exec_error("start_exec", error)
                } else {
                    platform_exec_error("start_exec", error)
                });
            }
        };
        let mut output = attached_exec_output(started, &admission)?;
        let mut out = String::new();
        while let Some(chunk) = output.next().await {
            let msg =
                chunk.map_err(|error| platform_exec_error("read container exec output", error))?;
            let rendered = msg.to_string();
            if out.len().saturating_add(rendered.len()) > MAX_EXEC_OUTPUT_BYTES {
                return Err(ContainerExecError::Participant(AppError::internal(
                    "container exec output exceeded 1 MiB",
                )));
            }
            out.push_str(&rendered);
        }
        Ok(out)
    }
}

/// Reconcile a failed Docker start without racing an idempotent adopter. A
/// stable operation is never removed here: another replica may have inspected
/// the CREATED container and be starting it concurrently.
pub(super) fn failed_start_action(
    stable_operation: bool,
    inspected: Option<&ContainerInspectResponse>,
) -> FailedStartAction {
    let status = inspected
        .and_then(|info| info.state.as_ref())
        .and_then(|state| state.status);
    match status {
        Some(ContainerStateStatusEnum::RUNNING) => FailedStartAction::TreatAsStarted,
        Some(
            ContainerStateStatusEnum::CREATED
            | ContainerStateStatusEnum::EXITED
            | ContainerStateStatusEnum::DEAD,
        ) if !stable_operation => FailedStartAction::RemoveOwned,
        _ => FailedStartAction::RetainForRetry,
    }
}

pub(super) fn docker_liveness(state: Option<ContainerStateStatusEnum>) -> ContainerLiveness {
    match state {
        Some(ContainerStateStatusEnum::RUNNING) => ContainerLiveness::Running,
        Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD) => {
            ContainerLiveness::Stopped
        }
        _ => ContainerLiveness::Unknown,
    }
}

/// Whether a bollard error is a Docker "404 Not Found" (container/image gone).
pub(super) fn is_not_found(err: &bollard::errors::Error) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}

/// Docker 409 Conflict — e.g. the container name is already taken.
pub(super) fn is_conflict(err: &bollard::errors::Error) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 409,
            ..
        }
    )
}

pub(super) fn verify_container_scope(
    info: &ContainerInspectResponse,
    scope: &str,
) -> AppResult<()> {
    let labels = info
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref());
    if labels_match_scope(labels, scope) {
        Ok(())
    } else {
        Err(AppError::conflict(
            "container identity belongs to another rsctf installation",
        ))
    }
}

impl DockerContainerManager {
    /// Resolve an identifier through Docker and prove that the resulting
    /// container belongs to this installation before any lifecycle operation.
    /// Callers use the inspected canonical ID for the follow-up request so a
    /// container name cannot be rebound between the ownership check and use.
    pub(super) async fn inspect_scoped_container(
        &self,
        docker: &Docker,
        id: &str,
    ) -> AppResult<Option<ContainerInspectResponse>> {
        match docker.inspect_container(id, None).await {
            Ok(info) => {
                verify_container_scope(&info, &self.scope)?;
                Ok(Some(info))
            }
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(AppError::internal(format!(
                "failed to inspect container: {error}"
            ))),
        }
    }

    pub(super) async fn start_or_reconcile_container(
        &self,
        docker: &Docker,
        id: &str,
        stable_operation: bool,
        adopted: bool,
    ) -> AppResult<()> {
        let already_running = adopted
            && docker
                .inspect_container(id, None)
                .await
                .ok()
                .as_ref()
                .is_some_and(container_is_running);
        if already_running {
            return Ok(());
        }
        let Err(error) = docker
            .start_container(id, None::<StartContainerOptions<String>>)
            .await
        else {
            return Ok(());
        };
        let inspected = match self.inspect_scoped_container(docker, id).await {
            Ok(info) => info,
            Err(reinspect_error) => {
                tracing::warn!(%id, %reinspect_error,
                    "failed-start container ownership reinspection failed; retaining it for retry");
                None
            }
        };
        match failed_start_action(stable_operation, inspected.as_ref()) {
            FailedStartAction::TreatAsStarted => Ok(()),
            FailedStartAction::RetainForRetry => Err(AppError::internal(format!(
                "failed to start container: {error}"
            ))),
            FailedStartAction::RemoveOwned => {
                if let Some(canonical_id) = inspected.as_ref().and_then(|info| info.id.as_deref()) {
                    let _ = docker
                        .remove_container(
                            canonical_id,
                            Some(RemoveContainerOptions {
                                v: false,
                                force: true,
                                link: false,
                            }),
                        )
                        .await;
                }
                Err(AppError::internal(format!(
                    "failed to start container: {error}"
                )))
            }
        }
    }

    /// Pull one resource sample from Docker's non-streaming stats endpoint.
    /// Errors degrade to empty samples so lifecycle queries remain available.
    pub(super) async fn sample_stats(&self, id: &str) -> (Option<u64>, Option<f64>) {
        let Ok(docker) = self.client() else {
            return (None, None);
        };

        let mut stream = docker.stats(
            id,
            Some(StatsOptions {
                stream: false,
                one_shot: true,
            }),
        );

        let stats = match stream.next().await {
            Some(Ok(stats)) => stats,
            Some(Err(e)) => {
                tracing::debug!(id = %id, error = %e, "container stats sample failed");
                return (None, None);
            }
            None => return (None, None),
        };

        let memory_bytes = stats.memory_stats.usage;
        let cpu_delta = stats
            .cpu_stats
            .cpu_usage
            .total_usage
            .saturating_sub(stats.precpu_stats.cpu_usage.total_usage);
        let system_delta = stats
            .cpu_stats
            .system_cpu_usage
            .unwrap_or(0)
            .saturating_sub(stats.precpu_stats.system_cpu_usage.unwrap_or(0));

        let mut online_cpus = stats.cpu_stats.online_cpus.unwrap_or(0);
        if online_cpus == 0 {
            if let Some(percpu) = stats.cpu_stats.cpu_usage.percpu_usage.as_ref() {
                if !percpu.is_empty() {
                    online_cpus = percpu.len() as u64;
                }
            }
        }

        let cpu_usage = if system_delta > 0 && online_cpus > 0 {
            Some(cpu_delta as f64 / system_delta as f64 * online_cpus as f64)
        } else {
            None
        };

        (memory_bytes, cpu_usage)
    }
}

/// Select Docker when its daemon is reachable, otherwise use the no-op backend.
pub fn from_env() -> std::sync::Arc<dyn ContainerManager> {
    match DockerContainerManager::connect() {
        Ok(manager) if manager.reachable_blocking() => {
            tracing::info!(
                endpoint = ?manager.endpoint,
                "docker daemon reachable; using DockerContainerManager"
            );
            std::sync::Arc::new(manager)
        }
        Ok(_) => {
            tracing::warn!(
                "docker daemon not reachable (ping failed); \
                 falling back to NoopContainerManager (containers disabled)"
            );
            std::sync::Arc::new(NoopContainerManager)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not connect to docker; \
                 falling back to NoopContainerManager (containers disabled)"
            );
            std::sync::Arc::new(NoopContainerManager)
        }
    }
}

/// Select Docker without silently degrading to the no-op backend.
pub fn from_env_required() -> AppResult<std::sync::Arc<dyn ContainerManager>> {
    let manager = DockerContainerManager::connect()?;
    if !manager.reachable_blocking() {
        return Err(AppError::internal(
            "RSCTF_CONTAINER_BACKEND=docker but the Docker daemon is unreachable",
        ));
    }
    tracing::info!(
        endpoint = ?manager.endpoint,
        "docker daemon reachable; using explicitly selected DockerContainerManager"
    );
    Ok(std::sync::Arc::new(manager))
}

#[cfg(test)]
mod exec_admission_tests {
    use super::*;

    #[test]
    fn previous_release_fingerprint_is_accepted_only_without_callback_ports() {
        let mut spec = ContainerSpec {
            game_kind: GameKind::Jeopardy,
            image: format!("registry.example/challenge@sha256:{}", "a".repeat(64)),
            memory_limit: 64,
            cpu_count: 1,
            storage_limit: crate::services::container::DEFAULT_CONTAINER_STORAGE_MB,
            expose_port: 8080,
            publish_port: false,
            proxy_only: false,
            env: Vec::new(),
            flag: Some("flag{legacy}".to_string()),
            ad_network: None,
            allow_egress: false,
            control_plane_callback_ports: Vec::new(),
            network_mode: crate::utils::enums::NetworkMode::Isolated,
            operation_id: Some("legacy-operation".to_string()),
        };
        let legacy = DockerLaunchSpecV4 {
            revision: 4,
            game_kind: spec.game_kind,
            image: &spec.image,
            memory_limit: spec.memory_limit,
            cpu_count: spec.cpu_count,
            storage_limit: spec.storage_limit,
            expose_port: spec.expose_port,
            publish_port: spec.publish_port,
            proxy_only: spec.proxy_only,
            env: &spec.env,
            flag: spec.flag.as_deref(),
            ad_network: spec.ad_network.as_deref(),
            allow_egress: spec.allow_egress,
            network_mode: spec.network_mode,
        };
        let legacy_fingerprint = crate::utils::codec::sha256_hex(
            &serde_json::to_vec(&legacy).expect("legacy launch spec serializes"),
        );
        let scope = "installation-scope";
        let labels = std::collections::HashMap::from([
            (MANAGED_LABEL.to_string(), scope.to_string()),
            (SCOPE_LABEL.to_string(), scope.to_string()),
            (OPERATION_LABEL.to_string(), "legacy-operation".to_string()),
            (LAUNCH_SPEC_LABEL.to_string(), legacy_fingerprint),
            (
                STORAGE_QUOTA_LABEL.to_string(),
                STORAGE_QUOTA_FALLBACK.to_string(),
            ),
        ]);
        let existing = ContainerInspectResponse {
            id: Some("legacy-container".to_string()),
            config: Some(bollard::models::ContainerConfig {
                image: Some(spec.image.clone()),
                labels: Some(labels),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            validate_operation_container(
                &existing,
                scope,
                &spec,
                &launch_spec_fingerprint(&spec),
                false,
                false,
            )
            .unwrap(),
            "legacy-container"
        );

        spec.control_plane_callback_ports.push(8080);
        assert!(matches!(
            validate_operation_container(
                &existing,
                scope,
                &spec,
                &launch_spec_fingerprint(&spec),
                false,
                false,
            ),
            Err(AppError::Conflict(_))
        ));
    }

    #[test]
    fn docker_admission_begins_only_for_an_attached_exec() {
        let detached_admission = ContainerExecAdmission::default();
        let detached = attached_exec_output(
            bollard::exec::StartExecResults::Detached,
            &detached_admission,
        );
        assert!(matches!(detached, Err(ContainerExecError::Platform(_))));
        assert!(!detached_admission.is_admitted());

        let attached_admission = ContainerExecAdmission::default();
        let attached = bollard::exec::StartExecResults::Attached {
            output: Box::pin(futures::stream::empty()),
            input: Box::pin(tokio::io::sink()),
        };
        assert!(attached_exec_output(attached, &attached_admission).is_ok());
        assert!(attached_admission.is_admitted());
    }
}
