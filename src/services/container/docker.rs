use bollard::container::{
    DownloadFromContainerOptions, RemoveContainerOptions, StartContainerOptions, StatsOptions,
};
use bollard::models::{
    ContainerInspectResponse, ContainerStateStatusEnum, ImageInspect, SystemInfo,
};
use bollard::Docker;
use futures::StreamExt;
use rsctf_worker_protocol::GameKind;

use super::{
    labels_match_scope, ContainerExecAdmission, ContainerExecError, ContainerFile,
    ContainerLiveness, ContainerManager, ContainerSpec, DockerContainerManager,
    NoopContainerManager, MAX_EXEC_OUTPUT_BYTES,
};
use crate::utils::error::{AppError, AppResult};

mod retry;
pub(crate) use retry::launch_spec_fingerprint;
#[cfg(test)]
pub(super) use retry::launch_spec_matches;
pub(super) use retry::{
    adopt_operation_container, discover_operation_container, failed_start_action, FailedStartAction,
};

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
pub(super) const MAX_FILE_ARCHIVE_METADATA_BYTES: usize = 512 * 1024;
const ABSOLUTE_MAX_FILE_PREVIEW_BYTES: usize = 256 * 1024;

/// Retain a deterministic prefix of Docker's TAR stream. Returning true asks
/// the caller to drop the stream immediately, which cancels the daemon body
/// transfer for large files instead of reading the rest into memory.
pub(super) fn append_file_archive_chunk(
    out: &mut Vec<u8>,
    chunk: &[u8],
    limit: usize,
) -> AppResult<bool> {
    let remaining = limit.saturating_sub(out.len());
    let retained = chunk.len().min(remaining);
    out.try_reserve(retained)
        .map_err(|_| AppError::internal("failed to reserve file preview buffer"))?;
    out.extend_from_slice(&chunk[..retained]);
    Ok(out.len() >= limit)
}

/// Decode the first regular file from Docker's archive response. Archive
/// parsing is called from `spawn_blocking`; a FIFO, device, directory, or link
/// is rejected without executing or opening it inside the participant box.
pub(super) fn parse_file_archive(archive: &[u8], limit: usize) -> AppResult<ContainerFile> {
    use std::io::Read;

    let mut archive = tar::Archive::new(archive);
    let entries = archive.entries().map_err(|error| {
        AppError::bad_request(format!("invalid container file archive: {error}"))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            AppError::bad_request(format!("invalid container file archive entry: {error}"))
        })?;
        if !entry.header().entry_type().is_file() {
            return Err(AppError::bad_request("Only regular files can be previewed"));
        }
        let size = entry.size();
        let take = u64::try_from(limit).unwrap_or(u64::MAX).min(size);
        let mut bytes = Vec::with_capacity(usize::try_from(take).unwrap_or(limit));
        entry.take(take).read_to_end(&mut bytes).map_err(|error| {
            AppError::bad_request(format!("invalid container file data: {error}"))
        })?;
        if bytes.len() < usize::try_from(take).unwrap_or(limit) {
            return Err(AppError::bad_request(
                "Container file preview ended before its declared size",
            ));
        }
        return Ok(ContainerFile {
            truncated: size > bytes.len() as u64,
            size,
            bytes,
        });
    }
    Err(AppError::not_found("Container file archive was empty"))
}

