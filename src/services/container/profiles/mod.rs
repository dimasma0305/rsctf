//! Per-challenge placement with immutable, backend-owned instance handles.
pub mod config;
pub mod policy;
#[cfg(test)]
mod real_tests;
mod router;
#[cfg(test)]
mod tests;

pub use config::{Isolation, ProfileBackend, RuntimeProfile};
pub use router::ProfileRouter;

use super::ContainerManager;
use crate::utils::error::{AppError, AppResult};
use std::sync::Arc;

pub async fn configure(
    default: Arc<dyn ContainerManager>,
    pool: sqlx::PgPool,
) -> AppResult<Arc<dyn ContainerManager>> {
    let definitions = config::from_env()?;
    config::checker_service_cidrs(
        &definitions,
        &crate::services::ad_vpn::services_cidr(),
        crate::services::ad_vpn::kubernetes_services_cidr().as_deref(),
    )?;
    let mut entries = Vec::new();
    for definition in definitions {
        let manager: Arc<dyn ContainerManager> = match definition.backend {
            ProfileBackend::Docker => {
                let manager = super::DockerContainerManager::connect()?.with_profile(&definition);
                if !manager.reachable_blocking() {
                    return Err(AppError::unavailable(format!(
                        "Docker runtime profile {} is unreachable",
                        definition.id
                    )));
                }
                super::capacity::gate(manager, pool.clone())?
            }
            ProfileBackend::Kubernetes => Arc::new(
                crate::services::k8s::KubernetesContainerManager::connect()
                    .await?
                    .with_profile(&definition)
                    .await?,
            ),
        };
        entries.push((definition, manager));
    }
    let router = ProfileRouter::new(default, entries)?;
    // Deployment edits cannot remove or reinterpret the owner of a live or
    // pending-cleanup handle. Retire old profiles with enabled:false.
    router.validate_persisted_handles(&pool).await?;
    Ok(Arc::new(router))
}
