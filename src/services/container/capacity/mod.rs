//! Replica-safe aggregate admission for the local Docker backend.
//!
//! [`CapacityGatedDockerManager`] wraps the concrete Docker manager so every
//! local create path (player, exercise, shared, A&D/KotH services, admin
//! tests) is accounted before a workload exists. PostgreSQL owns the
//! authority: a durable reservation keyed by the container operation
//! identity is admitted under the per-host capacity row lock, released when
//! the runtime is removed or the launch fails, and reconciled against the
//! labeled runtime inventory by the existing orphan sweep.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bollard::Docker;

use super::{
    docker_installation_scope, ContainerExecAdmission, ContainerExecError, ContainerFile,
    ContainerInfo, ContainerLiveness, ContainerManager, ContainerSpec, ContainerStatus,
    DockerContainerManager, FileChange,
};
use crate::utils::error::{AppError, AppResult};

pub mod config;
pub mod store;
#[cfg(test)]
mod tests;

pub use config::{HostResources, LocalCapacity};
pub use store::{ManagedRuntime, Reservation};

/// The orphan sweep lists the inventory every pass; reconciling reservations
/// on every listing would only add write load without new information.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);

impl Reservation {
    /// Account one container: whole CPUs become millicores and the memory
    /// limit is in mebibytes. One container occupies one running slot.
    pub fn for_spec(spec: &ContainerSpec) -> AppResult<Self> {
        let key = spec
            .operation_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let cpu_millis = i64::from(spec.cpu_count)
            .checked_mul(1_000)
            .filter(|value| *value > 0)
            .ok_or_else(|| AppError::bad_request("container CPU count is invalid"))?;
        let memory_bytes = i64::from(spec.memory_limit)
            .checked_mul(1024 * 1024)
            .filter(|value| *value > 0)
            .ok_or_else(|| AppError::bad_request("container memory limit is invalid"))?;
        Ok(Self {
            key,
            cpu_millis,
            memory_bytes,
            slots: 1,
        })
    }
}

/// Probe the daemon host's CPU and memory with the same dedicated-thread
/// runtime pattern as the startup reachability ping.
fn host_resources_blocking(docker: Docker) -> AppResult<HostResources> {
    let info = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| AppError::internal(error.to_string()))?;
        rt.block_on(async move {
            tokio::time::timeout(Duration::from_secs(5), docker.info())
                .await
                .map_err(|_| AppError::internal("Docker info probe timed out"))?
                .map_err(|error| AppError::internal(format!("Docker info probe failed: {error}")))
        })
    })
    .join()
    .map_err(|_| AppError::internal("Docker info probe thread panicked"))??;
    let cpu_millis = info
        .ncpu
        .unwrap_or(0)
        .max(0)
        .saturating_mul(1_000)
        .max(config::MINIMUM_CPU_MILLIS);
    let memory_bytes = info
        .mem_total
        .unwrap_or(0)
        .max(config::MINIMUM_MEMORY_BYTES);
    Ok(HostResources {
        cpu_millis,
        memory_bytes,
    })
}

/// Docker manager whose creates are admitted against a durable host ceiling.
pub struct CapacityGatedDockerManager {
    inner: DockerContainerManager,
    pool: sqlx::PgPool,
    host_key: String,
    capacity: LocalCapacity,
    last_reconcile: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for CapacityGatedDockerManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CapacityGatedDockerManager")
            .field("host_key", &self.host_key)
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

impl CapacityGatedDockerManager {
    /// Wrap a reachable Docker manager. Fails startup on invalid capacity
    /// configuration or an unreadable daemon host.
    pub fn from_env(inner: DockerContainerManager, pool: sqlx::PgPool) -> AppResult<Self> {
        let docker = inner.client()?.clone();
        let host = host_resources_blocking(docker)?;
        let capacity = config::from_env(host)?;
        tracing::info!(
            host_cpu_millis = host.cpu_millis,
            host_memory_bytes = host.memory_bytes,
            cpu_millis = capacity.cpu_millis,
            memory_bytes = capacity.memory_bytes,
            slots = capacity.slots,
            "local container capacity admission enabled"
        );
        Ok(Self::with_capacity(inner, pool, capacity))
    }

    pub fn with_capacity(
        inner: DockerContainerManager,
        pool: sqlx::PgPool,
        capacity: LocalCapacity,
    ) -> Self {
        Self {
            inner,
            pool,
            host_key: docker_installation_scope(),
            capacity,
            last_reconcile: Mutex::new(None),
        }
    }

