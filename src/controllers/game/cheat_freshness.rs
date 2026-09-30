//! Freshness of the monitor cheat report: the persisted evaluator watermark
//! and the work that can still change it.

use super::*;

#[derive(Debug, Default, sqlx::FromRow)]
pub(super) struct ReconciliationReportState {
    pub evidence_closed_at: Option<DateTime<Utc>>,
    pub last_reconciled_at: Option<DateTime<Utc>>,
    pub sealed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub pending_jobs: i64,
    pub oldest_pending_at: Option<DateTime<Utc>>,
    /// Captured evidence the reconciler has not applied yet. A quiet game has
    /// none, so an old reconciliation time alone does not mean stale.
    pub reconciliation_pending: bool,
}

/// Load the persisted evaluator watermark and only the jobs that can still
/// affect this game's competitive snapshot. Response generation is not an
/// evaluation, so the monitor must never use its wall clock as freshness.
pub(super) async fn load_reconciliation_report_state(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<ReconciliationReportState> {
    sqlx::query_as::<_, ReconciliationReportState>(
        r#"SELECT reconciliation.evidence_closed_at_utc AS evidence_closed_at,
                  reconciliation.last_reconciled_at_utc AS last_reconciled_at,
                  reconciliation.sealed_at_utc AS sealed_at,
                  COALESCE(reconciliation.last_error, pending.pending_error) AS last_error,
                  COALESCE(pending.pending_jobs, 0)::bigint AS pending_jobs,
                  pending.oldest_pending_at,
                  COALESCE(queue.desired_generation > queue.applied_generation, FALSE)
                  OR EXISTS (
                      SELECT 1 FROM "AntiCheatReconciliationSources" source
                       WHERE source.game_id = game.id
                         AND source.dirty_version > source.applied_version
                  ) AS reconciliation_pending
             FROM "Games" game
             LEFT JOIN "AntiCheatReconciliationQueue" queue ON queue.game_id = game.id
             LEFT JOIN "SuspicionReconciliationState" reconciliation
               ON reconciliation.game_id = game.id
             LEFT JOIN LATERAL (
               SELECT COUNT(*)::bigint AS pending_jobs,
                      MIN(job.observed_at_utc) AS oldest_pending_at,
                      (ARRAY_AGG(job.last_error ORDER BY job.observed_at_utc, job.id)
                         FILTER (WHERE job.last_error IS NOT NULL))[1] AS pending_error
                 FROM "SuspicionEvaluationOutbox" job
                WHERE job.game_id = game.id
                  AND job.completed_at_utc IS NULL
                  AND job.observed_at_utc >= game.start_time_utc
                  AND job.observed_at_utc < game.end_time_utc
             ) pending ON TRUE
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
    .map(|state| state.unwrap_or_default())
}
