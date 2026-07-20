use bollard::container::{RemoveContainerOptions, StartContainerOptions, StatsOptions};
use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum};
use bollard::Docker;
use futures::StreamExt;
use rsctf_worker_protocol::GameKind;

use super::{
    labels_match_scope, ContainerLiveness, ContainerManager, ContainerSpec, DockerContainerManager,
    NoopContainerManager,
};
use crate::utils::error::{AppError, AppResult};

pub(super) const LAUNCH_SPEC_LABEL: &str = "rsctf.launch-spec";

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerLaunchSpec<'a> {
    revision: u8,
    game_kind: GameKind,
    image: &'a str,
    memory_limit: i32,
    cpu_count: i32,
    expose_port: i32,
    env: &'a [(String, String)],
    flag: Option<&'a str>,
    ad_network: Option<&'a str>,
    allow_egress: bool,
}

/// Hash every launch-affecting caller input into a non-secret identity label.
/// Operation and installation identities have their own labels and deliberately
/// do not affect whether a crash retry represents the same workload.
pub(super) fn launch_spec_fingerprint(spec: &ContainerSpec) -> String {
    let canonical = DockerLaunchSpec {
        revision: 1,
        game_kind: spec.game_kind,
        image: &spec.image,
        memory_limit: spec.memory_limit,
        cpu_count: spec.cpu_count,
        expose_port: spec.expose_port,
        env: &spec.env,
        flag: spec.flag.as_deref(),
        ad_network: spec.ad_network.as_deref(),
        allow_egress: spec.allow_egress,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("the fixed Docker launch identity is always JSON serializable");
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FailedStartAction {
    TreatAsStarted,
    RetainForRetry,
    RemoveOwned,
}

fn container_is_running(info: &ContainerInspectResponse) -> bool {
    info.state.as_ref().and_then(|state| state.status) == Some(ContainerStateStatusEnum::RUNNING)
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