    pub fn capacity(&self) -> LocalCapacity {
        self.capacity
    }

    fn reconcile_due(&self) -> bool {
        let mut last = self
            .last_reconcile
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Instant::now();
        if last.is_some_and(|previous| now.duration_since(previous) < RECONCILE_INTERVAL) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// Build the gated manager the composition root installs for Docker.
pub fn gate(
    inner: DockerContainerManager,
    pool: sqlx::PgPool,
) -> AppResult<Arc<dyn ContainerManager>> {
    Ok(Arc::new(CapacityGatedDockerManager::from_env(inner, pool)?))
}

#[async_trait]
impl ContainerManager for CapacityGatedDockerManager {
    fn backend_kind(&self) -> super::ContainerBackendKind {
        self.inner.backend_kind()
    }

    async fn storage_quota_enforced(&self) -> Option<bool> {
        self.inner.storage_quota_enforced().await
    }

    async fn image_exists(&self, image: &str) -> bool {
        self.inner.image_exists(image).await
    }

    /// Lists the labeled inventory and, at a bounded rate, reconciles
    /// reservations against it. The orphan sweep is the only production
    /// caller, so this is the single inventory scan after a crash.
    async fn list_managed(&self) -> Vec<String> {
        let inventory = match self.inner.managed_inventory().await {
            Ok(inventory) => inventory,
            Err(error) => {
                tracing::warn!(%error, "list_managed: docker inventory failed");
                return Vec::new();
            }
        };
        if self.reconcile_due() {
            match store::reconcile(&self.pool, &self.host_key, &inventory).await {
                Ok(released) if released > 0 => {
                    tracing::info!(released, "released stale local container reservations");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "local container reservation reconciliation failed");
                }
            }
        }
        inventory
            .into_iter()
            .map(|runtime| runtime.backend_id)
            .collect()
    }

    async fn find_operation_runtime(&self, operation_id: &str) -> AppResult<Option<String>> {
        self.inner.find_operation_runtime(operation_id).await
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        let request = Reservation::for_spec(&spec)?;
        store::reserve(&self.pool, &self.host_key, &self.capacity, &request).await?;
        match self.inner.create(spec).await {
            Ok(info) => {
                if let Err(error) =
                    store::attach(&self.pool, &self.host_key, &request.key, &info.id).await
                {
                    // The inventory reconciliation re-attaches by operation
                    // label; an unlabeled launch ages out after its grace.
                    tracing::warn!(
                        reservation = %request.key,
                        backend_id = %info.id,
                        %error,
                        "failed to attach local container reservation"
                    );
                }
                Ok(info)
            }
            Err(error) => {
                if let Err(release_error) =
                    store::release_key(&self.pool, &self.host_key, &request.key).await
                {
                    tracing::warn!(
                        reservation = %request.key,
                        error = %release_error,
                        "failed to release local container reservation after launch failure"
                    );
                }
                Err(error)
            }
        }
    }

    async fn destroy(&self, id: &str) -> AppResult<()> {
        self.inner.destroy(id).await?;
        if let Err(error) = store::release_backend(&self.pool, &self.host_key, id).await {
            tracing::warn!(backend_id = %id, %error, "failed to release local container reservation");
        }
        Ok(())
    }

    async fn ensure_network(&self, name: &str, subnet: &str) -> AppResult<()> {
        self.inner.ensure_network(name, subnet).await
    }

    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        self.inner.query(id).await
    }

    async fn inspect_liveness(&self, id: &str) -> AppResult<ContainerLiveness> {
        self.inner.inspect_liveness(id).await
    }

    async fn snapshot_changes(&self, id: &str) -> AppResult<Vec<FileChange>> {
        self.inner.snapshot_changes(id).await
    }

    async fn read_file(&self, id: &str, path: &str, limit: usize) -> AppResult<ContainerFile> {
        self.inner.read_file(id, path, limit).await
    }

    async fn exec(&self, id: &str, cmd: Vec<String>) -> AppResult<String> {
        self.inner.exec(id, cmd).await
    }

    async fn exec_classified(
        &self,
        id: &str,
        cmd: Vec<String>,
        admission: ContainerExecAdmission,
    ) -> Result<String, ContainerExecError> {
        self.inner.exec_classified(id, cmd, admission).await
    }

    async fn resolve_interactive_exec_target(&self, id: &str) -> AppResult<String> {
        self.inner.resolve_interactive_exec_target(id).await
    }

    async fn export(&self, id: &str) -> AppResult<Vec<u8>> {
        self.inner.export(id).await
    }
}
