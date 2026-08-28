use super::*;

pub(crate) async fn seal_reconciled_game_for_test(
    pool: &sqlx::PgPool,
    game_id: i32,
    finalize_grace_seconds: u64,
) -> AppResult<bool> {
    if !close_competitive_evidence_window(pool, game_id, finalize_grace_seconds).await? {
        return Ok(false);
    }
    if defer_final_for_incomplete_jobs(pool, game_id).await? {
        return Ok(false);
    }
    sqlx::query(
        r#"UPDATE "SuspicionReconciliationState"
              SET evidence_closed_at_utc = COALESCE(evidence_closed_at_utc, clock_timestamp()),
                  sealed_at_utc = COALESCE(sealed_at_utc, clock_timestamp()),
                  last_reconciled_at_utc = clock_timestamp(), last_error = NULL,
                  completed_generation = dirty_generation, dirty_mask = 0,
                  attempts = attempts + 1
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(true)
}
