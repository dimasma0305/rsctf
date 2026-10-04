use crate::services::container::{ContainerLiveness, ContainerManager};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ManagedHillLiveness {
    Running,
    Dead(String),
    Unknown(String),
}

pub(super) async fn inspect_liveness(
    containers: &dyn ContainerManager,
    container_id: &str,
) -> ManagedHillLiveness {
    match containers.inspect_liveness(container_id).await {
        Ok(ContainerLiveness::Running) => ManagedHillLiveness::Running,
        Ok(ContainerLiveness::Stopped) => ManagedHillLiveness::Dead(container_id.to_string()),
        Ok(ContainerLiveness::Unknown) => {
            ManagedHillLiveness::Unknown("backend is in a transitional state".to_string())
        }
        Err(error) => ManagedHillLiveness::Unknown(error.to_string()),
    }
}
