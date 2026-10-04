//! Bounded recovery for exercise creates whose runtime outcome outlived its
//! detached HTTP owner.

use std::time::Duration;

use crate::app_state::SharedState;
use crate::services::container::ContainerLiveness;

use super::*;

const STALE_GRACE: Duration = Duration::from_secs(30);
const RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(30);
const RECOVERY_ATTEMPT_DEADLINE: Duration = Duration::from_secs(30);
const MAX_RECOVERIES_PER_PASS: usize = 4;

#[derive(sqlx::FromRow)]
struct ReconciliationClaim {
    operation_id: Uuid,
    publication_id: Uuid,
    user_id: Uuid,
    exercise_id: i32,
    backend_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PublishedExerciseContainer {
    backend_id: String,
    status: i16,
    lease_live: bool,
    is_proxy: bool,
    ip: String,
    port: i32,
    public_ip: Option<String>,
    public_port: Option<i32>,
    instance_id: i32,
    flag_id: Option<i32>,
}

impl PublishedExerciseContainer {
    fn entry(&self, publication_id: Uuid) -> String {
        if self.is_proxy {
            publication_id.to_string()
        } else {
            format!(
                "{}:{}",
                self.public_ip.as_deref().unwrap_or(&self.ip),
                self.public_port.unwrap_or(self.port)
            )
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryAction {
    Complete,
    Destroy,
    Defer,
}

enum InspectionOutcome {
    Liveness(ContainerLiveness),
    Absent,
    Deferred,
}

fn recovery_action(published_running: bool, liveness: ContainerLiveness) -> RecoveryAction {
    match liveness {
        ContainerLiveness::Running if published_running => RecoveryAction::Complete,
        ContainerLiveness::Running | ContainerLiveness::Stopped => RecoveryAction::Destroy,
        ContainerLiveness::Unknown => RecoveryAction::Defer,
    }
}

async fn claim_one(pool: &sqlx::PgPool) -> AppResult<Option<ReconciliationClaim>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let admitted: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(CLAIM_LOCK)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
    if !admitted {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(None);
    }
    if active_count(&mut transaction).await? >= MAX_DEPLOYMENT_OPERATIONS {
        transaction.commit().await.map_err(database_error)?;
        return Ok(None);
    }

    let stale_seconds = i64::try_from(STALE_GRACE.as_secs()).expect("stale grace fits i64");
    let candidate = sqlx::query_as::<_, ReconciliationClaim>(
        r#"SELECT operation_id, publication_id, user_id, exercise_id, backend_id
             FROM "ExerciseContainerOperations"
            WHERE state = 'Running' AND intent = 'Create'
              AND runtime_started = TRUE
              AND lease_expires_at_utc
                  < clock_timestamp() - $1::bigint * interval '1 second'
            ORDER BY lease_expires_at_utc, operation_id
            LIMIT 1
            FOR UPDATE SKIP LOCKED"#,
    )
    .bind(stale_seconds)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    let Some(candidate) = candidate else {
        transaction.commit().await.map_err(database_error)?;
        return Ok(None);
    };

    // Serialize only the short claim with the same exact owner identity used
    // by managed-container reaping. If a reaper already owns it, leave the row
    // stale for a later pass. Once this transaction commits, the renewed lease
    // makes a later reaper back off without retaining a pool connection while
    // the runtime is inspected or destroyed.
    let owner_lock = format!(
        "exercise-container:{}:{}",
        candidate.user_id, candidate.exercise_id
    );
    let owner_available = crate::utils::single_flight::try_acquire_transaction_advisory_lock(
        &mut transaction,
        &owner_lock,
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !owner_available {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(None);
    }
    let teardown_pending: bool = sqlx::query_scalar(MANAGED_REAP_PENDING_SQL)
        .bind(&owner_lock)
        .bind(candidate.publication_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
    if teardown_pending {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(None);
    }

    let renewed = sqlx::query(
        r#"UPDATE "ExerciseContainerOperations"
              SET lease_expires_at_utc = clock_timestamp() + interval '3 minutes',
                  updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND state = 'Running'"#,
    )
    .bind(candidate.operation_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    if renewed.rows_affected() != 1 {
        transaction.rollback().await.map_err(database_error)?;
        return Ok(None);
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(Some(candidate))
}

async fn load_exact_publication(
    pool: &sqlx::PgPool,
    claim: &ReconciliationClaim,
) -> AppResult<Option<PublishedExerciseContainer>> {
    sqlx::query_as::<_, PublishedExerciseContainer>(
        r#"SELECT container.container_id AS backend_id, container.status,
                  container.expect_stop_at > clock_timestamp() AS lease_live,
                  container.is_proxy, container.ip, container.port,
                  container.public_ip, container.public_port,
                  instance.id AS instance_id, instance.flag_id
             FROM "Containers" container
             JOIN "ExerciseInstances" instance
               ON instance.id = container.exercise_instance_id
              AND instance.container_id = container.id
            WHERE container.id = $1 AND instance.user_id = $2
              AND instance.exercise_id = $3 AND instance.is_loaded = TRUE"#,
    )
    .bind(claim.publication_id)
    .bind(claim.user_id)
    .bind(claim.exercise_id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)
}

fn docker_id_prefix(backend_id: &str) -> Option<String> {
    ((12..=64).contains(&backend_id.len())
        && backend_id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| backend_id[..12].to_ascii_lowercase())
}

async fn backend_has_other_durable_owner(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    backend_id: &str,
) -> AppResult<bool> {
    let prefix = docker_id_prefix(backend_id);
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1 FROM "Containers" WHERE container_id = $1
                  OR ($2::text IS NOT NULL AND LOWER(LEFT(container_id, 12)) = $2)
               UNION ALL
               SELECT 1 FROM "AdTeamServices" WHERE container_id = $1
                  OR ($2::text IS NOT NULL AND LOWER(LEFT(container_id, 12)) = $2)
               UNION ALL
               SELECT 1 FROM "KothTargets" WHERE container_id = $1
                  OR ($2::text IS NOT NULL AND LOWER(LEFT(container_id, 12)) = $2)
               UNION ALL
               SELECT 1 FROM "KothCrownCycles" cycle
                CROSS JOIN LATERAL unnest(ARRAY[
                    cycle.old_container_id, cycle.replacement_container_id
                ]) runtime(runtime_id)
                WHERE cycle.phase <> 'Ended'
                  AND (runtime_id = $1 OR ($2::text IS NOT NULL
                       AND LOWER(LEFT(runtime_id, 12)) = $2))
               UNION ALL
               SELECT 1 FROM "PlayerContainerOperations"
                WHERE backend_id IS NOT NULL AND state = 'Running'
                  AND lease_expires_at_utc > clock_timestamp() - interval '5 minutes'
                  AND (backend_id = $1 OR ($2::text IS NOT NULL
                       AND LOWER(LEFT(backend_id, 12)) = $2))
               UNION ALL
               SELECT 1 FROM "ExerciseContainerOperations"
                WHERE operation_id <> $3 AND backend_id IS NOT NULL
                  AND state = 'Running'
                  AND lease_expires_at_utc > clock_timestamp() - interval '5 minutes'
                  AND (backend_id = $1 OR ($2::text IS NOT NULL
                       AND LOWER(LEFT(backend_id, 12)) = $2))
           )"#,
    )
    .bind(backend_id)
    .bind(prefix)
    .bind(operation_id)
    .fetch_one(pool)
    .await
    .map_err(database_error)
}

async fn mark_failed(pool: &sqlx::PgPool, operation: &ClaimedOperation) -> AppResult<bool> {
    sqlx::query(
        r#"UPDATE "ExerciseContainerOperations"
              SET state = 'Failed', result = NULL,
                  lease_expires_at_utc = clock_timestamp(),
                  updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    .map(|result| result.rows_affected() == 1)
    .map_err(database_error)
}

async fn defer(pool: &sqlx::PgPool, operation: &ClaimedOperation) -> AppResult<()> {
    let retry_seconds =
        i64::try_from(RECOVERY_RETRY_DELAY.as_secs()).expect("retry delay fits i64");
    sqlx::query(
        r#"UPDATE "ExerciseContainerOperations"
              SET lease_expires_at_utc =
                      clock_timestamp() + $3::bigint * interval '1 second',
                  updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .bind(retry_seconds)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(database_error)
}

async fn complete_exact_publication(
    pool: &sqlx::PgPool,
    claim: &ReconciliationClaim,
    operation: &ClaimedOperation,
    container: &PublishedExerciseContainer,
) -> AppResult<bool> {
    let entry = serde_json::Value::String(container.entry(claim.publication_id));
    let updated = sqlx::query(
        r#"UPDATE "ExerciseContainerOperations" operation
              SET state = 'Succeeded', result = $3,
                  updated_at_utc = clock_timestamp(),
                  lease_expires_at_utc = clock_timestamp() + interval '24 hours'
            WHERE operation.operation_id = $1
              AND operation.publication_id = $2
              AND operation.state = 'Running'
              AND EXISTS (
                  SELECT 1
                    FROM "Containers" current
                    JOIN "ExerciseInstances" instance
                      ON instance.id = current.exercise_instance_id
                     AND instance.container_id = current.id
                   WHERE current.id = operation.publication_id
                     AND current.container_id = $4
                     AND current.status = $5
                     AND current.expect_stop_at > clock_timestamp()
                     AND instance.user_id = $6
                     AND instance.exercise_id = $7
                     AND instance.is_loaded = TRUE
              )"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .bind(entry)
    .bind(&container.backend_id)
    .bind(ContainerStatus::Running as i16)
    .bind(claim.user_id)
    .bind(claim.exercise_id)
    .execute(pool)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() == 1 {
        return Ok(true);
    }

    tracing::warn!(
        operation_id = %operation.operation_id,
        publication_id = %operation.publication_id,
        "exercise operation publication changed before recovery completion"
    );
    defer(pool, operation).await?;
    Ok(false)
}

async fn inspect_for_recovery(
    st: &SharedState,
    operation: &ClaimedOperation,
    backend_id: &str,
) -> AppResult<InspectionOutcome> {
    match st.containers.inspect_liveness(backend_id).await {
        Ok(liveness) => Ok(InspectionOutcome::Liveness(liveness)),
        Err(AppError::NotFound(error)) => {
            tracing::info!(
                operation_id = %operation.operation_id,
                %backend_id,
                %error,
                "exercise operation recovery confirmed an absent runtime"
            );
            Ok(InspectionOutcome::Absent)
        }
        Err(error) => {
            tracing::warn!(
                operation_id = %operation.operation_id,
                %backend_id,
                %error,
                "exercise operation recovery deferred after runtime inspection failure"
            );
            defer(st.pg(), operation).await?;
            Ok(InspectionOutcome::Deferred)
        }
    }
}

async fn clear_absent_exact_publication(
    st: &SharedState,
    operation: &ClaimedOperation,
    container: &PublishedExerciseContainer,
) -> AppResult<bool> {
    super::super::clear_exercise_container_owner(
        st.pg(),
        Some(container.instance_id),
        operation.publication_id,
        Some(&container.backend_id),
        container.flag_id,
    )
    .await?;
    mark_failed(st.pg(), operation).await
}

async fn destroy_exact_publication(
    st: &SharedState,
    operation: &ClaimedOperation,
    container: &PublishedExerciseContainer,
) -> AppResult<bool> {
    if let Err(error) = super::super::destroy_owned_exercise_container_with(
        st.pg(),
        Some(container.instance_id),
        operation.publication_id,
        &container.backend_id,
        container.flag_id,
        crate::services::traffic::destroy_container_after_capture_fence(st, &container.backend_id),
    )
    .await
    {
        tracing::warn!(
            operation_id = %operation.operation_id,
            backend_id = %container.backend_id,
            %error,
            "exercise operation recovery retained an owned runtime after destroy failure"
        );
        defer(st.pg(), operation).await?;
        return Ok(false);
    }
    mark_failed(st.pg(), operation).await
}

async fn recover_claim(st: &SharedState, claim: ReconciliationClaim) -> AppResult<bool> {
    let operation = ClaimedOperation {
        operation_id: claim.operation_id,
        publication_id: claim.publication_id,
    };
    if let Some(container) = load_exact_publication(st.pg(), &claim).await? {
        if claim.backend_id.is_none() {
            record_backend(st.pg(), &operation, &container.backend_id).await?;
        }
        if claim
            .backend_id
            .as_deref()
            .is_some_and(|backend_id| backend_id != container.backend_id)
        {
            tracing::warn!(
                operation_id = %operation.operation_id,
                recorded_backend = ?claim.backend_id,
                published_backend = %container.backend_id,
                "exercise operation backend identity no longer owns its publication"
            );
            return mark_failed(st.pg(), &operation).await;
        }
        let liveness = match inspect_for_recovery(st, &operation, &container.backend_id).await? {
            InspectionOutcome::Liveness(liveness) => liveness,
            InspectionOutcome::Absent => {
                return clear_absent_exact_publication(st, &operation, &container).await;
            }
            InspectionOutcome::Deferred => return Ok(false),
        };
        return match recovery_action(
            container.status == ContainerStatus::Running as i16 && container.lease_live,
            liveness,
        ) {
            RecoveryAction::Complete => {
                complete_exact_publication(st.pg(), &claim, &operation, &container).await
            }
            RecoveryAction::Destroy => destroy_exact_publication(st, &operation, &container).await,
            RecoveryAction::Defer => defer(st.pg(), &operation).await.map(|_| false),
        };
    }

    let backend_was_persisted = claim.backend_id.is_some();
    let backend_id = match claim.backend_id {
        Some(backend_id) => Some(backend_id),
        None => match st
            .containers
            .find_operation_runtime(&format!("exercise-container:{}", claim.operation_id))
            .await
        {
            Ok(backend_id) => backend_id,
            Err(error) => {
                tracing::warn!(
                    operation_id = %operation.operation_id,
                    %error,
                    "exercise operation recovery deferred after runtime discovery failure"
                );
                defer(st.pg(), &operation).await?;
                return Ok(false);
            }
        },
    };
    let Some(backend_id) = backend_id else {
        return mark_failed(st.pg(), &operation).await;
    };
    // Discovery closes the create/record crash window. Persist it before any
    // liveness outcome can defer so the orphan sweep recognizes this operation
    // as the runtime's durable retry owner.
    if !backend_was_persisted {
        record_backend(st.pg(), &operation, &backend_id).await?;
    }
    if backend_has_other_durable_owner(st.pg(), operation.operation_id, &backend_id).await? {
        tracing::warn!(
            operation_id = %operation.operation_id,
            %backend_id,
            "exercise operation recovery refused to destroy a differently owned runtime"
        );
        return mark_failed(st.pg(), &operation).await;
    }
    let liveness = match inspect_for_recovery(st, &operation, &backend_id).await? {
        InspectionOutcome::Liveness(liveness) => liveness,
        InspectionOutcome::Absent => return mark_failed(st.pg(), &operation).await,
        InspectionOutcome::Deferred => return Ok(false),
    };
    match recovery_action(false, liveness) {
        RecoveryAction::Destroy => {
            if let Err(error) =
                crate::services::traffic::destroy_container_after_capture_fence(st, &backend_id)
                    .await
            {
                tracing::warn!(
                    operation_id = %operation.operation_id,
                    %backend_id,
                    %error,
                    "exercise operation recovery retained an unpublished runtime after destroy failure"
                );
                defer(st.pg(), &operation).await?;
                return Ok(false);
            }
            mark_failed(st.pg(), &operation).await
        }
        RecoveryAction::Defer => defer(st.pg(), &operation).await.map(|_| false),
        RecoveryAction::Complete => Err(AppError::internal(
            "an unpublished exercise runtime cannot complete an operation",
        )),
    }
}

async fn purge_terminal(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    sqlx::query(
        r#"WITH victims AS (
               SELECT operation_id FROM "ExerciseContainerOperations"
                WHERE state <> 'Running'
                  AND updated_at_utc < clock_timestamp() - interval '24 hours'
                ORDER BY updated_at_utc, operation_id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           DELETE FROM "ExerciseContainerOperations" operation
            USING victims WHERE operation.operation_id = victims.operation_id"#,
    )
    .bind(limit.clamp(1, 256))
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
    .map_err(database_error)
}

pub(crate) async fn sweep(st: &SharedState, terminal_limit: i64) -> AppResult<u64> {
    let mut reconciled = 0;
    for _ in 0..MAX_RECOVERIES_PER_PASS {
        let Some(claim) = claim_one(st.pg()).await? else {
            break;
        };
        let operation = ClaimedOperation {
            operation_id: claim.operation_id,
            publication_id: claim.publication_id,
        };
        match tokio::time::timeout(RECOVERY_ATTEMPT_DEADLINE, recover_claim(st, claim)).await {
            Ok(result) => {
                if result? {
                    reconciled += 1;
                }
            }
            Err(_) => {
                tracing::warn!(
                    operation_id = %operation.operation_id,
                    "exercise operation recovery exceeded its deadline"
                );
                defer(st.pg(), &operation).await?;
            }
        }
    }
    Ok(reconciled + purge_terminal(st.pg(), terminal_limit).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncertain_liveness_is_never_destructive() {
        assert_eq!(
            recovery_action(true, ContainerLiveness::Unknown),
            RecoveryAction::Defer
        );
        assert_eq!(
            recovery_action(false, ContainerLiveness::Unknown),
            RecoveryAction::Defer
        );
    }

    #[test]
    fn only_an_exact_live_publication_can_complete() {
        assert_eq!(
            recovery_action(true, ContainerLiveness::Running),
            RecoveryAction::Complete
        );
        assert_eq!(
            recovery_action(false, ContainerLiveness::Running),
            RecoveryAction::Destroy
        );
        assert_eq!(
            recovery_action(true, ContainerLiveness::Stopped),
            RecoveryAction::Destroy
        );
    }

    #[test]
    fn each_sweep_has_fixed_admission_and_time_bounds() {
        assert_eq!(MAX_RECOVERIES_PER_PASS, 4);
        assert!(STALE_GRACE >= Duration::from_secs(1));
        assert!(RECOVERY_RETRY_DELAY >= Duration::from_secs(1));
        assert!(RECOVERY_ATTEMPT_DEADLINE <= Duration::from_secs(60));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn publication_requires_matching_forward_and_reverse_exercise_ownership() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("exercise_recovery_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = crate::migrations::test_pg_connect_options(&database_url)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Containers" (
                id UUID PRIMARY KEY, container_id TEXT NOT NULL,
                status SMALLINT NOT NULL, is_proxy BOOLEAN NOT NULL,
                ip TEXT NOT NULL, port INTEGER NOT NULL,
                public_ip TEXT, public_port INTEGER,
                exercise_instance_id INTEGER,
                expect_stop_at TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "ExerciseInstances" (
                id INTEGER PRIMARY KEY, user_id UUID NOT NULL,
                exercise_id INTEGER NOT NULL, is_loaded BOOLEAN NOT NULL,
                container_id UUID, flag_id INTEGER
            );
            CREATE TABLE "ExerciseContainerOperations" (
                operation_id UUID PRIMARY KEY, publication_id UUID NOT NULL,
                state TEXT NOT NULL, result JSONB,
                updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                lease_expires_at_utc TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let publication_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Containers"
                   (id, container_id, status, is_proxy, ip, port,
                    public_ip, public_port, exercise_instance_id, expect_stop_at)
               VALUES ($1, 'backend-1', $2, FALSE, '127.0.0.1', 31337,
                       NULL, NULL, 41, clock_timestamp() + interval '10 minutes')"#,
        )
        .bind(publication_id)
        .bind(ContainerStatus::Running as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "ExerciseInstances"
                   (id, user_id, exercise_id, is_loaded, container_id, flag_id)
               VALUES (41, $1, 7, TRUE, $2, NULL)"#,
        )
        .bind(user_id)
        .bind(publication_id)
        .execute(&pool)
        .await
        .unwrap();
        let claim = ReconciliationClaim {
            operation_id: Uuid::new_v4(),
            publication_id,
            user_id,
            exercise_id: 7,
            backend_id: Some("backend-1".to_string()),
        };
        sqlx::query(
            r#"INSERT INTO "ExerciseContainerOperations"
                   (operation_id, publication_id, state, result, lease_expires_at_utc)
               VALUES ($1, $2, 'Running', NULL,
                       clock_timestamp() + interval '3 minutes')"#,
        )
        .bind(claim.operation_id)
        .bind(publication_id)
        .execute(&pool)
        .await
        .unwrap();
        let operation = ClaimedOperation {
            operation_id: claim.operation_id,
            publication_id,
        };
        let container = load_exact_publication(&pool, &claim)
            .await
            .unwrap()
            .unwrap();
        assert!(
            complete_exact_publication(&pool, &claim, &operation, &container)
                .await
                .unwrap()
        );
        let succeeded = sqlx::query_as::<_, (String, Option<serde_json::Value>)>(
            r#"SELECT state, result FROM "ExerciseContainerOperations"
                WHERE operation_id = $1"#,
        )
        .bind(claim.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(succeeded.0, "Succeeded");
        assert_eq!(succeeded.1, Some(serde_json::json!("127.0.0.1:31337")));

        sqlx::query(
            r#"UPDATE "ExerciseContainerOperations"
                  SET state = 'Running', result = NULL
                WHERE operation_id = $1"#,
        )
        .bind(claim.operation_id)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(r#"UPDATE "ExerciseInstances" SET container_id = NULL WHERE id = 41"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(load_exact_publication(&pool, &claim)
            .await
            .unwrap()
            .is_none());
        assert!(
            !complete_exact_publication(&pool, &claim, &operation, &container)
                .await
                .unwrap()
        );

        sqlx::query(r#"UPDATE "ExerciseInstances" SET container_id = $1 WHERE id = 41"#)
            .bind(publication_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"UPDATE "Containers" SET container_id = 'backend-2' WHERE id = $1"#)
            .bind(publication_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !complete_exact_publication(&pool, &claim, &operation, &container)
                .await
                .unwrap()
        );
        sqlx::query(
            r#"UPDATE "Containers" SET container_id = 'backend-1', status = $2 WHERE id = $1"#,
        )
        .bind(publication_id)
        .bind(ContainerStatus::Destroyed as i16)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            !complete_exact_publication(&pool, &claim, &operation, &container)
                .await
                .unwrap()
        );
        sqlx::query(
            r#"UPDATE "Containers"
                  SET status = $2,
                      expect_stop_at = clock_timestamp() - interval '1 second'
                WHERE id = $1"#,
        )
        .bind(publication_id)
        .bind(ContainerStatus::Running as i16)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            !complete_exact_publication(&pool, &claim, &operation, &container)
                .await
                .unwrap()
        );
        sqlx::query(
            r#"UPDATE "Containers"
                  SET status = $2, exercise_instance_id = 42,
                      expect_stop_at = clock_timestamp() + interval '10 minutes'
                WHERE id = $1"#,
        )
        .bind(publication_id)
        .bind(ContainerStatus::Running as i16)
        .execute(&pool)
        .await
        .unwrap();
        assert!(load_exact_publication(&pool, &claim)
            .await
            .unwrap()
            .is_none());
        assert!(
            !complete_exact_publication(&pool, &claim, &operation, &container)
                .await
                .unwrap()
        );
        let failed_closed = sqlx::query_as::<_, (String, Option<serde_json::Value>)>(
            r#"SELECT state, result FROM "ExerciseContainerOperations"
                WHERE operation_id = $1"#,
        )
        .bind(claim.operation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(failed_closed, ("Running".to_string(), None));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
