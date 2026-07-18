//! Safe rollover of an interrupted previous crown cycle.

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

use super::lifecycle::{drive_one_cycle, OfficialConfig};

fn completed_phases(current: &str) -> &'static [&'static str] {
    match current {
        "SnapshotPending" => &["FinalizePending"],
        "DestroyPending" => &["FinalizePending", "SnapshotPending"],
        "CreatePending" => &["FinalizePending", "SnapshotPending", "DestroyPending"],
        "PublishPending" => &[
            "FinalizePending",
            "SnapshotPending",
            "DestroyPending",
            "CreatePending",
        ],
        "CapabilityPending" => &[
            "FinalizePending",
            "SnapshotPending",
            "DestroyPending",
            "CreatePending",
            "PublishPending",
        ],
        "ReadinessPending" => &[
            "FinalizePending",
            "SnapshotPending",
            "DestroyPending",
            "CreatePending",
            "PublishPending",
            "CapabilityPending",
        ],
        "FirewallPending" => &[
            "FinalizePending",
            "SnapshotPending",
            "DestroyPending",
            "CreatePending",
            "PublishPending",
            "CapabilityPending",
            "ReadinessPending",
        ],
        "Active" | "Completed" => &[
            "FinalizePending",
            "SnapshotPending",
            "DestroyPending",
            "CreatePending",
            "PublishPending",
            "CapabilityPending",
            "ReadinessPending",
            "FirewallPending",
        ],
        _ => &[],
    }
}

/// A crash can commit a phase CAS immediately before its receipt insert. Fill
/// only receipts proven complete by the durable successor phase; normal detailed
/// receipts win the same unique key before this recovery path runs.
pub(super) async fn backfill_missing_receipts(
    st: &SharedState,
    cycle_id: i64,
    current_phase: &str,
) -> AppResult<()> {
    let phases = completed_phases(current_phase);
    if phases.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r#"INSERT INTO "KothCycleAuditReceipts"
             (cycle_id, phase, attempt, receipt, filesystem_diff)
           SELECT cycle.id, completed.phase,
                  cycle.reset_attempt,
                  jsonb_build_object(
                    'recovered', TRUE, 'durablePhase', cycle.phase,
                    'readinessAttempt', cycle.readiness_attempt,
                    'oldContainerId', cycle.old_container_id,
                    'replacementContainerId', cycle.replacement_container_id
                  ),
                  CASE WHEN completed.phase = 'SnapshotPending'
                    THEN cycle.filesystem_diff ELSE NULL END
             FROM "KothCrownCycles" cycle
             CROSS JOIN UNNEST($2::text[]) AS completed(phase)
            WHERE cycle.id = $1
              AND (cycle.reset_attempt = 1
                   OR completed.phase NOT IN ('FinalizePending', 'SnapshotPending'))
           ON CONFLICT (cycle_id, phase, attempt) DO NOTHING"#,
    )
    .bind(cycle_id)
    .bind(phases)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Before declaring cycle N, finish recovery of cycle N-1 under the same hill
/// advisory lock. Otherwise a crash after runtime creation but before identity
/// persistence can leave an adopted previous container orphaned while the next
/// cycle creates a second backend.
pub(super) async fn resume_previous_cycle(
    st: &SharedState,
    config: &OfficialConfig,
    game_id: i32,
    challenge_id: i32,
    cycle_number: i32,
    ad_round_id: i32,
    round_number: i32,
) -> AppResult<()> {
    if cycle_number <= 1 {
        return Ok(());
    }
    let previous: Option<(i64, String)> = sqlx::query_as(
        r#"SELECT id, phase FROM "KothCrownCycles"
            WHERE game_id = $1 AND challenge_id = $2
              AND cycle_number = $3"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(cycle_number - 1)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((cycle_id, phase)) = previous else {
        return Ok(());
    };
    if matches!(phase.as_str(), "Active" | "Completed" | "Ended") {
        return Ok(());
    }
    drive_one_cycle(st, config, cycle_id, ad_round_id, round_number).await?;
    let phase: String = sqlx::query_scalar(r#"SELECT phase FROM "KothCrownCycles" WHERE id = $1"#)
        .bind(cycle_id)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if matches!(phase.as_str(), "Active" | "Completed" | "Ended") {
        Ok(())
    } else {
        Err(AppError::conflict(format!(
            "previous KotH crown cycle remains in {phase}; rollover is paused"
        )))
    }
}

pub(super) async fn refresh_old_container(st: &SharedState, cycle_id: i64) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "KothCrownCycles" cycle
              SET old_container_id = target.container_id,
                  updated_at = clock_timestamp()
             FROM "KothTargets" target
            WHERE cycle.id = $1 AND cycle.phase = 'FinalizePending'
              AND target.game_id = cycle.game_id
              AND target.challenge_id = cycle.challenge_id"#,
    )
    .bind(cycle_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::completed_phases;

    #[test]
    fn receipt_recovery_never_claims_the_current_pending_phase() {
        assert_eq!(completed_phases("FinalizePending"), &[] as &[&str]);
        assert_eq!(
            completed_phases("CreatePending"),
            &["FinalizePending", "SnapshotPending", "DestroyPending"]
        );
        assert!(completed_phases("Active").contains(&"FirewallPending"));
    }
}
