//! Backend constructors: Docker when reachable, the no-op backend otherwise,
//! and the capacity-gated variants the process entry point selects.

use super::super::{ContainerManager, DockerContainerManager, NoopContainerManager};
use crate::utils::error::{AppError, AppResult};

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

/// Docker-or-Noop selection whose Docker manager admits creates against the
/// durable local capacity ceiling. Configuration errors fail startup.
pub fn from_env_gated(pool: sqlx::PgPool) -> AppResult<std::sync::Arc<dyn ContainerManager>> {
    match DockerContainerManager::connect() {
        Ok(manager) if manager.reachable_blocking() => {
            tracing::info!(
                endpoint = ?manager.endpoint,
                "docker daemon reachable; using DockerContainerManager"
            );
            super::super::capacity::gate(manager, pool)
        }
        _ => Ok(from_env()),
    }
}

/// Choose the local backend for `RSCTF_CONTAINER_BACKEND=docker` (Docker is
/// required) or `auto` (Kubernetes wins when reachable, otherwise Docker or
/// the no-op backend). Local Docker creates are admitted against the durable
/// host ceiling in either case.
pub fn select_local_backend(
    pool: sqlx::PgPool,
    docker_required: bool,
) -> AppResult<std::sync::Arc<dyn ContainerManager>> {
    if docker_required {
        from_env_required_gated(pool)
    } else if let Some(kubernetes) = crate::services::k8s::from_env() {
        Ok(kubernetes)
    } else {
        from_env_gated(pool)
    }
}

/// Explicit Docker selection with the durable local capacity gate.
pub fn from_env_required_gated(
    pool: sqlx::PgPool,
) -> AppResult<std::sync::Arc<dyn ContainerManager>> {
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
    super::super::capacity::gate(manager, pool)
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