impl DockerContainerManager {
    pub(super) async fn read_bounded_file(
        &self,
        id: &str,
        path: &str,
        limit: usize,
    ) -> AppResult<ContainerFile> {
        if limit == 0 || limit > ABSOLUTE_MAX_FILE_PREVIEW_BYTES {
            return Err(AppError::bad_request(
                "file preview limit must be between 1 byte and 256 KiB",
            ));
        }
        let docker = self.client()?;
        let info = self
            .inspect_scoped_container(docker, id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("container not found: {id}")))?;
        let canonical_id = info
            .id
            .as_deref()
            .ok_or_else(|| AppError::internal("inspected container has no backend identity"))?;
        let archive_limit = limit
            .checked_add(MAX_FILE_ARCHIVE_METADATA_BYTES)
            .ok_or_else(|| AppError::bad_request("file preview size overflow"))?;
        let mut archive = Vec::new();
        let mut stream = docker
            .download_from_container(canonical_id, Some(DownloadFromContainerOptions { path }));
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|error| {
                if is_not_found(&error) {
                    AppError::not_found(format!("file not found in container: {path}"))
                } else {
                    AppError::internal(format!("failed to read container file archive: {error}"))
                }
            })?;
            if append_file_archive_chunk(&mut archive, &bytes, archive_limit)? {
                break;
            }
        }
        tokio::task::spawn_blocking(move || parse_file_archive(&archive, limit))
            .await
            .map_err(|error| AppError::internal(format!("file preview task failed: {error}")))?
    }
}

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
                let canonical_id = inspected
                    .as_ref()
                    .and_then(|info| info.id.as_deref())
                    .ok_or_else(|| {
                        AppError::internal(format!(
                            "failed to start container and cleanup identity was unavailable: {error}"
                        ))
                    })?;
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
                    Ok(())
                    | Err(bollard::errors::Error::DockerResponseServerError {
                        status_code: 404,
                        ..
                    }) => {}
                    Err(cleanup_error) => {
                        return Err(AppError::internal(format!(
                            "failed to start container ({error}) and cleanup failed: {cleanup_error}"
                        )));
                    }
                }
                if stable_operation {
                    Err(AppError::conflict(
                        "Docker retry reached a terminal container; retry with a new operation identity",
                    ))
                } else {
                    Err(AppError::internal(format!(
                        "failed to start container: {error}"
                    )))
                }
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

#[cfg(test)]
mod file_archive_tests {
    use super::*;

    fn archive_header(entry_type: tar::EntryType, size: u64) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.set_path("preview").unwrap();
        header.set_mode(0o600);
        header.set_entry_type(entry_type);
        header.set_size(size);
        header.set_cksum();
        header.as_bytes().to_vec()
    }

    #[test]
    fn large_regular_file_returns_only_a_truthful_bounded_preview() {
        let limit = 16 * 1024;
        let declared = 1024 * 1024 * 1024u64;
        let mut archive = archive_header(tar::EntryType::Regular, declared);
        archive.extend(std::iter::repeat_n(b'x', limit));

        let file = parse_file_archive(&archive, limit).unwrap();
        assert_eq!(file.size, declared);
        assert_eq!(file.bytes.len(), limit);
        assert!(file.truncated);
    }

    #[test]
    fn fifo_and_device_like_entries_are_never_opened_as_files() {
        for entry_type in [
            tar::EntryType::Fifo,
            tar::EntryType::Block,
            tar::EntryType::Char,
        ] {
            let archive = archive_header(entry_type, 0);
            assert!(matches!(
                parse_file_archive(&archive, 1024),
                Err(AppError::BadRequest(_))
            ));
        }
    }

    #[test]
    fn docker_archive_collection_stops_at_the_memory_cap() {
        let mut output = vec![1; 8];
        assert!(append_file_archive_chunk(&mut output, &[2; 8], 12).unwrap());
        assert_eq!(output.len(), 12);
        assert_eq!(&output[8..], &[2; 4]);
    }

    #[tokio::test]
    #[ignore = "requires a disposable scoped Docker container in RSCTF_TEST_FORENSICS_CONTAINER_ID"]
    async fn real_docker_large_file_and_fifo_are_bounded_without_exec() {
        let id = std::env::var("RSCTF_TEST_FORENSICS_CONTAINER_ID")
            .expect("RSCTF_TEST_FORENSICS_CONTAINER_ID is required");
        let manager = DockerContainerManager::connect().unwrap();
        let large = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            manager.read_file(&id, "/tmp/large", 240 * 1024),
        )
        .await
        .expect("large-file archive read timed out")
        .unwrap();
        assert_eq!(large.bytes.len(), 240 * 1024);
        assert!(large.size >= 1024 * 1024);
        assert!(large.truncated);

        let fifo = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            manager.read_file(&id, "/tmp/blocked", 240 * 1024),
        )
        .await
        .expect("FIFO archive metadata read timed out");
        assert!(matches!(fifo, Err(AppError::BadRequest(_))));
    }
}
