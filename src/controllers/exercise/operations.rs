//! Durable admission and replay receipts for legacy exercise container work.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::utils::enums::ContainerStatus;
use crate::utils::error::{AppError, AppResult};

mod recovery;
pub(crate) use recovery::sweep;

const CLAIM_LOCK: &str = "rsctf:exercise-container-operation-admission";
const MAX_DEPLOYMENT_OPERATIONS: i64 = 16;
const MAX_LOCAL_OPERATIONS: usize = 4;
const OPERATION_DEADLINE: Duration = Duration::from_secs(120);
const RESULT_WAIT_DEADLINE: Duration = Duration::from_secs(125);
const MAX_LOCAL_RESULT_KEYS: usize = 128;
const MANAGED_REAP_PENDING_SQL: &str = r#"SELECT EXISTS(
       SELECT 1
         FROM "ManagedContainerReapOperations" reap
         JOIN "Containers" container
           ON container.id = reap.container_id
          AND container.container_id = reap.backend_id
        WHERE reap.scope_key = $1
           OR ($2::uuid IS NOT NULL
               AND container.id = $2
               AND container.container_id = reap.backend_id)
)"#;

static LOCAL_ADMISSION: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(MAX_LOCAL_OPERATIONS)));
static RESULT_FLIGHT: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<serde_json::Value>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Intent {
    Create,
    Delete,
}

impl Intent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Delete => "Delete",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ClaimedOperation {
    pub operation_id: Uuid,
    pub publication_id: Uuid,
}

pub(super) struct OperationRequest {
    pub(super) operation_id: Uuid,
    pub(super) may_adopt_stale: bool,
}

#[derive(Debug)]
pub(super) enum ClaimOutcome<T> {
    Owned(ClaimedOperation),
    Recovered(T),
    Following,
}

