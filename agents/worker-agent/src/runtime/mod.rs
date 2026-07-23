mod docker;

use std::sync::Arc;

use async_trait::async_trait;
use rsctf_worker_protocol::{
    CommandError, CommandErrorCode, EnsureAbsent, EnsureWorkload, InteractiveExecRequest,
    InventoryItem, Platform, ResourceUsage, RuntimeDescriptor, TcpProxyRequest, WorkerCapabilities,
    WorkerCapacity, WorkloadStatus, WriteFlag,
};
use thiserror::Error;
use tokio::net::TcpStream;
use uuid::Uuid;

use crate::config::DoctorArgs;

pub use docker::DockerRuntime;

#[async_trait]
pub trait WorkerRuntime: Send + Sync {
    fn descriptor(&self) -> RuntimeDescriptor;
    fn capabilities(&self) -> WorkerCapabilities;
    fn platform(&self) -> Platform;

    async fn probe(&self) -> Result<(), RuntimeError>;
    async fn capacity(&self) -> Result<WorkerCapacity, RuntimeError>;
    async fn usage(&self) -> Result<ResourceUsage, RuntimeError>;
    async fn inventory(&self) -> Result<Vec<InventoryItem>, RuntimeError>;
    async fn ensure_workload(
        &self,
        command: EnsureWorkload,
    ) -> Result<WorkloadStatus, RuntimeError>;
    async fn ensure_absent(&self, command: EnsureAbsent) -> Result<WorkloadStatus, RuntimeError>;
    async fn write_flag(&self, command: WriteFlag) -> Result<WorkloadStatus, RuntimeError>;
    async fn open_tcp(&self, request: &TcpProxyRequest) -> Result<TcpStream, RuntimeError>;
    async fn open_exec(&self, _request: &InteractiveExecRequest) -> Result<(), RuntimeError> {
        Err(RuntimeError::unsupported(
            "interactive container exec is not implemented by this runtime",
        ))
    }
}

pub type SharedRuntime = Arc<dyn WorkerRuntime>;

#[derive(Clone, Copy, Debug)]
pub struct RuntimeOptions {
    pub writable_layer_bytes: u64,
    pub minimum_free_bytes: u64,
    pub allow_unbounded_storage: bool,
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct RuntimeError {
    pub code: CommandErrorCode,
    pub message: String,
    pub failed_replicas: Vec<String>,
}

impl RuntimeError {
    pub fn new(code: CommandErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: bounded(message.into()),
            failed_replicas: Vec::new(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(CommandErrorCode::Unsupported, message)
    }

    pub fn with_failed_replicas(mut self, failed_replicas: Vec<String>) -> Self {
        self.failed_replicas = failed_replicas;
        self
    }

    pub fn as_command_error(&self) -> CommandError {
        CommandError {
            code: self.code,
            message: self.message.clone(),
            failed_replicas: self.failed_replicas.clone(),
        }
    }
}

fn bounded(mut message: String) -> String {
    const MAX_ERROR_BYTES: usize = 2048;
    if message.len() <= MAX_ERROR_BYTES {
        return message;
    }
    let mut end = MAX_ERROR_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message
}

pub async fn runtime_for(
    worker_id: Uuid,
    endpoint: Option<&str>,
    state_dir: &std::path::Path,
    options: RuntimeOptions,
) -> Result<SharedRuntime, RuntimeError> {
    Ok(Arc::new(
        DockerRuntime::connect(worker_id, endpoint, state_dir, options).await?,
    ))
}

pub async fn doctor(arguments: DoctorArgs) -> Result<(), RuntimeError> {
    docker::preflight(
        arguments.docker_endpoint.as_deref(),
        arguments.allow_unbounded_storage,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::bounded;

    #[test]
    fn truncates_long_unicode_errors_on_a_character_boundary() {
        let message = format!("{}é", "x".repeat(2047));
        let bounded = bounded(message);
        assert_eq!(bounded.len(), 2047);
        assert!(bounded.chars().all(|character| character == 'x'));
    }
}
