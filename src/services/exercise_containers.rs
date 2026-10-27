//! Exercise runtime ownership and exact-publication teardown.

use crate::utils::error::{AppError, AppResult};
pub(crate) mod operations;

pub(crate) async fn clear_exercise_container_owner(
    pool: &sqlx::PgPool,
    instance_id: Option<i32>,
    container_id: uuid::Uuid,
    backend_id: Option<&str>,
    created_flag_id: Option<i32>,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(instance_id) = instance_id {
        sqlx::query(
            r#"UPDATE "ExerciseInstances"
                  SET container_id = NULL,
                      is_loaded = FALSE,
                      flag_id = CASE WHEN flag_id = $3 THEN NULL ELSE flag_id END
                WHERE id = $1 AND container_id = $2"#,
        )
        .bind(instance_id)
        .bind(container_id)
        .bind(created_flag_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(backend_id) = backend_id {
        sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1 AND container_id = $2"#)
            .bind(container_id)
            .bind(backend_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if let Some(flag_id) = created_flag_id {
        sqlx::query(
            r#"DELETE FROM "FlagContexts" flag
                WHERE flag.id = $1
                  AND NOT EXISTS (
                      SELECT 1 FROM "ExerciseInstances" instance
                       WHERE instance.flag_id = flag.id
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

pub(crate) async fn destroy_owned_exercise_container_with<F>(
    pool: &sqlx::PgPool,
    instance_id: Option<i32>,
    container_id: uuid::Uuid,
    backend_id: &str,
    created_flag_id: Option<i32>,
    destroy: F,
) -> AppResult<()>
where
    F: std::future::Future<Output = AppResult<()>>,
{
    // Await destruction before opening the cleanup transaction. A failed
    // backend call therefore leaves every durable owner available for retry.
    destroy.await?;
    clear_exercise_container_owner(
        pool,
        instance_id,
        container_id,
        Some(backend_id),
        created_flag_id,
    )
    .await
}