#[derive(sqlx::FromRow)]
struct OperationRow {
    user_id: Uuid,
    exercise_id: i32,
    intent: String,
    publication_id: Uuid,
    state: String,
    result: Option<serde_json::Value>,
    lease_active: bool,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub(super) fn operation_request(headers: &HeaderMap) -> AppResult<OperationRequest> {
    let Some(value) = headers.get("x-rsctf-operation-id") else {
        return Ok(OperationRequest {
            operation_id: Uuid::new_v4(),
            // A legacy caller cannot resend a server-generated identity. If
            // its process died, let its next request resume a stale operation
            // for this exact exercise and intent instead of blocking forever.
            may_adopt_stale: true,
        });
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::bad_request("Invalid exercise container operation ID"))?;
    let operation_id = Uuid::parse_str(value)
        .map_err(|_| AppError::bad_request("Invalid exercise container operation ID"))?;
    if operation_id.is_nil() {
        return Err(AppError::bad_request(
            "Exercise container operation ID must be opaque",
        ));
    }
    Ok(OperationRequest {
        operation_id,
        may_adopt_stale: false,
    })
}

fn validate_identity(
    row: &OperationRow,
    user_id: Uuid,
    exercise_id: i32,
    intent: Intent,
) -> AppResult<()> {
    if row.user_id != user_id || row.exercise_id != exercise_id || row.intent != intent.as_str() {
        return Err(AppError::conflict(
            "Exercise container operation ID was reused for another intent",
        ));
    }
    Ok(())
}

fn recovered<T: DeserializeOwned>(row: &mut OperationRow) -> AppResult<T> {
    serde_json::from_value(
        row.result
            .take()
            .ok_or_else(|| AppError::internal("exercise operation receipt is missing"))?,
    )
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn active_count(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> AppResult<i64> {
    sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "ExerciseContainerOperations"
            WHERE state = 'Running' AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)
}

async fn claim<T: DeserializeOwned>(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    user_id: Uuid,
    exercise_id: i32,
    intent: Intent,
    expected_publication_id: Option<Uuid>,
    may_adopt_stale: bool,
) -> AppResult<ClaimOutcome<T>> {
    let mut transaction = match tokio::time::timeout(
        Duration::from_millis(250),
        crate::utils::database::begin_sqlx_transaction(pool),
    )
    .await
    {
        Ok(result) => result.map_err(database_error)?,
        Err(_) => {
            return Err(AppError::overloaded(
                "Exercise operation capacity is busy",
                2,
            ))
        }
    };
    let admitted =
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(CLAIM_LOCK)
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
    if !admitted {
        transaction.rollback().await.map_err(database_error)?;
        return Err(AppError::too_many_requests(2));
    }
    let runtime_lock_key = format!("exercise-container:{user_id}:{exercise_id}");
    let runtime_available = crate::utils::single_flight::try_acquire_transaction_advisory_lock(
        &mut transaction,
        &runtime_lock_key,
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !runtime_available {
        transaction.rollback().await.map_err(database_error)?;
        return Err(AppError::too_many_requests(2));
    }
    // Read the reaper's durable marker only after taking the same owner lock;
    // this closes its commit/release hand-off without spanning runtime I/O.
    let teardown_pending: bool = sqlx::query_scalar(MANAGED_REAP_PENDING_SQL)
        .bind(&runtime_lock_key)
        .bind(expected_publication_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
    if teardown_pending {
        transaction.rollback().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "Container teardown is being reconciled",
            2,
        ));
    }
    sqlx::query(
        r#"WITH expired AS (
               SELECT operation_id
                 FROM "ExerciseContainerOperations"
                WHERE state <> 'Running'
                  AND updated_at_utc < clock_timestamp() - interval '24 hours'
                ORDER BY updated_at_utc, operation_id
                LIMIT 64
           )
           DELETE FROM "ExerciseContainerOperations" operation
            USING expired
            WHERE operation.operation_id = expired.operation_id"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;

    let existing = sqlx::query_as::<_, OperationRow>(
        r#"SELECT user_id, exercise_id, intent, publication_id, state, result,
                  lease_expires_at_utc > clock_timestamp() AS lease_active
             FROM "ExerciseContainerOperations"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    if let Some(mut row) = existing {
        if let Err(error) = validate_identity(&row, user_id, exercise_id, intent) {
            transaction.rollback().await.map_err(database_error)?;
            return Err(error);
        }
        if expected_publication_id.is_some_and(|expected| expected != row.publication_id) {
            transaction.rollback().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "Exercise container operation targets another runtime",
            ));
        }
        if row.state == "Succeeded" {
            let still_published = if intent == Intent::Create {
                sqlx::query_scalar::<_, bool>(
                    r#"SELECT EXISTS(
                           SELECT 1
                             FROM "Containers" container
                             JOIN "ExerciseInstances" instance
                               ON instance.container_id = container.id
                            WHERE container.id = $1 AND instance.user_id = $2
                              AND instance.exercise_id = $3
                              AND instance.is_loaded = TRUE
                              AND container.status = $4
                       )"#,
                )
                .bind(row.publication_id)
                .bind(user_id)
                .bind(exercise_id)
                .bind(ContainerStatus::Running as i16)
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?
            } else {
                true
            };
            if still_published {
                let result = recovered(&mut row)?;
                transaction.commit().await.map_err(database_error)?;
                return Ok(ClaimOutcome::Recovered(result));
            }
            sqlx::query(
                r#"UPDATE "ExerciseContainerOperations"
                      SET state = 'Failed', result = NULL,
                          updated_at_utc = clock_timestamp(),
                          lease_expires_at_utc = clock_timestamp()
                    WHERE operation_id = $1 AND state = 'Succeeded'"#,
            )
            .bind(operation_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            row.state = "Failed".to_string();
        }
        if row.state == "Running" && row.lease_active {
            transaction.commit().await.map_err(database_error)?;
            return Ok(ClaimOutcome::Following);
        }
        if active_count(&mut transaction).await? >= MAX_DEPLOYMENT_OPERATIONS {
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::overloaded(
                "Exercise container capacity is busy",
                2,
            ));
        }
        let reclaimed = sqlx::query(
            r#"UPDATE "ExerciseContainerOperations"
                  SET state = 'Running', result = NULL,
                      updated_at_utc = clock_timestamp(),
                      lease_expires_at_utc = clock_timestamp() + interval '3 minutes',
                      runtime_started = CASE WHEN state = 'Failed' THEN FALSE
                                             ELSE runtime_started END,
                      backend_id = CASE WHEN state = 'Failed' THEN NULL
                                        ELSE backend_id END
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .execute(&mut *transaction)
        .await;
        match reclaimed {
            Ok(_) => {}
            Err(error) if crate::utils::error::is_unique_violation(&error) => {
                transaction.rollback().await.map_err(database_error)?;
                return Err(AppError::too_many_requests(2));
            }
            Err(error) => return Err(database_error(error)),
        }
        transaction.commit().await.map_err(database_error)?;
        return Ok(ClaimOutcome::Owned(ClaimedOperation {
            operation_id,
            publication_id: row.publication_id,
        }));
    }

    let competing = sqlx::query_as::<_, (Uuid, i32, String, Uuid, bool, bool)>(
        r#"SELECT operation_id, exercise_id, intent, publication_id,
                  lease_expires_at_utc > clock_timestamp() AS lease_active,
                  runtime_started
             FROM "ExerciseContainerOperations"
            WHERE user_id = $1 AND state = 'Running'
            LIMIT 1"#,
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    if let Some((
        stale_id,
        stale_exercise_id,
        stale_intent,
        publication_id,
        lease_active,
        runtime_started,
    )) = competing
    {
        if lease_active {
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::too_many_requests(2));
        }
        let same_target = stale_exercise_id == exercise_id
            && stale_intent == intent.as_str()
            && expected_publication_id.is_none_or(|expected| expected == publication_id);
        if !runtime_started {
            sqlx::query(
                r#"UPDATE "ExerciseContainerOperations"
                      SET state = 'Failed', result = NULL,
                          lease_expires_at_utc = clock_timestamp(),
                          updated_at_utc = clock_timestamp()
                    WHERE operation_id = $1 AND state = 'Running'"#,
            )
            .bind(stale_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        } else if may_adopt_stale && same_target {
            if active_count(&mut transaction).await? >= MAX_DEPLOYMENT_OPERATIONS {
                transaction.commit().await.map_err(database_error)?;
                return Err(AppError::overloaded(
                    "Exercise container capacity is busy",
                    2,
                ));
            }
            let adopted = sqlx::query(
                r#"UPDATE "ExerciseContainerOperations"
                      SET result = NULL, updated_at_utc = clock_timestamp(),
                          lease_expires_at_utc = clock_timestamp() + interval '3 minutes'
                    WHERE operation_id = $1 AND state = 'Running'
                      AND lease_expires_at_utc <= clock_timestamp()"#,
            )
            .bind(stale_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
            if adopted.rows_affected() != 1 {
                transaction.rollback().await.map_err(database_error)?;
                return Err(AppError::conflict(
                    "Exercise container operation changed; retry",
                ));
            }
            transaction.commit().await.map_err(database_error)?;
            return Ok(ClaimOutcome::Owned(ClaimedOperation {
                operation_id: stale_id,
                publication_id,
            }));
        } else {
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "Retry the original exercise container operation ID so its runtime can be reconciled",
            ));
        }
    }

    let active = active_count(&mut transaction).await?;
    if active >= MAX_DEPLOYMENT_OPERATIONS {
        transaction.commit().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "Exercise container capacity is busy",
            2,
        ));
    }

    let publication_id = match (intent, expected_publication_id) {
        (Intent::Create, publication_id) => publication_id.unwrap_or_else(Uuid::new_v4),
        (Intent::Delete, Some(publication_id)) => publication_id,
        (Intent::Delete, None) => {
            transaction.commit().await.map_err(database_error)?;
            return Err(AppError::not_found("No instance"));
        }
    };
    let inserted = sqlx::query(
        r#"INSERT INTO "ExerciseContainerOperations"
               (operation_id, user_id, exercise_id, intent, publication_id,
                state, lease_expires_at_utc)
           VALUES ($1, $2, $3, $4, $5, 'Running',
                   clock_timestamp() + interval '3 minutes')
           ON CONFLICT (operation_id) DO NOTHING"#,
    )
    .bind(operation_id)
    .bind(user_id)
    .bind(exercise_id)
    .bind(intent.as_str())
    .bind(publication_id)
    .execute(&mut *transaction)
    .await;
    match inserted {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => {
            transaction.rollback().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "Exercise container operation changed; retry",
            ));
        }
        Err(error) if crate::utils::error::is_unique_violation(&error) => {
            transaction.rollback().await.map_err(database_error)?;
            return Err(AppError::too_many_requests(2));
        }
        Err(error) => return Err(database_error(error)),
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(ClaimOutcome::Owned(ClaimedOperation {
        operation_id,
        publication_id,
    }))
}

