//! Exact backend target resolution shared by player and administrator tunnels.

use sea_orm::EntityTrait;
use uuid::Uuid;

use super::authorization::GameProxyTargetIdentity;
use super::LEGACY_EXERCISE_OWNER_SQL;
use crate::app_state::SharedState;
use crate::models::data::container;
use crate::services::worker::{parse_worker_handle, WorkerHandle};

/// Resolve the reachable `ip:port` for an admin test container. The caller has
/// already established a live admin identity; only detached throwaway test
/// containers are eligible.
pub(super) async fn resolve_noinstance_target(st: &SharedState, id: Uuid) -> Option<ProxyTarget> {
    let container = container::Entity::find_by_id(id).one(&st.db).await.ok()??;
    if !container.is_proxy
        || container.game_instance_id.is_some()
        || container.exercise_instance_id.is_some()
    {
        return None;
    }
    let legacy_exercise_owner = sqlx::query_scalar::<_, bool>(LEGACY_EXERCISE_OWNER_SQL)
        .bind(container.id)
        .fetch_one(st.pg())
        .await
        .ok()?;
    if legacy_exercise_owner {
        return None;
    }
    proxy_target(&container)
}

#[derive(Clone)]
pub(super) enum ProxyTarget {
    Tcp(String),
    Worker(WorkerHandle),
}

pub(super) fn proxy_target(container: &container::Model) -> Option<ProxyTarget> {
    if let Some(handle) = parse_worker_handle(&container.container_id) {
        return Some(ProxyTarget::Worker(handle));
    }
    target_endpoint(container).map(ProxyTarget::Tcp)
}

pub(super) fn game_proxy_target_identity(
    container: &container::Model,
    game_instance_id: Option<i32>,
) -> GameProxyTargetIdentity {
    GameProxyTargetIdentity {
        container_id: container.id,
        runtime_id: container.container_id.clone(),
        ip: container.ip.clone(),
        port: container.port,
        game_instance_id,
    }
}

/// Build the `ip:port` the proxy should dial. Container persistence stores the
/// host-reachable published address in these columns for the Docker backend.
pub(super) fn target_endpoint(container: &container::Model) -> Option<String> {
    if container.ip.trim().is_empty() || container.port <= 0 {
        return None;
    }
    Some(format!("{}:{}", container.ip, container.port))
}
