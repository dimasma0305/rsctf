//! Event-deadline cleanup for in-flight crown-cycle transitions.

use serde_json::{json, Value};

use crate::app_state::SharedState;
use crate::services::container::ContainerBackendKind;
use crate::utils::error::{AppError, AppResult};

use super::super::CrownPhase;
use super::data::{CycleRow, OfficialConfig};

mod access;

use access::persist_deadline_access_revocation;

struct CompletedCleanup<'a> {
    cycle_id: i64,
    game_id: i32,
    challenge_id: i32,
    reset_attempt: i32,
    round_number: i32,
    container_ids: &'a [String],
}

#[derive(Debug)]
struct CleanupRuntimeState {
    container_ids: Vec<String>,
    snapshot_container_id: Option<String>,
}

fn push_runtime_id(container_ids: &mut Vec<String>, container_id: Option<&String>) {
    if let Some(container_id) = container_id.filter(|container_id| !container_id.is_empty()) {
        if !container_ids.contains(container_id) {
            container_ids.push(container_id.clone());
        }
    }
}

async fn load_cleanup_runtime_state(
    st: &SharedState,
    cycle: &CycleRow,
) -> AppResult<CleanupRuntimeState> {
    let (target_container_id, shared_container_id) =
        sqlx::query_as::<_, (Option<String>, Option<String>)>(
            r#"SELECT target.container_id, shared.container_id
             FROM "GameChallenges" challenge
             LEFT JOIN "KothTargets" target
               ON target.game_id = challenge.game_id
              AND target.challenge_id = challenge.id
             LEFT JOIN "Containers" shared
               ON shared.id = challenge.shared_container_id
            WHERE challenge.game_id = $1 AND challenge.id = $2"#,
        )
        .bind(cycle.game_id)
        .bind(cycle.challenge_id)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .unwrap_or((None, None));

    let snapshot_container_id = target_container_id
        .as_ref()
        .filter(|container_id| !container_id.is_empty())
        .or_else(|| {
            cycle
                .replacement_container_id
                .as_ref()
                .filter(|container_id| !container_id.is_empty())
        })
        .or_else(|| {
            shared_container_id
                .as_ref()
                .filter(|container_id| !container_id.is_empty())
        })
        .or_else(|| {
            cycle
                .old_container_id
                .as_ref()
                .filter(|container_id| !container_id.is_empty())
        })
        .cloned();
    let mut container_ids = Vec::with_capacity(4);
    for container_id in [
        cycle.old_container_id.as_ref(),
        cycle.replacement_container_id.as_ref(),
        target_container_id.as_ref(),
        shared_container_id.as_ref(),
    ] {
        push_runtime_id(&mut container_ids, container_id);
    }
    Ok(CleanupRuntimeState {
        container_ids,
        snapshot_container_id,
    })
}