pub(super) async fn claim_create(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    user_id: Uuid,
    exercise_id: i32,
    expected_publication_id: Option<Uuid>,
    may_adopt_stale: bool,
) -> AppResult<ClaimOutcome<String>> {
    claim(
        pool,
        operation_id,
        user_id,
        exercise_id,
        Intent::Create,
        expected_publication_id,
        may_adopt_stale,
    )
    .await
}

pub(super) async fn claim_delete(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    user_id: Uuid,
    exercise_id: i32,
    expected_publication_id: Option<Uuid>,
    may_adopt_stale: bool,
) -> AppResult<ClaimOutcome<()>> {
    claim(
        pool,
        operation_id,
        user_id,
        exercise_id,
        Intent::Delete,
        expected_publication_id,
        may_adopt_stale,
    )
    .await
}

pub(super) async fn mark_runtime_started(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "ExerciseContainerOperations"
              SET runtime_started = TRUE, updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Exercise operation ownership changed before runtime launch",
        ));
    }
    Ok(())
}

pub(super) async fn record_backend(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
    backend_id: &str,
) -> AppResult<()> {
    if backend_id.is_empty() || backend_id.len() > 512 {
        return Err(AppError::internal(
            "exercise backend identity exceeds its durable bound",
        ));
    }
    let stored = sqlx::query_scalar::<_, String>(
        r#"UPDATE "ExerciseContainerOperations"
              SET backend_id = COALESCE(backend_id, $3),
                  updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2
              AND state = 'Running' AND runtime_started = TRUE
        RETURNING backend_id"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .bind(backend_id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::conflict("Exercise operation ownership changed after launch"))?;
    if stored != backend_id {
        return Err(AppError::conflict(
            "Exercise operation resolved to another backend runtime",
        ));
    }
    Ok(())
}

async fn complete<T: Serialize>(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
    result: &T,
) -> AppResult<()> {
    let result =
        serde_json::to_value(result).map_err(|error| AppError::internal(error.to_string()))?;
    let updated = sqlx::query(
        r#"UPDATE "ExerciseContainerOperations"
              SET state = 'Succeeded', result = $2,
                  updated_at_utc = clock_timestamp(),
                  lease_expires_at_utc = clock_timestamp() + interval '24 hours'
            WHERE operation_id = $1 AND publication_id = $3 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(&result)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Exercise container operation ownership changed before completion",
        ));
    }
    Ok(())
}

