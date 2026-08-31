//! Exact Docker operation lookup used by crash recovery.

use bollard::container::ListContainersOptions;

use super::{
    managed_container_filters, AppError, AppResult, DockerContainerManager, MANAGED_LABEL,
    OPERATION_LABEL, SCOPE_LABEL,
};

pub(super) async fn find_operation_runtime(
    manager: &DockerContainerManager,
    operation_id: &str,
) -> AppResult<Option<String>> {
    if operation_id.trim().is_empty() || operation_id.len() > 256 {
        return Err(AppError::bad_request(
            "invalid container operation identity",
        ));
    }
    let docker = manager.client()?;
    let mut filters = managed_container_filters(&manager.scope);
    filters.insert(
        "label".to_string(),
        vec![
            format!("{MANAGED_LABEL}={}", manager.scope),
            format!("{SCOPE_LABEL}={}", manager.scope),
            format!("{OPERATION_LABEL}={operation_id}"),
        ],
    );
    let rows = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        }))
        .await
        .map_err(|error| {
            AppError::internal(format!(
                "failed to discover container operation runtime: {error}"
            ))
        })?;
    if rows.len() > 1 {
        return Err(AppError::conflict(
            "multiple Docker containers claim one operation identity",
        ));
    }
    Ok(rows.into_iter().next().and_then(|row| row.id))
}
