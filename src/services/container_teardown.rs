//! Endpoint/capture fencing and exact test-container removal.
use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use uuid::Uuid;

pub(crate) async fn revoke_and_destroy_backend(
    st: &SharedState,
    backend_id: &str,
) -> AppResult<()> {
    let game_ids = crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
        &st.db, backend_id,
    )
    .await?;
    for game_id in game_ids {
        crate::services::scoreboard_invalidation::invalidate_live_hill_snapshot(st, game_id).await;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    crate::services::traffic::destroy_container_after_capture_fence(st, backend_id).await
}

pub(crate) async fn destroy_test_container_with<F>(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    container_id: Uuid,
    backend_id: &str,
    destroy: F,
) -> AppResult<()>
where
    F: std::future::Future<Output = AppResult<()>>,
{
    destroy.await?;
    sqlx::query(
        r#"WITH owner AS (
             SELECT id FROM "GameChallenges"
              WHERE id = $1 AND test_container_id = $2
           ), removed AS (
             DELETE FROM "Containers" container USING owner
              WHERE container.id = $2 AND container.container_id = $3
              RETURNING container.id
           )
           UPDATE "GameChallenges"
              SET test_container_id = NULL
            WHERE id = $1 AND test_container_id IN (SELECT id FROM removed)"#,
    )
    .bind(challenge_id)
    .bind(container_id)
    .bind(backend_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