async fn persist_deadline_target_clear(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH target AS (
             UPDATE "KothTargets"
                SET host = '', port = 0, container_id = NULL,
                    holder_participation_id = NULL, held_since = NULL
              WHERE game_id = $1 AND challenge_id = $2
              RETURNING id
           )
           DELETE FROM "KothClaimStates" claim
            USING target WHERE claim.target_id = target.id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn prepare_deadline_network_shutdown(st: &SharedState, cycle: &CycleRow) -> AppResult<()> {
    let mut access = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    persist_deadline_access_revocation(&mut access, cycle.game_id, cycle.challenge_id).await?;
    access
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    // Commit access revocation first. In particular, a FirewallPending retry
    // must never expose an empty target to cooldown validation while its row is
    // still considered unreleased.
    let mut target = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    persist_deadline_target_clear(&mut target, cycle.game_id, cycle.challenge_id).await?;
    target
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn persist_deadline_snapshot_receipt(
    connection: &mut sqlx::PgConnection,
    cycle_id: i64,
    reset_attempt: i32,
    receipt: Value,
    filesystem_diff: Option<Value>,
) -> AppResult<bool> {
    let inserted = sqlx::query(
        r#"INSERT INTO "KothCycleAuditReceipts"
             (cycle_id, phase, attempt, receipt, filesystem_diff)
           SELECT cycle.id, 'DeadlineSnapshot', $2, $3, $4
             FROM "KothCrownCycles" cycle
            WHERE cycle.id = $1 AND cycle.reset_attempt = $2
           ON CONFLICT (cycle_id, phase, attempt) DO NOTHING"#,
    )
    .bind(cycle_id)
    .bind(reset_attempt.max(0))
    .bind(receipt)
    .bind(filesystem_diff)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    Ok(inserted == 1)
}

async fn capture_deadline_snapshot(
    st: &SharedState,
    cycle: &CycleRow,
    container_id: Option<&str>,
) -> AppResult<()> {
    let recorded: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1 FROM "KothCycleAuditReceipts"
              WHERE cycle_id = $1 AND phase = 'DeadlineSnapshot' AND attempt = $2
           )"#,
    )
    .bind(cycle.id)
    .bind(cycle.reset_attempt.max(0))
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if recorded {
        return Ok(());
    }

    let (receipt, filesystem_diff) = match (container_id, st.containers.backend_kind()) {
        (Some(container_id), ContainerBackendKind::Docker) => {
            match st.containers.snapshot_changes(container_id).await {
                Ok(changes) => {
                    let super::audit::BoundedFilesystemDiff { value, summary } =
                        super::audit::bounded_filesystem_diff(changes)?;
                    (
                        json!({
                            "reason": "eventDeadline",
                            "status": "captured",
                            "containerId": container_id,
                            "filesystemDiffSummary": summary,
                        }),
                        Some(value),
                    )
                }
                Err(AppError::NotFound(_)) => (
                    json!({
                        "reason": "eventDeadline",
                        "status": "unavailable",
                        "containerId": container_id,
                        "unavailableReason": "runtimeNotFound",
                    }),
                    None,
                ),
                Err(error) => return Err(error),
            }
        }
        (Some(container_id), _) => (
            json!({
                "reason": "eventDeadline",
                "status": "unavailable",
                "containerId": container_id,
                "unavailableReason": "filesystemDiffUnsupported",
            }),
            None,
        ),
        (None, _) => (
            json!({
                "reason": "eventDeadline",
                "status": "unavailable",
                "containerId": null,
                "unavailableReason": "missingRuntimeIdentity",
            }),
            None,
        ),
    };
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let inserted = persist_deadline_snapshot_receipt(
        &mut transaction,
        cycle.id,
        cycle.reset_attempt,
        receipt,
        filesystem_diff,
    )
    .await?;
    if !inserted {
        let recorded: bool = sqlx::query_scalar(
            r#"SELECT EXISTS (
                 SELECT 1 FROM "KothCycleAuditReceipts"
                  WHERE cycle_id = $1 AND phase = 'DeadlineSnapshot' AND attempt = $2
               )"#,
        )
        .bind(cycle.id)
        .bind(cycle.reset_attempt.max(0))
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !recorded {
            return Err(AppError::conflict(
                "KotH cycle identity changed before deadline snapshot persistence",
            ));
        }
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn deactivate_deadline_endpoints(
    st: &SharedState,
    game_id: i32,
    container_ids: &[String],
) -> AppResult<()> {
    if container_ids.is_empty() {
        // Access/cooldown intent can change even if recovery has no runtime
        // identity. It still needs exactly one policy reconciliation.
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    } else {
        crate::services::ad_vpn::deactivate_backend_endpoints(&st.db, container_ids)
            .await
            .map(|_| ())?;
    }
    crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
    Ok(())
}

async fn persist_completed_cleanup(
    connection: &mut sqlx::PgConnection,
    cleanup: CompletedCleanup<'_>,
) -> AppResult<()> {
    persist_deadline_access_revocation(connection, cleanup.game_id, cleanup.challenge_id).await?;
    persist_deadline_target_clear(connection, cleanup.game_id, cleanup.challenge_id).await?;
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET shared_container_id = NULL
            WHERE id = $1"#,
    )
    .bind(cleanup.challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "Containers" WHERE container_id = ANY($1)"#)
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

async fn persist_recovery_error(
    connection: &mut sqlx::PgConnection,
    cycle_id: i64,
    message: &str,
) -> AppResult<bool> {
    let updated = sqlx::query(
        r#"UPDATE "KothCrownCycles" cycle
              SET last_error = $2, updated_at = clock_timestamp()
            WHERE cycle.id = $1
              AND cycle.phase <> 'Ended'
              AND (
                   cycle.phase <> 'Completed'
                   OR NOT EXISTS (
                        SELECT 1 FROM "KothCycleAuditReceipts" receipt
                         WHERE receipt.cycle_id = cycle.id
                           AND receipt.phase = 'DeadlineCleanup'
                           AND receipt.attempt = cycle.reset_attempt
                   )
                   OR EXISTS (
                        SELECT 1 FROM "KothTargets" target
                         WHERE target.game_id = cycle.game_id
                           AND target.challenge_id = cycle.challenge_id
                           AND (target.container_id IS NOT NULL OR target.host <> ''
                                OR target.holder_participation_id IS NOT NULL)
                   )
                   OR EXISTS (
                        SELECT 1 FROM "KothClaimStates" claim
                        JOIN "KothTargets" target ON target.id = claim.target_id
                         WHERE target.game_id = cycle.game_id
                           AND target.challenge_id = cycle.challenge_id
                   )
                   OR EXISTS (
                        SELECT 1 FROM "KothTokens" token
                        JOIN "KothCrownCycles" owner ON owner.id = token.cycle_id
                         WHERE owner.game_id = cycle.game_id
                           AND owner.challenge_id = cycle.challenge_id
                           AND token.revoked_at IS NULL
                   )
              )"#,
    )
    .bind(cycle_id)
    .bind(message)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    Ok(updated == 1)
}

/// Persist only operational recovery state. A finalized, already-clean cycle is
/// immutable; a Completed cycle still missing cleanup evidence remains retryable
/// and records its latest failure for operators and the idempotent cron path.
pub(super) async fn record_recovery_error(
    st: &SharedState,
    cycle_id: i64,
    message: &str,
) -> AppResult<()> {
    let mut connection = st
        .pg()
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    persist_recovery_error(&mut connection, cycle_id, message).await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Complete,
    Cleanup,
    AdoptReplacement,
    Reclaim,
    Done,
}

pub(super) const fn action(phase: CrownPhase, replacement_persisted: bool) -> Action {
    match phase {
        CrownPhase::Active | CrownPhase::CooldownReleasePending => Action::Complete,
        CrownPhase::Completed => Action::Cleanup,
        CrownPhase::Ended => Action::Done,
        CrownPhase::CreatePending if !replacement_persisted => Action::AdoptReplacement,
        _ => Action::Reclaim,
    }
}

/// Tear down the final live hill after its cycle evidence has been finalized.
///
/// The caller holds the hill lifecycle lock, which fences the checker and
/// replacement publisher. Access is revoked and reconciled before the final
/// bounded snapshot; runtime destruction happens while the cycle still retains
/// its exact identities, so a crash leaves enough state for an idempotent retry.
/// Finalized cycle fields and immutable scoring evidence are preserved.
pub(super) async fn cleanup_completed_cycle(
    st: &SharedState,
    config: &OfficialConfig,
    cycle: &CycleRow,
    round_number: i32,
) -> AppResult<()> {
    let runtime = load_cleanup_runtime_state(st, cycle).await?;
    prepare_deadline_network_shutdown(st, cycle).await?;
    deactivate_deadline_endpoints(st, cycle.game_id, &runtime.container_ids).await?;
    capture_deadline_snapshot(st, cycle, runtime.snapshot_container_id.as_deref()).await?;
    for container_id in &runtime.container_ids {
        st.containers.destroy(container_id).await?;
    }

    let mut control =
        super::super::super::koth_auth::acquire_game_lock(&st.db, cycle.game_id).await?;
    persist_completed_cleanup(
        &mut *control.transaction_mut(),
        CompletedCleanup {
            cycle_id: cycle.id,
            game_id: cycle.game_id,
            challenge_id: cycle.challenge_id,
            reset_attempt: cycle.reset_attempt,
            round_number,
            container_ids: &runtime.container_ids,
        },
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    for key in [
        format!("_KothScoreBoard_{}", cycle.game_id),
        format!("_KothScoreBoardFrozen_{}", cycle.game_id),
        format!("_KothTimeline_{}", cycle.game_id),
        format!("_KothTimelineFrozen_{}", cycle.game_id),
        format!("_KothHillState_{}_{}", cycle.game_id, cycle.challenge_id),
        format!("latestround:{}", cycle.game_id),
    ] {
        st.cache.remove(&key).await;
    }
    for participation_id in &config.roster {
        st.cache
            .remove(&format!(
                "kothtoken:{}:{}:{}:{}",
                cycle.game_id, cycle.challenge_id, participation_id, round_number
            ))
            .await;
        st.cache
            .remove(&format!(
                "kothtokensall:{}:{}:{}",
                cycle.game_id, participation_id, round_number
            ))
            .await;
    }
    Ok(())
}

pub(super) async fn complete_active_cycle(
    st: &SharedState,
    cycle: &CycleRow,
    round_number: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH completed AS (
             UPDATE "KothCrownCycles"
              SET phase = 'Completed',
                  actual_end_round = CASE
                    WHEN actual_start_round IS NULL THEN NULL
                    ELSE GREATEST($2, actual_start_round) END,
                  finalized_at = COALESCE(finalized_at, clock_timestamp()),
                  completed_at = COALESCE(completed_at, clock_timestamp()),
                  updated_at = clock_timestamp(), last_error = NULL
            WHERE id = $1 AND phase IN ('Active','CooldownReleasePending')
          RETURNING id, reset_attempt
           )
           INSERT INTO "KothCycleAuditReceipts"
             (cycle_id, phase, attempt, receipt, filesystem_diff)
           SELECT id, 'Completed', reset_attempt,
                  jsonb_build_object('reason', 'eventDeadline', 'endedRound', $2), NULL
             FROM completed
           ON CONFLICT (cycle_id, phase, attempt) DO NOTHING"#,
    )
    .bind(cycle.id)
    .bind(round_number)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn terminate_interrupted_cycle(
    st: &SharedState,
    config: &OfficialConfig,
    cycle: &CycleRow,
    round_number: i32,
) -> AppResult<()> {
    let runtime = load_cleanup_runtime_state(st, cycle).await?;
    prepare_deadline_network_shutdown(st, cycle).await?;
    deactivate_deadline_endpoints(st, cycle.game_id, &runtime.container_ids).await?;
    capture_deadline_snapshot(st, cycle, runtime.snapshot_container_id.as_deref()).await?;

    // Runtime destruction happens before cycle identities are cleared. If the
    // backend call fails, the durable cycle still identifies every workload for
    // a later retry. Container backends treat an absent workload as success.
    for container_id in &runtime.container_ids {
        st.containers.destroy(container_id).await?;
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    persist_deadline_access_revocation(&mut transaction, cycle.game_id, cycle.challenge_id).await?;
    persist_deadline_target_clear(&mut transaction, cycle.game_id, cycle.challenge_id).await?;
    sqlx::query(
        r#"WITH removed AS (
             DELETE FROM "Containers" WHERE container_id = ANY($2) RETURNING id
           )
           UPDATE "GameChallenges"
              SET shared_container_id = NULL
            WHERE id = $1 AND shared_container_id IN (SELECT id FROM removed)"#,
    )
    .bind(cycle.challenge_id)
    .bind(&runtime.container_ids)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "KothCrownCycles"
              SET phase = 'Ended',
                  actual_end_round = CASE
                    WHEN actual_start_round IS NULL THEN NULL
                    ELSE GREATEST($2, actual_start_round) END,
                  finalized_at = COALESCE(finalized_at, clock_timestamp()),
                  completed_at = COALESCE(completed_at, clock_timestamp()),
                  provisional_participation_id = NULL,
                  confirmed_participation_id = NULL,
                  confirmation_progress = 0,
                  lease_token = NULL, lease_until = NULL,
                  updated_at = clock_timestamp(), last_error = NULL
            WHERE id = $1 AND phase NOT IN ('Completed','Ended')"#,
    )
    .bind(cycle.id)
    .bind(round_number)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let deadline_receipt = json!({
        "reason": "eventDeadline",
        "reclaimedContainerIds": runtime.container_ids,
        "endedRound": round_number,
    });
    sqlx::query(
        r#"INSERT INTO "KothCycleAuditReceipts"
             (cycle_id, phase, attempt, receipt, filesystem_diff)
           SELECT id, 'Ended', reset_attempt, $2, NULL
             FROM "KothCrownCycles" WHERE id = $1 AND phase = 'Ended'
           ON CONFLICT (cycle_id, phase, attempt) DO NOTHING"#,
    )
    .bind(cycle.id)
    .bind(deadline_receipt)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    for key in [
        format!("_KothScoreBoard_{}", cycle.game_id),
        format!("_KothScoreBoardFrozen_{}", cycle.game_id),
        format!("_KothTimeline_{}", cycle.game_id),
        format!("_KothTimelineFrozen_{}", cycle.game_id),
        format!("_KothHillState_{}_{}", cycle.game_id, cycle.challenge_id),
        format!("latestround:{}", cycle.game_id),
    ] {
        st.cache.remove(&key).await;
    }
    for participation_id in &config.roster {
        st.cache
            .remove(&format!(
                "kothtoken:{}:{}:{}:{}",
                cycle.game_id, cycle.challenge_id, participation_id, round_number
            ))
            .await;
        st.cache
            .remove(&format!(
                "kothtokensall:{}:{}:{}",
                cycle.game_id, participation_id, round_number
            ))
            .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        persist_completed_cleanup, persist_deadline_access_revocation,
        persist_deadline_snapshot_receipt, persist_deadline_target_clear, persist_recovery_error,
        CompletedCleanup,
    };

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn deadline_preparation_and_cleanup_are_idempotent_and_preserve_evidence() {
        use sqlx::{Connection, PgConnection};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, phase TEXT NOT NULL,
              reset_attempt INTEGER NOT NULL,
              old_container_id TEXT,
              replacement_container_id TEXT,
              provisional_participation_id INTEGER,
              confirmed_participation_id INTEGER,
              confirmation_progress INTEGER NOT NULL,
              last_error TEXT,
              updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, host TEXT NOT NULL,
              port INTEGER NOT NULL, container_id TEXT,
              holder_participation_id INTEGER, held_since TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothClaimStates" (target_id INTEGER PRIMARY KEY);
            CREATE TEMP TABLE "KothTokens" (
              id INTEGER PRIMARY KEY, cycle_id BIGINT NOT NULL,
              revoked_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothCycleCooldowns" (
              cycle_id BIGINT NOT NULL, network_enforced BOOLEAN NOT NULL,
              network_enforced_at TIMESTAMPTZ,
              network_released_at TIMESTAMPTZ,
              CHECK (
                (NOT network_enforced OR network_enforced_at IS NOT NULL)
                AND (network_released_at IS NULL OR network_enforced_at IS NOT NULL)
              )
            );
            CREATE TEMP TABLE "Containers" (id INTEGER PRIMARY KEY, container_id TEXT NOT NULL);
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, shared_container_id INTEGER
            );
            CREATE TEMP TABLE "KothCycleAuditReceipts" (
              cycle_id BIGINT NOT NULL, phase TEXT NOT NULL,
              attempt INTEGER NOT NULL, receipt JSONB NOT NULL,
              filesystem_diff JSONB,
              UNIQUE (cycle_id, phase, attempt)
            );
            INSERT INTO "KothCrownCycles"
              (id, game_id, challenge_id, phase, reset_attempt,
               old_container_id, replacement_container_id, provisional_participation_id,
               confirmed_participation_id, confirmation_progress)
            VALUES
              (41, 7, 9, 'Completed', 2, 'prior-runtime', 'final-runtime', 11, 11, 2),
              (42, 8, 10, 'FirewallPending', 1, 'old-firewall-runtime',
               'firewall-runtime', NULL, NULL, 0),
              (43, 8, 10, 'Completed', 1, 'historical-runtime',
               'historical-replacement', NULL, NULL, 0),
              (44, 8, 11, 'Active', 1, 'other-hill-runtime',
               'other-hill-replacement', NULL, NULL, 0),
              (45, 9, 12, 'Active', 1, 'protected-runtime',
               'protected-replacement', NULL, NULL, 0);
            INSERT INTO "KothTargets" VALUES
              (3, 7, 9, '10.0.0.8', 8080, 'final-runtime', 11, clock_timestamp()),
              (4, 8, 10, '10.0.0.9', 8080, 'firewall-runtime', 12, clock_timestamp());
            INSERT INTO "KothClaimStates" VALUES (3), (4);
            INSERT INTO "KothTokens" VALUES
              (1, 41, NULL), (2, 41, NULL), (3, 42, NULL), (4, 45, NULL);
            INSERT INTO "KothCycleCooldowns" VALUES
              (41, TRUE, clock_timestamp(), NULL), (42, FALSE, NULL, NULL),
              (43, TRUE, clock_timestamp(), NULL), (44, TRUE, clock_timestamp(), NULL),
              (45, FALSE, NULL, NULL);
            INSERT INTO "Containers" VALUES (5, 'final-runtime');
            INSERT INTO "GameChallenges" VALUES (9, 5);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let protected_error = persist_deadline_access_revocation(&mut connection, 9, 12)
            .await
            .unwrap_err();
        assert!(protected_error.to_string().contains("Active cycle"));
        assert_eq!(
            sqlx::query_as::<_, (bool, i64)>(
                r#"SELECT token.revoked_at IS NULL,
                          (SELECT COUNT(*) FROM "KothCycleCooldowns" WHERE cycle_id = 45)
                     FROM "KothTokens" token WHERE token.id = 4"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            (true, 1)
        );

        // FirewallPending can contain a selected cooldown that failed before
        // network_enforced was persisted. Revoke it durably without clearing
        // the target or either crash-recovery identity.
        for _ in 0..2 {
            let mut transaction = connection.begin().await.unwrap();
            persist_deadline_access_revocation(&mut transaction, 8, 10)
                .await
                .unwrap();
            transaction.commit().await.unwrap();
        }
        assert_eq!(
            sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
                r#"SELECT phase, old_container_id, replacement_container_id
                     FROM "KothCrownCycles" WHERE id = 42"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            (
                "FirewallPending".to_string(),
                Some("old-firewall-runtime".to_string()),
                Some("firewall-runtime".to_string()),
            )
        );
        assert_eq!(
            sqlx::query_as::<_, (String, Option<String>)>(
                r#"SELECT host, container_id FROM "KothTargets" WHERE id = 4"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            ("10.0.0.9".to_string(), Some("firewall-runtime".to_string()))
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "KothTokens"
                    WHERE cycle_id = 42 AND revoked_at IS NULL"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                r#"SELECT COUNT(*) FILTER (WHERE cycle_id = 42),
                          COUNT(*) FILTER (
                            WHERE cycle_id = 43 AND network_released_at IS NOT NULL)
                     FROM "KothCycleCooldowns" WHERE cycle_id IN (42, 43)"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            (0, 1)
        );
        assert!(sqlx::query_scalar::<_, bool>(
            r#"SELECT network_released_at IS NULL
                     FROM "KothCycleCooldowns" WHERE cycle_id = 44"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap());
        persist_deadline_target_clear(&mut connection, 8, 10)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, i32, Option<String>)>(
                r#"SELECT host, port, container_id FROM "KothTargets" WHERE id = 4"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            (String::new(), 0, None)
        );

        // The immutable phase/attempt key retains only the first snapshot, so a
        // destroy retry never duplicates or rewrites final filesystem evidence.
        persist_deadline_snapshot_receipt(
            &mut connection,
            41,
            2,
            json!({"status": "captured", "containerId": "final-runtime"}),
            Some(json!([{"path": "/patched", "kind": "Modified"}])),
        )
        .await
        .unwrap();
        persist_deadline_snapshot_receipt(
            &mut connection,
            41,
            2,
            json!({"status": "unavailable", "unavailableReason": "mustNotReplace"}),
            None,
        )
        .await
        .unwrap();

        let container_ids = ["final-runtime".to_string()];
        assert!(
            persist_recovery_error(&mut connection, 41, "runtime destroy failed")
                .await
                .unwrap()
        );
        for cleanup_attempt in 0..2 {
            if cleanup_attempt == 1 {
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Containers""#)
                        .fetch_one(&mut connection)
                        .await
                        .unwrap(),
                    0
                );
                sqlx::query(r#"UPDATE "GameChallenges" SET shared_container_id = 5 WHERE id = 9"#)
                    .execute(&mut connection)
                    .await
                    .unwrap();
            }
            let mut transaction = connection.begin().await.unwrap();
            persist_completed_cleanup(
                &mut transaction,
                CompletedCleanup {
                    cycle_id: 41,
                    game_id: 7,
                    challenge_id: 9,
                    reset_attempt: 2,
                    round_number: 12,
                    container_ids: &container_ids,
                },
            )
            .await
            .unwrap();
            transaction.commit().await.unwrap();
        }

        let target: (String, i32, Option<String>, Option<i32>) = sqlx::query_as(
            r#"SELECT host, port, container_id, holder_participation_id
                 FROM "KothTargets" WHERE id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(target, (String::new(), 0, None, None));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "KothClaimStates""#)
                .fetch_one(&mut connection)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "KothTokens"
                    WHERE cycle_id IN (41, 42) AND revoked_at IS NULL"#
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                r#"SELECT COUNT(*) FROM "KothCycleCooldowns"
                    WHERE cycle_id = 41 AND network_released_at IS NULL"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_as::<_, (Option<String>, Option<i32>, Option<i32>, i32)>(
                r#"SELECT replacement_container_id, provisional_participation_id,
                          confirmed_participation_id, confirmation_progress
                     FROM "KothCrownCycles" WHERE id = 41"#
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            (Some("final-runtime".to_string()), Some(11), Some(11), 2)
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                r#"SELECT last_error FROM "KothCrownCycles" WHERE id = 41"#
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            None
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Containers""#)
                .fetch_one(&mut connection)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<i32>>(
                r#"SELECT shared_container_id FROM "GameChallenges" WHERE id = 9"#
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            None
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "KothCycleAuditReceipts""#)
                .fetch_one(&mut connection)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_as::<_, (String, serde_json::Value)>(
                r#"SELECT receipt->>'status', filesystem_diff
                     FROM "KothCycleAuditReceipts"
                    WHERE cycle_id = 41 AND phase = 'DeadlineSnapshot' AND attempt = 2"#,
            )
            .fetch_one(&mut connection)
            .await
            .unwrap(),
            (
                "captured".to_string(),
                json!([{"path": "/patched", "kind": "Modified"}]),
            )
        );
        assert!(
            !persist_recovery_error(&mut connection, 41, "must not stain clean evidence")
                .await
                .unwrap()
        );
    }
}
