use async_trait::async_trait;
use rsctf_worker_protocol::{GameKind, ValidatedWorkloadSpec};

use super::{ContainerInfo, ContainerSpec};
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Debug, Default)]
pub struct ContainerExecAdmission(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl ContainerExecAdmission {
    pub(crate) fn mark_admitted(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_admitted(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct ContainerFile {
    pub bytes: Vec<u8>,
    pub size: u64,
    pub truncated: bool,
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

pub fn should_use_platform_proxy(
    game_kind: GameKind,
    backend_requires_proxy: bool,
    platform_proxy_configured: bool,
    vpn_access_required: bool,
) -> bool {
    game_kind == GameKind::Jeopardy
        && !vpn_access_required
        && (backend_requires_proxy || platform_proxy_configured)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerLiveness {
    Running,
    Stopped,
    Unknown,
}

/// One backend-owned inventory page. `next_cursor` is opaque to the reaper and
/// must be supplied to the same backend on the next pass.
#[derive(Debug, Default)]
pub struct ManagedContainerPage {
    pub ids: Vec<String>,
    pub next_cursor: Option<String>,
}

/// Typed attribution for a container exec failure. Callers that affect event
/// scoring must distinguish a target controlled by the participant from an
/// unavailable platform backend; the ordinary [`ContainerManager::exec`]
/// method intentionally keeps its existing `AppResult` API.
#[derive(Debug, thiserror::Error)]
pub enum ContainerExecError {
    #[error("participant container exec failed: {0}")]
    Participant(#[source] AppError),
    #[error("container backend exec failed: {0}")]
    Platform(#[source] AppError),
}

impl ContainerExecError {
    pub fn into_app_error(self) -> AppError {
        match self {
            Self::Participant(error) | Self::Platform(error) => error,
        }
    }
}

/// Pluggable lifecycle boundary shared by local and trusted-worker runtimes.
#[async_trait]
pub trait ContainerManager: Send + Sync {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::None
    }

    /// Stable, non-secret identity of backend routing that a managed challenge
    /// uses to call rsctf. Backends with no extra route policy return `None`.
    fn managed_callback_routing_identity(&self) -> AppResult<Option<String>> {
        Ok(None)
    }

    fn requires_proxy(&self) -> bool {
        false
    }

    /// Whether aggregate/worker-local Jeopardy workloads are available in
    /// addition to the backend reported by `backend_kind`.
    fn supports_worker_workloads(&self) -> bool {
        self.backend_kind() == ContainerBackendKind::Worker
    }

    /// Whether this backend can enforce the configured writable-layer storage
    /// limit. `None` means the backend cannot report the capability.
    async fn storage_quota_enforced(&self) -> Option<bool> {
        None
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo>;

    async fn create_workload(
        &self,
        _spec: ValidatedWorkloadSpec,
        _operation_id: Option<String>,
        _flag: Option<String>,
        _proxy_only: bool,
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

    /// Fetch a bounded inventory page without first materializing every
    /// managed runtime. Backends used in production override this method with
    /// server-side pagination; the fallback only preserves compatibility for
    /// test and out-of-tree implementations.
    async fn list_managed_page(&self, _cursor: Option<&str>, limit: usize) -> ManagedContainerPage {
        let mut ids = self.list_managed().await;
        ids.truncate(limit);
        ManagedContainerPage {
            ids,
            next_cursor: None,
        }
    }

    async fn ensure_network(&self, _name: &str, _subnet: &str) -> AppResult<()> {
        Ok(())
    }

    async fn snapshot_changes(&self, _id: &str) -> AppResult<Vec<FileChange>> {
        Ok(Vec::new())
    }

    /// Read one regular file without launching a process inside an untrusted
    /// workload. Implementations must stop after `limit` bytes and report the
    /// original size and whether the preview was truncated.
    async fn read_file(&self, _id: &str, _path: &str, _limit: usize) -> AppResult<ContainerFile> {
        Err(AppError::bad_request(
            "bounded file inspection is not supported by this backend",
        ))
    }

    async fn exec(&self, _id: &str, _cmd: Vec<String>) -> AppResult<String> {
        Err(AppError::bad_request(
            "exec is not supported by this backend",
        ))
    }

    /// Exec with failure attribution for scoring-sensitive internal callers.
    /// Backends default to platform attribution so an unsupported or
    /// unavailable control plane can never become participant evidence merely
    /// because it was surfaced as a generic application error.
    async fn exec_classified(
        &self,
        id: &str,
        cmd: Vec<String>,
        _admission: ContainerExecAdmission,
    ) -> Result<String, ContainerExecError> {
        self.exec(id, cmd)
            .await
            .map_err(ContainerExecError::Platform)
    }

    /// Resolve a local interactive-exec target to a backend-canonical identity
    /// after applying the backend's ownership checks. The A&D SSH bridge keeps
    /// the stream attached itself, so it cannot use the bounded-output `exec`
    /// method, but it must pass through the same installation boundary first.
    async fn resolve_interactive_exec_target(&self, _id: &str) -> AppResult<String> {
        Err(AppError::bad_request(
            "interactive exec is not supported by this backend",
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