async fn settle_failed_work(pool: &sqlx::PgPool, operation: &ClaimedOperation) {
    let terminalized = match sqlx::query(
        r#"UPDATE "ExerciseContainerOperations"
              SET state = 'Failed', result = NULL,
                  updated_at_utc = clock_timestamp(),
                  lease_expires_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2
              AND state = 'Running' AND runtime_started = FALSE"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    {
        Ok(result) => result.rows_affected() == 1,
        Err(error) => {
            tracing::warn!(%error, operation_id = %operation.operation_id, "failed to terminalize pre-launch exercise operation");
            false
        }
    };
    if terminalized {
        return;
    }
    if let Err(error) = sqlx::query(
        r#"UPDATE "ExerciseContainerOperations"
              SET updated_at_utc = clock_timestamp(),
                  lease_expires_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    {
        tracing::warn!(%error, operation_id = %operation.operation_id, "failed to release ambiguous exercise operation");
    }
}

pub(super) fn spawn_owner<T, F>(
    pool: sqlx::PgPool,
    operation: ClaimedOperation,
    work: F,
) -> tokio::task::JoinHandle<AppResult<T>>
where
    T: Serialize + Send + Sync + 'static,
    F: Future<Output = AppResult<T>> + Send + 'static,
{
    tokio::spawn(async move {
        let permit: OwnedSemaphorePermit = match LOCAL_ADMISSION.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                settle_failed_work(&pool, &operation).await;
                return Err(AppError::overloaded(
                    "Local exercise container capacity is busy",
                    2,
                ));
            }
        };
        let result = tokio::time::timeout(OPERATION_DEADLINE, work).await;
        drop(permit);
        match result {
            Ok(Ok(value)) => {
                complete(&pool, &operation, &value).await?;
                Ok(value)
            }
            Ok(Err(error)) => {
                settle_failed_work(&pool, &operation).await;
                Err(error)
            }
            Err(_) => {
                settle_failed_work(&pool, &operation).await;
                Err(AppError::overloaded(
                    "Exercise container operation timed out",
                    2,
                ))
            }
        }
    })
}

