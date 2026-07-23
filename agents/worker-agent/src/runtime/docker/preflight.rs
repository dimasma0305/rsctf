use rsctf_worker_protocol::{CommandErrorCode, OperatingSystem};

use crate::runtime::RuntimeError;

use super::support::{connect_docker, daemon_platform, docker_error, storage_quota_supported};

pub(in crate::runtime) async fn run(
    endpoint: Option<&str>,
    allow_unbounded_storage: bool,
) -> Result<(), RuntimeError> {
    let endpoint = endpoint.unwrap_or("local");
    let (docker, _) = connect_docker(endpoint)?;
    let docker = docker
        .negotiate_version()
        .await
        .map_err(|error| docker_error("negotiate Docker API version", error))?;
    let info = docker
        .info()
        .await
        .map_err(|error| docker_error("read Docker daemon information", error))?;
    let platform = daemon_platform(&info)?;
    if cfg!(windows) && platform.operating_system == OperatingSystem::Linux {
        return Err(RuntimeError::unsupported(
            "a Windows agent cannot safely proxy a Linux Docker daemon's private bridge addresses; switch Docker to Windows-container mode",
        ));
    }
    if !storage_quota_supported(&info) && !allow_unbounded_storage {
        return Err(RuntimeError::unsupported(
            "Docker storage driver cannot enforce per-container writable-layer quotas; configure overlay2 on XFS with project quotas or native Windows windowsfilter",
        ));
    }
    if info
        .docker_root_dir
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(RuntimeError::new(
            CommandErrorCode::RuntimeUnavailable,
            "Docker daemon did not report its data root",
        ));
    }
    tracing::info!(
        operating_system = ?platform.operating_system,
        architecture = platform.architecture,
        storage_driver = info.driver.as_deref().unwrap_or("unknown"),
        "worker runtime preflight passed"
    );
    Ok(())
}
