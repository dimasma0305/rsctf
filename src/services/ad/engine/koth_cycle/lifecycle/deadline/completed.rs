//! Fenced receipts and pointer removal after completed deadline teardown.
use super::{clear_destroyed_deadline_target, persist_deadline_access_revocation};
use crate::utils::error::{AppError, AppResult};
use serde_json::json;

pub(super) struct CompletedCleanup<'a> {
    pub(super) cycle_id: i64,
    pub(super) game_id: i32,
    pub(super) challenge_id: i32,
    pub(super) reset_attempt: i32,
    pub(super) round_number: i32,
    pub(super) container_ids: &'a [String],
}

pub(super) async fn persist_completed_cleanup(
    connection: &mut sqlx::PgConnection,
    cleanup: CompletedCleanup<'_>,
) -> AppResult<()> {
    persist_deadline_access_revocation(connection, cleanup.game_id, cleanup.challenge_id).await?;
    clear_destroyed_deadline_target(
        connection,
        cleanup.game_id,
        cleanup.challenge_id,
        cleanup.container_ids,
    )
    .await?;
    sqlx::query(
        r#"WITH removed AS (
             DELETE FROM "Containers" WHERE container_id = ANY($2) RETURNING id
           )
           UPDATE "GameChallenges"
              SET shared_container_id = NULL
            WHERE id = $1 AND shared_container_id IN (SELECT id FROM removed)"#,
    )
    .bind(cleanup.challenge_id)
    .bind(cleanup.container_ids)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let receipt = json!({
        "reason": "eventDeadline",
        "endedRound": cleanup.round_number,
        "destroyedContainerIds": cleanup.container_ids,
    });
    sqlx::query(
        r#"WITH cleared_error AS (
             UPDATE "KothCrownCycles"
                SET last_error = NULL, updated_at = clock_timestamp()
              WHERE id = $1 AND phase = 'Completed' AND last_error IS NOT NULL
           )
           INSERT INTO "KothCycleAuditReceipts"
             (cycle_id, phase, attempt, receipt, filesystem_diff)
           SELECT id, 'DeadlineCleanup', $3, $2, NULL
             FROM "KothCrownCycles"
            WHERE id = $1 AND phase = 'Completed'
           ON CONFLICT (cycle_id, phase, attempt) DO NOTHING"#,
    )
    .bind(cleanup.cycle_id)
    .bind(receipt)
    .bind(cleanup.reset_attempt)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
