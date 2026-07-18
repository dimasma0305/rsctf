//! Route ordinary Jeopardy containers to trusted workers while retaining the
//! established local backend for A&D/KotH networking and maintenance tools.

use std::sync::Arc;

use async_trait::async_trait;
use rsctf_worker_protocol::{GameKind, ValidatedWorkloadSpec};

use super::{parse_worker_handle, WorkerContainerManager};
use crate::services::container::{
    ContainerBackendKind, ContainerInfo, ContainerLiveness, ContainerManager, ContainerSpec,
    ContainerStatus, FileChange,
};
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CreateTarget {
    Local,
    Worker,
}

fn create_target(spec: &ContainerSpec) -> CreateTarget {
    if spec.game_kind == GameKind::Jeopardy {
        CreateTarget::Worker
    } else {
        CreateTarget::Local
    }
}

#[derive(Clone)]
pub struct HybridWorkerContainerManager {
    local: Arc<dyn ContainerManager>,
    worker: WorkerContainerManager,
}

impl HybridWorkerContainerManager {
    pub fn new(
        local: Arc<dyn ContainerManager>,
        worker: WorkerContainerManager,
    ) -> AppResult<Self> {
        if !matches!(
            local.backend_kind(),
            ContainerBackendKind::Docker | ContainerBackendKind::Kubernetes
        ) {
            return Err(AppError::bad_request(
                "a worker hybrid requires a local Docker or Kubernetes backend",
            ));
        }
        Ok(Self { local, worker })
    }

    fn is_worker_id(id: &str) -> bool {
        parse_worker_handle(id).is_some()
    }
}

#[async_trait]
impl ContainerManager for HybridWorkerContainerManager {
    fn backend_kind(&self) -> ContainerBackendKind {
        self.local.backend_kind()
    }

    fn requires_proxy(&self) -> bool {
        true
    }

    fn supports_worker_workloads(&self) -> bool {
        true
    }

    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        match create_target(&spec) {
            CreateTarget::Local => self.local.create(spec).await,
            CreateTarget::Worker => self.worker.create(spec).await,
        }
    }

    async fn create_workload(
        &self,
        spec: ValidatedWorkloadSpec,
        operation_id: Option<String>,
        flag: Option<String>,
    ) -> AppResult<ContainerInfo> {
        if spec.game_kind == GameKind::Jeopardy {
            self.worker.create_workload(spec, operation_id, flag).await
        } else {
            self.local.create_workload(spec, operation_id, flag).await
        }
    }

    async fn destroy(&self, id: &str) -> AppResult<()> {
        if Self::is_worker_id(id) {
            self.worker.destroy(id).await
        } else {
            self.local.destroy(id).await
        }
    }

    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        if Self::is_worker_id(id) {
            self.worker.query(id).await
        } else {
            self.local.query(id).await
        }
    }

    async fn inspect_liveness(&self, id: &str) -> AppResult<ContainerLiveness> {
        if Self::is_worker_id(id) {
            self.worker.inspect_liveness(id).await
        } else {
            self.local.inspect_liveness(id).await
        }
    }

    async fn image_exists(&self, image: &str) -> bool {
        self.local.image_exists(image).await
    }

    async fn list_managed(&self) -> Vec<String> {
        self.local.list_managed().await
    }

    async fn ensure_network(&self, name: &str, subnet: &str) -> AppResult<()> {
        self.local.ensure_network(name, subnet).await
    }

    async fn snapshot_changes(&self, id: &str) -> AppResult<Vec<FileChange>> {
        if Self::is_worker_id(id) {
            return Err(AppError::bad_request(
                "snapshot changes are not supported for remote workers",
            ));
        }
        self.local.snapshot_changes(id).await
    }

    async fn exec(&self, id: &str, command: Vec<String>) -> AppResult<String> {
        if Self::is_worker_id(id) {
            return Err(AppError::bad_request(
                "exec is not supported for remote workers",
            ));
        }
        self.local.exec(id, command).await
    }

    async fn export(&self, id: &str) -> AppResult<Vec<u8>> {
        if Self::is_worker_id(id) {
            return Err(AppError::bad_request(
                "snapshot export is not supported for remote workers",
            ));
        }
        self.local.export(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_ad_network_spec(game_kind: GameKind) -> ContainerSpec {
        ContainerSpec {
            game_kind,
            image: "sha256:test".into(),
            memory_limit: 64,
            cpu_count: 1,
            expose_port: 8080,
            env: Vec::new(),
            flag: None,
            ad_network: None,
            allow_egress: false,
            operation_id: None,
        }
    }

    #[test]
    fn no_network_attack_defense_routes_local() {
        assert_eq!(
            create_target(&no_ad_network_spec(GameKind::AttackDefense)),
            CreateTarget::Local
        );
    }

    #[test]
    fn no_network_koth_routes_local() {
        assert_eq!(
            create_target(&no_ad_network_spec(GameKind::KingOfTheHill)),
            CreateTarget::Local
        );
    }

    #[test]
    fn no_network_jeopardy_routes_remote() {
        assert_eq!(
            create_target(&no_ad_network_spec(GameKind::Jeopardy)),
            CreateTarget::Worker
        );
    }
}
