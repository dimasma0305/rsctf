use async_trait::async_trait;
use rsctf_worker_protocol::ValidatedWorkloadSpec;

use super::{ContainerInfo, ContainerSpec};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct ContainerStatus {
    pub id: String,
    pub status: String,
    pub memory_bytes: Option<u64>,
    pub cpu_usage: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerBackendKind {
    None,
    Docker,
    Kubernetes,
    Worker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerLiveness {
    Running,
    Stopped,
    Unknown,
}

/// Pluggable lifecycle boundary shared by local and trusted-worker runtimes.
#[async_trait]
pub trait ContainerManager: Send + Sync {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::None
    }

    fn requires_proxy(&self) -> bool {
        false
    }

    /// Whether aggregate/worker-local Jeopardy workloads are available in
    /// addition to the backend reported by `backend_kind`.
    fn supports_worker_workloads(&self) -> bool {
        self.backend_kind() == ContainerBackendKind::Worker
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo>;

    async fn create_workload(
        &self,
        _spec: ValidatedWorkloadSpec,
        _operation_id: Option<String>,
        _flag: Option<String>,
    ) -> AppResult<ContainerInfo> {
        Err(AppError::bad_request(
            "aggregate workloads require RSCTF_CONTAINER_BACKEND=worker",
        ))
    }

    async fn destroy(&self, id: &str) -> AppResult<()>;
    async fn query(&self, id: &str) -> AppResult<ContainerStatus>;

    async fn inspect_liveness(&self, id: &str) -> AppResult<ContainerLiveness> {
        match self.query(id).await {
            Ok(status) if status.status == "running" => Ok(ContainerLiveness::Running),
            Ok(status) if matches!(status.status.as_str(), "exited" | "destroyed") => {
                Ok(ContainerLiveness::Stopped)
            }
            Ok(_) => Ok(ContainerLiveness::Unknown),
            Err(AppError::NotFound(_)) => Ok(ContainerLiveness::Stopped),
            Err(error) => Err(error),
        }
    }

    async fn is_running(&self, id: &str) -> bool {
        matches!(
            self.inspect_liveness(id).await,
            Ok(ContainerLiveness::Running)
        )
    }

    async fn image_exists(&self, _image: &str) -> bool {
        true
    }

    async fn list_managed(&self) -> Vec<String> {
        Vec::new()
    }

    async fn ensure_network(&self, _name: &str, _subnet: &str) -> AppResult<()> {
        Ok(())
    }

    async fn snapshot_changes(&self, _id: &str) -> AppResult<Vec<FileChange>> {
        Ok(Vec::new())
    }

    async fn exec(&self, _id: &str, _cmd: Vec<String>) -> AppResult<String> {
        Err(AppError::bad_request(
            "exec is not supported by this backend",
        ))
    }

    async fn export(&self, _id: &str) -> AppResult<Vec<u8>> {
        Err(AppError::bad_request(
            "snapshot export is not supported by this backend",
        ))
    }
}

#[derive(Debug, Default, Clone)]
pub struct NoopContainerManager;

#[async_trait]
impl ContainerManager for NoopContainerManager {
    async fn create(&self, _spec: ContainerSpec) -> AppResult<ContainerInfo> {
        Err(AppError::bad_request("no container backend configured"))
    }

    async fn destroy(&self, _id: &str) -> AppResult<()> {
        Err(AppError::bad_request("no container backend configured"))
    }

    async fn query(&self, _id: &str) -> AppResult<ContainerStatus> {
        Err(AppError::bad_request("no container backend configured"))
    }
}
