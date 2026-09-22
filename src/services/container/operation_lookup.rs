//! Exact Docker operation lookup used by crash recovery, plus the labeled
//! inventory that capacity reconciliation shares with the orphan sweep.

use bollard::container::ListContainersOptions;

use super::capacity::ManagedRuntime;
use super::{
    managed_container_filters, AppError, AppResult, DockerContainerManager, MANAGED_LABEL,
    OPERATION_LABEL, SCOPE_LABEL,
};

impl DockerContainerManager {
    /// Every container of this installation scope with its operation label.
    pub(super) async fn managed_inventory(&self) -> AppResult<Vec<ManagedRuntime>> {
        let docker = self.client()?;
        let rows = docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: managed_container_filters(&self.scope),
                ..Default::default()
            }))
            .await
            .map_err(|error| {
                AppError::internal(format!("docker list_containers failed: {error}"))
            })?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let backend_id = row.id?;
                let operation_id = row
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(OPERATION_LABEL))
                    .cloned();
                Some(ManagedRuntime {
                    backend_id,
                    operation_id,
                })
            })
            .collect())
    }
}

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
