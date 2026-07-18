//! Functional readiness and durable recovery of an unusable replacement.

use serde_json::json;

use crate::app_state::SharedState;
use crate::services::ad::engine::{koth_auth, AdCheckStatus};
use crate::services::container::ContainerLiveness;
use crate::utils::error::{AppError, AppResult};

use super::super::CrownPhase;
use super::data::{load_hill_spec, CycleRow};
use super::{record_receipt, set_phase};

const STOPPED_ERROR: &str = "replacement container stopped before readiness";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LivenessAction {
    Validate,
    Reclaim,
    Retry,
}

pub(super) const fn liveness_action(liveness: ContainerLiveness) -> LivenessAction {
    match liveness {
        ContainerLiveness::Running => LivenessAction::Validate,
        ContainerLiveness::Stopped => LivenessAction::Reclaim,
        ContainerLiveness::Unknown => LivenessAction::Retry,
    }
}

/// Move the exact published replacement back into the durable destruction
/// phase before touching the container backend. A crash after this statement
/// is therefore resumed by `destroy_old`, which owns idempotent external
/// teardown. Advancing `reset_attempt` also invalidates the failed window
/// before a replacement can be created with a fresh operation identity.
async fn move_stopped_replacement_to_destroy(
    connection: &mut sqlx::PgConnection,
    cycle_id: i64,
    game_id: i32,
    challenge_id: i32,
    container_id: &str,
) -> AppResult<Option<i32>> {
    sqlx::query_scalar::<_, i32>(
        r#"WITH moved AS (
             UPDATE "KothCrownCycles" cycle
                SET phase = 'DestroyPending',
                    old_container_id = cycle.replacement_container_id,
                    replacement_container_id = NULL,
                    replacement_host = NULL,
                    replacement_port = NULL,
                    reset_attempt = cycle.reset_attempt + 1,
                    readiness_attempt = cycle.readiness_attempt + 1,
                    readiness_failures = cycle.readiness_failures + 1,
                    readiness_error = $5,
                    last_error = $5,
                    provisional_participation_id = NULL,
                    confirmed_participation_id = NULL,
                    confirmation_progress = 0,
                    updated_at = clock_timestamp()
              WHERE cycle.id = $1
                AND cycle.game_id = $2
                AND cycle.challenge_id = $3
                AND cycle.phase = 'ReadinessPending'
                AND cycle.replacement_container_id = $4
                AND EXISTS (
                    SELECT 1 FROM "KothTargets" target
                     WHERE target.game_id = cycle.game_id
                       AND target.challenge_id = cycle.challenge_id
                       AND target.container_id = cycle.replacement_container_id
                )
          RETURNING cycle.id, cycle.challenge_id,
                    cycle.reset_attempt - 1 AS failed_attempt
           ), revoked AS (
             UPDATE "KothTokens" token
                SET revoked_at = COALESCE(token.revoked_at, clock_timestamp())
               FROM moved
              WHERE token.cycle_id = moved.id
                AND token.challenge_id = moved.challenge_id
                AND token.reset_attempt = moved.failed_attempt
          RETURNING token.id
           ), receipt AS (
             INSERT INTO "KothCycleAuditReceipts"
               (cycle_id, phase, attempt, receipt, filesystem_diff)
             SELECT moved.id, 'ReadinessPending', moved.failed_attempt,
                    jsonb_build_object(
                      'containerId', $4,
                      'failure', $5,
                      'action', 'reclaimAndRecreate',
                      'nextResetAttempt', moved.failed_attempt + 1
                    ), NULL
               FROM moved
             ON CONFLICT (cycle_id, phase, attempt) DO NOTHING
          RETURNING id
           )
           SELECT failed_attempt FROM moved"#,
    )
    .bind(cycle_id)
    .bind(game_id)
    .bind(challenge_id)
    .bind(container_id)
    .bind(STOPPED_ERROR)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn reclaim_stopped(st: &SharedState, cycle: &CycleRow, container_id: &str) -> AppResult<()> {
    // The lifecycle takes the hill lock before reaching this function. Taking
    // the game lock second matches the checker/capture lock order and makes
    // token revocation atomic with the durable phase transition.
    let mut control = koth_auth::acquire_game_lock(&st.db, cycle.game_id).await?;
    let moved = move_stopped_replacement_to_destroy(
        &mut *control.transaction_mut(),
        cycle.id,
        cycle.game_id,
        cycle.challenge_id,
        container_id,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if moved.is_none() {
        return Err(AppError::conflict(
            "replacement identity changed while preparing readiness recovery",
        ));
    }
    Ok(())
}

pub(super) async fn validate(st: &SharedState, cycle: &CycleRow) -> AppResult<()> {
    let spec = load_hill_spec(st, cycle).await?;
    let container_id = cycle
        .replacement_container_id
        .as_deref()
        .ok_or_else(|| AppError::internal("replacement container identity is missing"))?;
    match liveness_action(st.containers.inspect_liveness(container_id).await?) {
        LivenessAction::Validate => {}
        LivenessAction::Reclaim => return reclaim_stopped(st, cycle, container_id).await,
        LivenessAction::Retry => {
            return Err(AppError::conflict(
                "replacement container is still transitioning",
            ))
        }
    }

    let host = cycle
        .replacement_host
        .as_deref()
        .ok_or_else(|| AppError::internal("replacement host is missing"))?;
    let port = cycle
        .replacement_port
        .ok_or_else(|| AppError::internal("replacement port is missing"))?;
    let (status, message) = super::super::super::checker::validate_koth_functional_readiness(
        spec.checker_dir.as_deref(),
        host,
        port,
        cycle.planned_start_round,
        cycle.challenge_id,
    )
    .await;
    if status != AdCheckStatus::Ok {
        let error = message.unwrap_or_else(|| "functional checker did not return Ok".to_string());
        sqlx::query(
            r#"UPDATE "KothCrownCycles"
                  SET readiness_attempt = readiness_attempt + 1,
                      readiness_failures = readiness_failures + 1,
                      readiness_error = $2, last_error = $2,
                      updated_at = clock_timestamp()
                WHERE id = $1 AND phase = 'ReadinessPending'
                  AND replacement_container_id = $3"#,
        )
        .bind(cycle.id)
        .bind(&error)
        .bind(container_id)
        .execute(st.pg())
        .await
        .map_err(|db_error| AppError::internal(db_error.to_string()))?;
        return Err(AppError::conflict(error));
    }
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    set_phase(
        &mut transaction,
        cycle.id,
        CrownPhase::ReadinessPending,
        CrownPhase::FirewallPending,
    )
    .await?;
    record_receipt(
        &mut transaction,
        cycle,
        CrownPhase::ReadinessPending,
        json!({
            "containerId": container_id,
            "functionalStatus": "Ok",
            "readinessAttempt": cycle.readiness_attempt,
        }),
        None,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_is_reclaimed_while_unknown_is_only_retried() {
        assert_eq!(
            liveness_action(ContainerLiveness::Running),
            LivenessAction::Validate
        );
        assert_eq!(
            liveness_action(ContainerLiveness::Stopped),
            LivenessAction::Reclaim
        );
        assert_eq!(
            liveness_action(ContainerLiveness::Unknown),
            LivenessAction::Retry
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn stopped_replacement_moves_once_to_durable_destroy() {
        use sqlx::{Connection, PgConnection};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothTargets" (
              game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              container_id TEXT,
              PRIMARY KEY (game_id, challenge_id)
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY,
              game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              phase TEXT NOT NULL,
              old_container_id TEXT,
              replacement_container_id TEXT,
              replacement_host TEXT,
              replacement_port INTEGER,
              reset_attempt INTEGER NOT NULL,
              readiness_attempt INTEGER NOT NULL,
              readiness_failures INTEGER NOT NULL,
              readiness_error TEXT,
              last_error TEXT,
              provisional_participation_id INTEGER,
              confirmed_participation_id INTEGER,
              confirmation_progress INTEGER NOT NULL,
              updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            CREATE TEMP TABLE "KothTokens" (
              id BIGSERIAL PRIMARY KEY,
              cycle_id BIGINT NOT NULL,
              challenge_id INTEGER NOT NULL,
              reset_attempt INTEGER NOT NULL,
              revoked_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothCycleAuditReceipts" (
              id BIGSERIAL PRIMARY KEY,
              cycle_id BIGINT NOT NULL,
              phase TEXT NOT NULL,
              attempt INTEGER NOT NULL,
              receipt JSONB NOT NULL,
              filesystem_diff JSONB,
              UNIQUE (cycle_id, phase, attempt)
            );
            INSERT INTO "KothTargets" VALUES (7, 9, 'dead-replacement');
            INSERT INTO "KothCrownCycles" VALUES (
              41, 7, 9, 'ReadinessPending', 'old-container',
              'dead-replacement', '10.0.0.8', 8080,
              4, 2, 1, NULL, NULL, 11, 12, 1, clock_timestamp()
            );
            INSERT INTO "KothTokens"
              (cycle_id, challenge_id, reset_attempt, revoked_at)
            VALUES (41, 9, 4, NULL);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let failed_attempt =
            move_stopped_replacement_to_destroy(&mut connection, 41, 7, 9, "dead-replacement")
                .await
                .unwrap();
        assert_eq!(failed_attempt, Some(4));

        let state: (String, Option<String>, Option<String>, i32, i32, i32, bool) = sqlx::query_as(
            r#"SELECT phase, old_container_id, replacement_container_id,
                          reset_attempt, readiness_attempt, readiness_failures,
                          provisional_participation_id IS NULL
                     FROM "KothCrownCycles" WHERE id = 41"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            state,
            (
                "DestroyPending".to_string(),
                Some("dead-replacement".to_string()),
                None,
                5,
                3,
                2,
                true,
            )
        );
        assert!(sqlx::query_scalar::<_, bool>(
            r#"SELECT revoked_at IS NOT NULL FROM "KothTokens" WHERE id = 1"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap());
        let receipt: (i32, String) = sqlx::query_as(
            r#"SELECT attempt, receipt->>'containerId'
                 FROM "KothCycleAuditReceipts" WHERE cycle_id = 41"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(receipt, (4, "dead-replacement".to_string()));

        assert_eq!(
            move_stopped_replacement_to_destroy(&mut connection, 41, 7, 9, "dead-replacement",)
                .await
                .unwrap(),
            None
        );
        let failures: i32 =
            sqlx::query_scalar(r#"SELECT readiness_failures FROM "KothCrownCycles" WHERE id = 41"#)
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(failures, 2);
    }
}
