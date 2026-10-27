//! Service-owned inspector lookup and fenced teardown.
use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ExistingInspector {
    pub(crate) id: Uuid,
    pub(crate) backend_id: String,
    pub(crate) image: String,
}

pub(crate) async fn load_existing(
    st: &SharedState,
    service_id: i32,
) -> AppResult<Option<ExistingInspector>> {
    sqlx::query_as::<_, ExistingInspector>(
        r#"SELECT id, container_id AS backend_id, image
             FROM "Containers"
            WHERE ad_team_service_id = $1"#,
    )
    .bind(service_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub(crate) async fn destroy_exact(
    st: &SharedState,
    service_id: i32,
    inspector: &ExistingInspector,
) -> AppResult<()> {
    st.containers.destroy(&inspector.backend_id).await?;
    sqlx::query(
        r#"DELETE FROM "Containers"
            WHERE id = $1 AND container_id = $2 AND ad_team_service_id = $3"#,
    )
    .bind(inspector.id)
    .bind(&inspector.backend_id)
    .bind(service_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(crate) async fn destroy_service_inspector_locked(
    st: &SharedState,
    service_id: i32,
) -> AppResult<()> {
    if let Some(existing) = load_existing(st, service_id).await? {
        destroy_exact(st, service_id, &existing).await?;
    }
    Ok(())
}

pub(crate) async fn destroy_service_inspector(st: &SharedState, service_id: i32) -> AppResult<()> {
    let lock_key = format!("ad-inspector:{service_id}");
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    let result = destroy_service_inspector_locked(st, service_id).await;
    let released = distributed.release().await.map_err(AppError::from);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