pub(super) async fn await_owner<T>(owner: tokio::task::JoinHandle<AppResult<T>>) -> AppResult<T> {
    owner.await.map_err(|error| {
        AppError::internal(format!(
            "exercise container operation owner failed: {error}"
        ))
    })?
}

pub(super) async fn wait_for_result<T: DeserializeOwned>(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
) -> AppResult<T> {
    let key = format!("exercise-container-operation:{operation_id}");
    let pool = pool.clone();
    let result = RESULT_FLIGHT
        .run_with_limit(
            &key,
            RESULT_WAIT_DEADLINE,
            MAX_LOCAL_RESULT_KEYS,
            move || async move {
                let deadline = tokio::time::Instant::now() + RESULT_WAIT_DEADLINE;
                let mut delay = Duration::from_millis(100);
                loop {
                    let row = sqlx::query_as::<_, (String, Option<serde_json::Value>, bool)>(
                        r#"SELECT state, result,
                                  lease_expires_at_utc > clock_timestamp() AS lease_active
                             FROM "ExerciseContainerOperations"
                            WHERE operation_id = $1"#,
                    )
                    .bind(operation_id)
                    .fetch_optional(&pool)
                    .await
                    .ok()
                    .flatten();
                    match row {
                        Some((state, result, _)) if state == "Succeeded" => return result,
                        Some((state, _, _)) if state == "Failed" => return None,
                        Some((state, _, false)) if state == "Running" => return None,
                        None => return None,
                        _ if tokio::time::Instant::now() >= deadline => return None,
                        _ => {
                            tokio::time::sleep(delay).await;
                            delay = (delay * 2).min(Duration::from_secs(1));
                        }
                    }
                }
            },
        )
        .await
        .ok_or_else(|| AppError::overloaded("Exercise container result is not ready", 2))?;
    serde_json::from_value(result).map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_operation_identity_is_validated() {
        let mut headers = HeaderMap::new();
        headers.insert("x-rsctf-operation-id", "not-a-uuid".parse().unwrap());
        assert!(operation_request(&headers).is_err());
        headers.insert(
            "x-rsctf-operation-id",
            Uuid::nil().to_string().parse().unwrap(),
        );
        assert!(operation_request(&headers).is_err());

        let legacy = operation_request(&HeaderMap::new()).unwrap();
        assert!(legacy.may_adopt_stale);
        assert!(!legacy.operation_id.is_nil());
    }

    #[test]
    fn operation_admission_is_bounded() {
        assert!((1..=32).contains(&MAX_DEPLOYMENT_OPERATIONS));
        assert!((1..=8).contains(&MAX_LOCAL_OPERATIONS));
        assert!(OPERATION_DEADLINE <= Duration::from_secs(5 * 60));
        assert!((1..=256).contains(&MAX_LOCAL_RESULT_KEYS));
    }

    #[test]
    fn exercise_claim_fences_exact_durable_reap_markers() {
        assert!(MANAGED_REAP_PENDING_SQL.contains("reap.scope_key = $1"));
        assert!(MANAGED_REAP_PENDING_SQL.contains("container.id = reap.container_id"));
        assert!(MANAGED_REAP_PENDING_SQL.contains("container.container_id = reap.backend_id"));
        assert!(MANAGED_REAP_PENDING_SQL.contains("container.id = $2"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn exact_replay_and_competing_intent_use_one_pool_connection() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("exercise_operations_{}", Uuid::new_v4().simple());
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
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
            CREATE TABLE "ExerciseChallenges" (id INTEGER PRIMARY KEY);
            CREATE TABLE "Containers" (
                id UUID PRIMARY KEY, status SMALLINT NOT NULL,
                container_id TEXT NOT NULL
            );
            CREATE TABLE "ExerciseInstances" (
                user_id UUID NOT NULL, exercise_id INTEGER NOT NULL,
                container_id UUID, is_loaded BOOLEAN NOT NULL
            );
            CREATE TABLE "ManagedContainerReapOperations" (
                backend_id TEXT PRIMARY KEY, container_id UUID NOT NULL UNIQUE,
                scope_key TEXT NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::EXERCISE_CONTAINER_OPERATIONS_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::EXERCISE_OPERATION_RECOVERY_SQL)
            .execute(&pool)
            .await
            .unwrap();
        let user_id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "ExerciseChallenges" VALUES (7)"#)
            .execute(&pool)
            .await
            .unwrap();

        let reaping_container = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Containers" (id, status, container_id)
               VALUES ($1, $2, 'backend-reaping')"#,
        )
        .bind(reaping_container)
        .bind(ContainerStatus::Running as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "ManagedContainerReapOperations"
                   (backend_id, container_id, scope_key)
               VALUES ('backend-reaping', $1, $2)"#,
        )
        .bind(reaping_container)
        .bind(format!("exercise-container:{user_id}:7"))
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            claim_create(&pool, Uuid::new_v4(), user_id, 7, None, false)
                .await
                .unwrap_err(),
            AppError::RetryableUnavailable { .. }
        ));
        sqlx::query(
            r#"UPDATE "ManagedContainerReapOperations"
                  SET scope_key = 'exercise-container:another-scope'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            claim_create(
                &pool,
                Uuid::new_v4(),
                user_id,
                7,
                Some(reaping_container),
                false,
            )
            .await
            .unwrap_err(),
            AppError::RetryableUnavailable { .. }
        ));
        sqlx::query(r#"DELETE FROM "ManagedContainerReapOperations""#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"DELETE FROM "Containers" WHERE id = $1"#)
            .bind(reaping_container)
            .execute(&pool)
            .await
            .unwrap();

        let operation_id = Uuid::new_v4();
        let operation = match claim_create(&pool, operation_id, user_id, 7, None, false)
            .await
            .unwrap()
        {
            ClaimOutcome::Owned(operation) => operation,
            _ => panic!("first request must own the operation"),
        };
        assert!(matches!(
            claim_create(&pool, operation_id, user_id, 7, None, false)
                .await
                .unwrap(),
            ClaimOutcome::Following
        ));
        let competing = claim_delete(
            &pool,
            Uuid::new_v4(),
            user_id,
            7,
            Some(operation.publication_id),
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(competing, AppError::TooManyRequests { .. }));

        sqlx::query(
            r#"INSERT INTO "Containers" (id, status, container_id)
               VALUES ($1, $2, 'backend-live')"#,
        )
        .bind(operation.publication_id)
        .bind(ContainerStatus::Running as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "ExerciseInstances" VALUES ($1, 7, $2, TRUE)"#)
            .bind(user_id)
            .bind(operation.publication_id)
            .execute(&pool)
            .await
            .unwrap();
        complete(&pool, &operation, &"127.0.0.1:31337".to_string())
            .await
            .unwrap();
        let replay = claim_create(
            &pool,
            operation_id,
            user_id,
            7,
            Some(operation.publication_id),
            false,
        )
        .await
        .unwrap();
        assert!(matches!(
            replay,
            ClaimOutcome::Recovered(ref entry) if entry == "127.0.0.1:31337"
        ));
        assert!(matches!(
            claim_delete(
                &pool,
                operation_id,
                user_id,
                7,
                Some(operation.publication_id),
                false,
            )
            .await
            .unwrap_err(),
            AppError::Conflict(_)
        ));

        sqlx::query(r#"UPDATE "Containers" SET status = $2 WHERE id = $1"#)
            .bind(operation.publication_id)
            .bind(ContainerStatus::Destroyed as i16)
            .execute(&pool)
            .await
            .unwrap();
        let reclaimed = claim_create(
            &pool,
            operation_id,
            user_id,
            7,
            Some(operation.publication_id),
            false,
        )
        .await
        .unwrap();
        assert!(matches!(
            reclaimed,
            ClaimOutcome::Owned(ref reclaimed_operation)
                if reclaimed_operation.publication_id == operation.publication_id
        ));
        sqlx::query(
            r#"UPDATE "ExerciseContainerOperations"
                  SET lease_expires_at_utc = clock_timestamp() - interval '1 second'
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .execute(&pool)
        .await
        .unwrap();
        let adopted = claim_create(
            &pool,
            Uuid::new_v4(),
            user_id,
            7,
            Some(operation.publication_id),
            true,
        )
        .await
        .unwrap();
        assert!(matches!(
            adopted,
            ClaimOutcome::Owned(ref adopted_operation)
                if adopted_operation.operation_id == operation_id
                    && adopted_operation.publication_id == operation.publication_id
        ));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
