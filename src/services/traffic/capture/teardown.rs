use super::*;

use std::future::Future;

pub(super) const CLEAR_INACTIVE_BACKEND_SQL: &str = r#"UPDATE "AdTeamServices"
       SET container_id = NULL
     WHERE container_id = $1
       AND NULLIF(BTRIM(host), '') IS NULL
       AND port = 0"#;

/// Fence a teardown against the singleton owner without releasing its durable
/// backend identity. Callers that destroy the runtime use this so a backend
/// failure remains exactly retryable.
async fn fence_container_capture_stop(state: &SharedState, container_id: &str) -> AppResult<()> {
    let identity = capture_identity_state(state.pg(), container_id).await?;
    if !identity.has_identity {
        return Ok(());
    }
    if identity.is_desired {
        return Err(AppError::internal(
            "traffic capture teardown requested before container deactivation",
        ));
    }
    let generation = request_reconciliation(state, container_id, ReconcileAction::Stop).await?;
    wait_for_request_result(state.pg(), generation).await
}

async fn clear_inactive_backend_identity<'e, E>(executor: E, container_id: &str) -> AppResult<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query(CLEAR_INACTIVE_BACKEND_SQL)
        .bind(container_id)
        .execute(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Release the retained database pointer only after the backend operation has
/// succeeded. Keeping this boundary injectable gives every hard-delete caller
/// the same regression-tested retry contract.
pub(super) async fn destroy_inactive_backend_with<'e, E, F>(
    executor: E,
    container_id: &str,
    destroy: F,
) -> AppResult<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    F: Future<Output = AppResult<()>>,
{
    destroy.await?;
    clear_inactive_backend_identity(executor, container_id).await
}

/// Fence capture, destroy the backend, then release only the exact inactive
/// A&D identity. A failed destroy or acknowledgement retains the pointer.
pub(crate) async fn destroy_container_after_capture_fence(
    state: &SharedState,
    container_id: &str,
) -> AppResult<()> {
    fence_container_capture_stop(state, container_id).await?;
    destroy_inactive_backend_with(
        state.pg(),
        container_id,
        state.containers.destroy(container_id),
    )
    .await
}

/// Fence capture and release an inactive identity for callers that retain a
/// separate durable retry owner, such as a `Containers` row.
pub async fn stop_container_capture(state: &SharedState, container_id: &str) -> AppResult<()> {
    fence_container_capture_stop(state, container_id).await?;
    clear_inactive_backend_identity(state.pg(), container_id).await
}
