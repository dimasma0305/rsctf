use super::*;

pub(crate) async fn claim_repo_scan(
    pool: &sqlx::PgPool,
    specific_id: Option<i32>,
    limit: i64,
) -> AppResult<Vec<(i32, Uuid)>> {
    let token = Uuid::new_v4();
    let rows = sqlx::query_scalar::<_, i32>(
        r#"WITH due AS (
               SELECT id FROM "RepoBindings"
                WHERE ($2::INTEGER IS NOT NULL AND id = $2
                       OR $2::INTEGER IS NULL AND status = $1
                          AND (next_scan_utc IS NULL OR next_scan_utc <= clock_timestamp()))
                  AND (scan_lease_until IS NULL OR scan_lease_until <= clock_timestamp())
                ORDER BY next_scan_utc NULLS FIRST, id
                FOR UPDATE SKIP LOCKED
                LIMIT $3
           )
           UPDATE "RepoBindings" binding
              SET scan_lease_token = $4,
                  scan_lease_until = clock_timestamp() + make_interval(secs => $5),
                  scan_started_at_utc = clock_timestamp()
             FROM due WHERE binding.id = due.id
           RETURNING binding.id"#,
    )
    .bind(RepoWatchStatus::Active as i16)
    .bind(specific_id)
    .bind(limit.clamp(1, 4))
    .bind(token)
    .bind(SCAN_LEASE_SECONDS)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows.into_iter().map(|id| (id, token)).collect())
}

pub(crate) async fn run_claimed_repo_scan(
    st: &SharedState,
    id: i32,
    lease_token: Uuid,
    allow_paused: bool,
) -> AppResult<RepoBindingScanResultModel> {
    let result = run_repo_scan(st, id, allow_paused).await;
    let successful = result.as_ref().is_ok_and(|result| result.failures == 0);
    let error = result.as_ref().err().map(ToString::to_string);
    finish_repo_scan_lease(st.pg(), id, lease_token, successful, error.as_deref()).await?;
    result
}

async fn finish_repo_scan_lease(
    pool: &sqlx::PgPool,
    id: i32,
    lease_token: Uuid,
    successful: bool,
    error: Option<&str>,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET scan_lease_token = NULL, scan_lease_until = NULL,
                  scan_started_at_utc = NULL,
                  consecutive_scan_failures = CASE WHEN $3 THEN 0
                      ELSE consecutive_scan_failures + 1 END,
                  next_scan_utc = CASE WHEN $3 THEN next_scan_utc ELSE
                      clock_timestamp() + make_interval(secs =>
                          LEAST(3600, 30 * (1 << LEAST(consecutive_scan_failures, 6)))
                          + (id % 17)) END,
                  last_scan_message = COALESCE($4, last_scan_message)
            WHERE id = $1 AND scan_lease_token = $2"#,
    )
    .bind(id)
    .bind(lease_token)
    .bind(successful)
    .bind(error.map(|message| message.chars().take(2_000).collect::<String>()))
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() == 0 {
        return Err(AppError::conflict("Repository scan lease was lost"));
    }
    Ok(())
}
