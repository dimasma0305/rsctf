//! Audited reopening of a competition's reconciliation window.
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

#[derive(Default, sqlx::FromRow)]
struct EvidenceBoundary {
    evidence_closed_at_utc: Option<DateTime<Utc>>,
    sealed_at_utc: Option<DateTime<Utc>>,
}

/// The editor owns Games FOR UPDATE. Claims take Games FOR SHARE before their
/// queue lease, so a schedule cannot change beneath a running detector pass.
/// Evidence rows and completed outbox receipts are deliberately never rewritten.
pub(crate) async fn record_schedule_change(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    actor: Uuid,
    operation: Uuid,
    revision: i64,
    previous: (DateTime<Utc>, DateTime<Utc>),
    requested: (DateTime<Utc>, DateTime<Utc>),
) -> AppResult<()> {
    let boundary = sqlx::query_as::<_, EvidenceBoundary>(
        r#"SELECT evidence_closed_at_utc, sealed_at_utc
             FROM "SuspicionReconciliationState" WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?;
    let leased = sqlx::query_scalar::<_, Option<bool>>(
        r#"SELECT lease_expires_at_utc > clock_timestamp()
             FROM "AntiCheatReconciliationQueue" WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?;
    if leased.flatten().unwrap_or(false) {
        return Err(AppError::conflict(
            "Anti-cheat reconciliation is finishing. Retry the schedule change shortly.",
        ));
    }
    let boundary = boundary.unwrap_or_default();
    sqlx::query(
        r#"INSERT INTO "GameScheduleChanges"
             (operation_id, game_id, actor_user_id, configuration_revision,
              previous_start, previous_end, requested_start, requested_end,
              previous_evidence_closed_at, previous_sealed_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)"#,
    )
    .bind(operation)
    .bind(game_id)
    .bind(actor)
    .bind(revision)
    .bind(previous.0)
    .bind(previous.1)
    .bind(requested.0)
    .bind(requested.1)
    .bind(boundary.evidence_closed_at_utc)
    .bind(boundary.sealed_at_utc)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;

    sqlx::query(
        r#"UPDATE "SuspicionReconciliationState"
              SET evidence_closed_at_utc = NULL, sealed_at_utc = NULL,
                  last_error = NULL
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET desired_generation = desired_generation + 1,
                  final_requested_at_utc = NULL, final_applied_at_utc = NULL,
                  lease_token = NULL, lease_expires_at_utc = NULL,
                  available_at_utc = clock_timestamp(), last_error = NULL,
                  updated_at_utc = clock_timestamp()
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    // Late practice evidence was acknowledged while sealed. Replay the stored
    // source cursors in bounded batches, retaining their original timestamps.
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationSources"
              SET applied_version = 0, applied_at_utc = NULL
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(connection)
    .await
    .map_err(database_error)?;
    Ok(())
}

/// Completed practice submissions can enter an extended competition window.
/// Re-evaluate only those historical submissions; normal outbox work is not
/// duplicated and its immutable completion/version is left untouched.
pub(super) async fn replay_newly_competitive_submissions(
    state: &crate::app_state::SharedState,
    game_id: i32,
    cursor: super::reconciliation::SourceCursor,
) -> AppResult<()> {
    let submissions = load_replay_submissions(state.pg(), game_id, cursor).await?;
    for submission_id in submissions {
        super::evaluate_submission_by_id(&state.db, submission_id).await?;
    }
    Ok(())
}

pub(super) async fn load_replay_submissions(
    pool: &sqlx::PgPool,
    game_id: i32,
    cursor: super::reconciliation::SourceCursor,
) -> AppResult<Vec<i32>> {
    sqlx::query_scalar(
        r#"SELECT job.source_id FROM "SuspicionEvaluationOutbox" job
            WHERE job.game_id = $1 AND job.source_kind = 0
              AND job.completed_at_utc IS NOT NULL
              AND job.reconciliation_version > $2
              AND job.reconciliation_version <= $3
              AND EXISTS (
                  SELECT 1 FROM "GameScheduleChanges" change
                   WHERE change.game_id = job.game_id
                     AND job.observed_at_utc >= change.previous_end
                     AND job.observed_at_utc < change.requested_end
                     AND job.observed_at_utc <= change.changed_at
              )
            ORDER BY job.reconciliation_version LIMIT $4"#,
    )
    .bind(game_id)
    .bind(cursor.after)
    .bind(cursor.through)
    .bind(super::reconciliation::SOURCE_BATCH)
    .fetch_all(pool)
    .await
    .map_err(database_error)
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}
