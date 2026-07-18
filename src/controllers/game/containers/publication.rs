use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

/// Refresh a live KotH target's managed-container lease while its caller holds
/// the challenge's shared-container lock. A missing bookkeeping row returns
/// `false` so provisioning can revoke the stale endpoint and recreate it.
pub(crate) async fn refresh_shared_container_lease_locked(
    st: &SharedState,
    backend_id: &str,
) -> AppResult<bool> {
    let stop_at = chrono::Utc::now() + chrono::Duration::hours(super::CONTAINER_LIFETIME_HOURS);
    let result = sqlx::query(
        r#"UPDATE "Containers" SET expect_stop_at = $2
            WHERE container_id = $1"#,
    )
    .bind(backend_id)
    .bind(stop_at)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result.rows_affected() != 0)
}

pub(super) async fn revoke_published_team_container(
    st: &SharedState,
    backend_id: &str,
    container_id: uuid::Uuid,
    instance_id: i32,
    created_instance_id: Option<i32>,
    created_flag_id: Option<i32>,
) -> AppResult<()> {
    crate::services::traffic::stop_container_capture(st, backend_id).await?;
    if let Err(error) = st.containers.destroy(backend_id).await {
        tracing::warn!(%backend_id, %error, "stale published container destroy failed");
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
        .bind(container_id)
        .bind(backend_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if created_instance_id == Some(instance_id) {
        sqlx::query(
            r#"DELETE FROM "GameInstances"
                WHERE id = $1 AND (container_id = $2 OR container_id IS NULL)"#,
        )
        .bind(instance_id)
        .bind(container_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    } else {
        sqlx::query(
            r#"UPDATE "GameInstances"
                  SET container_id = NULL,
                      is_loaded = FALSE,
                      flag_id = CASE WHEN flag_id = $3 THEN NULL ELSE flag_id END,
                      last_container_operation = CURRENT_TIMESTAMP
                WHERE id = $1 AND container_id = $2"#,
        )
        .bind(instance_id)
        .bind(container_id)
        .bind(created_flag_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(flag_id) = created_flag_id {
        sqlx::query(
            r#"DELETE FROM "FlagContexts" flag
                WHERE flag.id = $1
                  AND NOT EXISTS (
                      SELECT 1 FROM "GameInstances" instance WHERE instance.flag_id = flag.id
                  )"#,
        )
        .bind(flag_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn revoke_published_shared_container(
    st: &SharedState,
    challenge_id: i32,
    container_id: uuid::Uuid,
    backend_id: &str,
) -> AppResult<()> {
    if let Err(error) =
        crate::services::ad_vpn::deactivate_backend_endpoint(&st.db, backend_id).await
    {
        tracing::warn!(%backend_id, %error, "stale shared endpoint revocation failed");
    }
    crate::services::traffic::stop_container_capture(st, backend_id).await?;
    if let Err(error) = st.containers.destroy(backend_id).await {
        tracing::warn!(%backend_id, %error, "stale published shared container destroy failed");
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET shared_container_id = NULL
            WHERE id = $1 AND shared_container_id = $2"#,
    )
    .bind(challenge_id)
    .bind(container_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
        .bind(container_id)
        .bind(backend_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}
