use bollard::container::StatsOptions;
use bollard::models::ContainerStateStatusEnum;
use futures::StreamExt;

use super::{ContainerLiveness, ContainerManager, DockerContainerManager, NoopContainerManager};
use crate::utils::error::{AppError, AppResult};

pub(super) fn docker_liveness(state: Option<ContainerStateStatusEnum>) -> ContainerLiveness {
    match state {
        Some(ContainerStateStatusEnum::RUNNING) => ContainerLiveness::Running,
        Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD) => {
            ContainerLiveness::Stopped
        }
        _ => ContainerLiveness::Unknown,
    }
}

impl DockerContainerManager {
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
