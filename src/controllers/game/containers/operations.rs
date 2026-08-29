//! Durable admission, detached ownership, and replay receipts for player
//! container lifecycle work.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::*;

const CLAIM_LOCK: &str = "rsctf:player-container-operation-admission";
const MAX_DEPLOYMENT_OPERATIONS: i64 = 32;
const MAX_LOCAL_OPERATIONS: usize = 4;
const OPERATION_DEADLINE: Duration = Duration::from_secs(120);
const RESULT_WAIT_DEADLINE: Duration = Duration::from_secs(125);
const MAX_LOCAL_RESULT_KEYS: usize = 256;

static LOCAL_OWNER_ADMISSION: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(MAX_LOCAL_OPERATIONS)));
static RESULT_FLIGHT: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<serde_json::Value>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Debug)]
pub(crate) struct ClaimedOperation {
    pub operation_id: Uuid,
    pub publication_id: Uuid,
}

pub(crate) enum ClaimOutcome<T> {
    Owned(ClaimedOperation),
    Recovered(T),
    Following,
}

#[derive(Clone, Copy)]
enum Intent {
    Create,
    Delete,
    Extend,
}

impl Intent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Delete => "Delete",
            Self::Extend => "Extend",
        }
    }
}

#[derive(sqlx::FromRow)]
struct OperationRow {
    operation_id: Uuid,
    scope_key: String,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
    intent: String,
    publication_id: Uuid,
    state: String,
    result: Option<serde_json::Value>,
    lease_active: bool,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

struct ExpectedOperationIdentity<'a> {
    scope_key: &'a str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
    intent: Intent,
    expected_publication_id: Option<Uuid>,
}

fn validate_identity(
    row: &OperationRow,
    expected: &ExpectedOperationIdentity<'_>,
) -> AppResult<()> {
    if row.scope_key != expected.scope_key
        || row.actor_user_id != expected.actor_user_id
        || row.game_id != expected.game_id
        || row.participation_id != expected.participation_id
        || row.challenge_id != expected.challenge_id
        || row.intent != expected.intent.as_str()
        || expected
            .expected_publication_id
            .is_some_and(|publication_id| publication_id != row.publication_id)
    {
        return Err(AppError::conflict(
            "Container operation identity was reused for another intent",
        ));
    }
    Ok(())
}

async fn active_count(transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> AppResult<i64> {
    sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "PlayerContainerOperations"
            WHERE state = 'Running' AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)
}

fn recovered<T: DeserializeOwned>(row: &mut OperationRow) -> AppResult<T> {
    serde_json::from_value(
        row.result
            .take()
            .ok_or_else(|| AppError::internal("container operation receipt is missing"))?,
    )
    .map_err(|error| AppError::internal(error.to_string()))
}

#[allow(clippy::too_many_arguments)]
async fn claim<T: DeserializeOwned>(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    scope_key: &str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
    intent: Intent,
    expected_publication_id: Option<Uuid>,
) -> AppResult<ClaimOutcome<T>> {
    let mut tx = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(CLAIM_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    let existing = sqlx::query_as::<_, OperationRow>(
        r#"SELECT operation_id, scope_key, actor_user_id, game_id, participation_id, challenge_id,
                  intent, publication_id, state, result,
                  lease_expires_at_utc > clock_timestamp() AS lease_active
             FROM "PlayerContainerOperations"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    if let Some(mut row) = existing {
        validate_identity(
            &row,
            &ExpectedOperationIdentity {
                scope_key,
                actor_user_id,
                game_id,
                participation_id,
                challenge_id,
                intent,
                expected_publication_id,
            },
        )?;
        if row.state == "Succeeded" {
            if matches!(intent, Intent::Create) {
                let still_published: bool = sqlx::query_scalar(
                    r#"SELECT EXISTS(
                           SELECT 1 FROM "Containers"
                            WHERE id = $1 AND status = $2
                       )"#,
                )
                .bind(row.publication_id)
                .bind(ContainerStatus::Running as i16)
                .fetch_one(&mut *tx)
                .await
                .map_err(database_error)?;
                if !still_published {
                    row.state = "Failed".to_string();
                }
            }
            if row.state == "Succeeded" {
                let result = recovered(&mut row)?;
                tx.commit().await.map_err(database_error)?;
                return Ok(ClaimOutcome::Recovered(result));
            }
        }
        if row.state == "Running" && row.lease_active {
            tx.commit().await.map_err(database_error)?;
            return Ok(ClaimOutcome::Following);
        }
        if active_count(&mut tx).await? >= MAX_DEPLOYMENT_OPERATIONS {
            tx.commit().await.map_err(database_error)?;
            return Err(AppError::overloaded(
                "Container provisioning capacity is busy",
                2,
            ));
        }
        let reclaimed = sqlx::query(
            r#"UPDATE "PlayerContainerOperations"
                  SET state = 'Running', result = NULL,
                      updated_at_utc = clock_timestamp(),
                      lease_expires_at_utc = clock_timestamp() + interval '3 minutes'
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .execute(&mut *tx)
        .await;
        match reclaimed {
            Ok(_) => {}
            Err(error) if crate::utils::error::is_unique_violation(&error) => {
                tx.rollback().await.map_err(database_error)?;
                return Err(AppError::overloaded(
                    "Another container operation is active for this team",
                    2,
                ));
            }
            Err(error) => return Err(database_error(error)),
        }
        tx.commit().await.map_err(database_error)?;
        return Ok(ClaimOutcome::Owned(ClaimedOperation {
            operation_id,
            publication_id: row.publication_id,
        }));
    }

    // Shared create results are challenge-owned rather than actor-owned. Once
    // the first caller published a still-running endpoint, later authorized
    // callers can reuse that durable receipt without runtime inspection or a
    // new operation row. Per-team creates retain exact-operation semantics.
    if matches!(intent, Intent::Create) && participation_id.is_none() {
        let reusable = sqlx::query_as::<_, (serde_json::Value,)>(
            r#"SELECT operation.result
                 FROM "PlayerContainerOperations" operation
                 JOIN "Containers" container ON container.id = operation.publication_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = operation.challenge_id
                  AND challenge.shared_container_id = container.id
                WHERE operation.scope_key = $1 AND operation.game_id = $2
                  AND operation.challenge_id = $3 AND operation.intent = 'Create'
                  AND operation.state = 'Succeeded' AND container.status = $4
             ORDER BY operation.updated_at_utc DESC, operation.operation_id
                LIMIT 1"#,
        )
        .bind(scope_key)
        .bind(game_id)
        .bind(challenge_id)
        .bind(ContainerStatus::Running as i16)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        if let Some((result,)) = reusable {
            let result = serde_json::from_value(result)
                .map_err(|error| AppError::internal(error.to_string()))?;
            tx.commit().await.map_err(database_error)?;
            return Ok(ClaimOutcome::Recovered(result));
        }
    }

    let stale_scope = sqlx::query_as::<_, OperationRow>(
        r#"SELECT operation_id, scope_key, actor_user_id, game_id, participation_id, challenge_id,
                  intent, publication_id, state, result,
                  lease_expires_at_utc > clock_timestamp() AS lease_active
             FROM "PlayerContainerOperations"
            WHERE scope_key = $1 AND state = 'Running'
            FOR UPDATE"#,
    )
    .bind(scope_key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(database_error)?;
    if let Some(row) = stale_scope {
        if row.lease_active {
            tx.commit().await.map_err(database_error)?;
            return Err(AppError::overloaded(
                "Another container operation is active for this team",
                2,
            ));
        }
        if row.game_id != game_id
            || row.participation_id != participation_id
            || row.challenge_id != challenge_id
        {
            tx.commit().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "A stale container operation must be reconciled before changing intent",
            ));
        }
        if row.intent != intent.as_str() {
            if row.intent == Intent::Create.as_str() {
                tx.commit().await.map_err(database_error)?;
                return Err(AppError::conflict(
                    "A stale container create must be reconciled before changing intent",
                ));
            }
            // Extend commits its receipt atomically with the lease change;
            // delete is compare-and-swap/idempotent. An expired non-create
            // cannot hide an unowned newly launched runtime.
            sqlx::query(
                r#"UPDATE "PlayerContainerOperations"
                      SET state = 'Failed', result = NULL,
                          lease_expires_at_utc = clock_timestamp(),
                          updated_at_utc = clock_timestamp()
                    WHERE operation_id = $1 AND state = 'Running'"#,
            )
            .bind(row.operation_id)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
        } else {
            if expected_publication_id.is_some_and(|expected| expected != row.publication_id) {
                tx.commit().await.map_err(database_error)?;
                return Err(AppError::conflict(
                    "The stale container operation targets another runtime",
                ));
            }
            sqlx::query(
                r#"UPDATE "PlayerContainerOperations"
                      SET lease_expires_at_utc = clock_timestamp() + interval '3 minutes',
                          updated_at_utc = clock_timestamp()
                    WHERE operation_id = $1 AND state = 'Running'"#,
            )
            .bind(row.operation_id)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
            tx.commit().await.map_err(database_error)?;
            return Ok(ClaimOutcome::Owned(ClaimedOperation {
                operation_id: row.operation_id,
                publication_id: row.publication_id,
            }));
        }
    }

    if active_count(&mut tx).await? >= MAX_DEPLOYMENT_OPERATIONS {
        tx.commit().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "Container provisioning capacity is busy",
            2,
        ));
    }
    let publication_id = expected_publication_id.unwrap_or_else(Uuid::new_v4);
    let inserted = sqlx::query(
        r#"INSERT INTO "PlayerContainerOperations"
               (operation_id, scope_key, actor_user_id, game_id, participation_id,
                challenge_id, intent, publication_id, state, lease_expires_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'Running',
                   clock_timestamp() + interval '3 minutes')
           ON CONFLICT (operation_id) DO NOTHING"#,
    )
    .bind(operation_id)
    .bind(scope_key)
    .bind(actor_user_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
    .bind(intent.as_str())
    .bind(publication_id)
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => return Err(AppError::conflict("Container operation changed; retry")),
        Err(error) if crate::utils::error::is_unique_violation(&error) => {
            return Err(AppError::overloaded(
                "Another container operation is active for this team",
                2,
            ));
        }
        Err(error) => return Err(database_error(error)),
    }
    tx.commit().await.map_err(database_error)?;
    Ok(ClaimOutcome::Owned(ClaimedOperation {
        operation_id,
        publication_id,
    }))
}

pub(crate) async fn claim_create(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    scope_key: &str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
) -> AppResult<ClaimOutcome<ContainerInfoModel>> {
    claim(
        pool,
        operation_id,
        scope_key,
        actor_user_id,
        game_id,
        participation_id,
        challenge_id,
        Intent::Create,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn claim_delete(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    scope_key: &str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    expected_container_id: Uuid,
) -> AppResult<ClaimOutcome<()>> {
    claim(
        pool,
        operation_id,
        scope_key,
        actor_user_id,
        game_id,
        Some(participation_id),
        challenge_id,
        Intent::Delete,
        Some(expected_container_id),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn claim_extend(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    scope_key: &str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
    expected_container_id: Uuid,
) -> AppResult<ClaimOutcome<ContainerInfoModel>> {
    claim(
        pool,
        operation_id,
        scope_key,
        actor_user_id,
        game_id,
        participation_id,
        challenge_id,
        Intent::Extend,
        Some(expected_container_id),
    )
    .await
}

async fn complete<T: Serialize>(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
    result: &T,
) -> AppResult<()> {
    let result =
        serde_json::to_value(result).map_err(|error| AppError::internal(error.to_string()))?;
    let updated = sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
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
        let replay: Option<(String, Uuid, Option<serde_json::Value>)> = sqlx::query_as(
            r#"SELECT state, publication_id, result
                 FROM "PlayerContainerOperations" WHERE operation_id = $1"#,
        )
        .bind(operation.operation_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?;
        if !matches!(
            replay,
            Some((state, publication_id, Some(ref stored)))
                if state == "Succeeded"
                    && publication_id == operation.publication_id
                    && stored == &result
        ) {
            return Err(AppError::conflict(
                "Container operation ownership changed before publication",
            ));
        }
    }
    Ok(())
}

/// Commit a database-only lifecycle result with its durable receipt in the
/// same transaction. The detached owner later repeats `complete`, which is an
/// exact no-op replay of this receipt.
pub(crate) async fn complete_locked<T: Serialize>(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation: &ClaimedOperation,
    result: &T,
) -> AppResult<()> {
    let result =
        serde_json::to_value(result).map_err(|error| AppError::internal(error.to_string()))?;
    let updated = sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
              SET state = 'Succeeded', result = $2,
                  updated_at_utc = clock_timestamp(),
                  lease_expires_at_utc = clock_timestamp() + interval '24 hours'
            WHERE operation_id = $1 AND publication_id = $3 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(result)
    .bind(operation.publication_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Container operation ownership changed before publication",
        ));
    }
    Ok(())
}

async fn release_for_exact_retry(pool: &sqlx::PgPool, operation: &ClaimedOperation) {
    if let Err(error) = sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
              SET lease_expires_at_utc = clock_timestamp(),
                  updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    {
        tracing::warn!(operation_id = %operation.operation_id, %error, "failed to release container operation for exact retry");
    }
}

fn local_owner_permit() -> Option<OwnedSemaphorePermit> {
    LOCAL_OWNER_ADMISSION.clone().try_acquire_owned().ok()
}

/// Start an accepted operation in a detached, bounded owner. Dropping the HTTP
/// waiter never cancels the runtime mutation or its durable completion receipt.
pub(crate) fn spawn_owner<T, F>(
    pool: sqlx::PgPool,
    operation: ClaimedOperation,
    work: F,
) -> tokio::task::JoinHandle<AppResult<T>>
where
    T: Serialize + Send + Sync + 'static,
    F: Future<Output = AppResult<T>> + Send + 'static,
{
    let permit = local_owner_permit();
    tokio::spawn(async move {
        let Some(_permit) = permit else {
            release_for_exact_retry(&pool, &operation).await;
            return Err(AppError::overloaded(
                "Local container operation capacity is busy",
                2,
            ));
        };
        let result = match tokio::time::timeout(OPERATION_DEADLINE, work).await {
            Ok(result) => result,
            Err(_) => {
                release_for_exact_retry(&pool, &operation).await;
                return Err(AppError::overloaded("Container operation timed out", 2));
            }
        };
        match &result {
            Ok(value) => complete(&pool, &operation, value).await?,
            Err(_) => release_for_exact_retry(&pool, &operation).await,
        }
        result
    })
}

pub(crate) async fn await_owner<T>(owner: tokio::task::JoinHandle<AppResult<T>>) -> AppResult<T> {
    owner
        .await
        .map_err(|error| AppError::internal(format!("container operation owner failed: {error}")))?
}

pub(crate) async fn wait_for_result<T: DeserializeOwned>(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
) -> AppResult<T> {
    let key = format!("player-container-operation:{operation_id}");
    let pool = pool.clone();
    let result = RESULT_FLIGHT
        .run_with_limit(
            &key,
            RESULT_WAIT_DEADLINE,
            MAX_LOCAL_RESULT_KEYS,
            move || async move {
                let deadline = tokio::time::Instant::now() + RESULT_WAIT_DEADLINE;
                let mut poll_delay = Duration::from_millis(100);
                loop {
                    let row = sqlx::query_as::<_, (String, Option<serde_json::Value>, bool)>(
                        r#"SELECT state, result,
                                  lease_expires_at_utc > clock_timestamp() AS lease_active
                             FROM "PlayerContainerOperations"
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
                            tokio::time::sleep(poll_delay).await;
                            poll_delay = (poll_delay * 2).min(Duration::from_secs(1));
                        }
                    }
                }
            },
        )
        .await
        .ok_or_else(|| AppError::overloaded("Container operation result is not ready", 2))?;
    serde_json::from_value(result).map_err(|error| AppError::internal(error.to_string()))
}

pub(crate) async fn purge_terminal(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    sqlx::query(
        r#"WITH victims AS (
               SELECT operation_id FROM "PlayerContainerOperations"
                WHERE state <> 'Running'
                  AND updated_at_utc < clock_timestamp() - interval '24 hours'
                ORDER BY updated_at_utc, operation_id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
           )
           DELETE FROM "PlayerContainerOperations" operation
            USING victims WHERE operation.operation_id = victims.operation_id"#,
    )
    .bind(limit.clamp(1, 256))
    .execute(pool)
    .await
    .map(|result| result.rows_affected())
    .map_err(database_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_budgets_and_deadlines_are_finite() {
        assert!((1..=64).contains(&MAX_DEPLOYMENT_OPERATIONS));
        assert!((1..=16).contains(&MAX_LOCAL_OPERATIONS));
        assert!(OPERATION_DEADLINE <= Duration::from_secs(5 * 60));
        assert!(std::hint::black_box(MAX_LOCAL_RESULT_KEYS) <= 512);
    }

    #[test]
    fn delete_and_extend_replays_are_bound_to_the_expected_runtime() {
        let actor = Uuid::new_v4();
        let runtime = Uuid::new_v4();
        let row = OperationRow {
            operation_id: Uuid::new_v4(),
            scope_key: "participation:7".to_string(),
            actor_user_id: actor,
            game_id: 2,
            participation_id: Some(7),
            challenge_id: 11,
            intent: Intent::Delete.as_str().to_string(),
            publication_id: runtime,
            state: "Running".to_string(),
            result: None,
            lease_active: true,
        };
        assert!(validate_identity(
            &row,
            &ExpectedOperationIdentity {
                scope_key: "participation:7",
                actor_user_id: actor,
                game_id: 2,
                participation_id: Some(7),
                challenge_id: 11,
                intent: Intent::Delete,
                expected_publication_id: Some(runtime),
            },
        )
        .is_ok());
        assert!(validate_identity(
            &row,
            &ExpectedOperationIdentity {
                scope_key: "participation:7",
                actor_user_id: actor,
                game_id: 2,
                participation_id: Some(7),
                challenge_id: 11,
                intent: Intent::Delete,
                expected_publication_id: Some(Uuid::new_v4()),
            },
        )
        .is_err());
        assert!(validate_identity(
            &row,
            &ExpectedOperationIdentity {
                scope_key: "participation:7",
                actor_user_id: actor,
                game_id: 2,
                participation_id: Some(7),
                challenge_id: 11,
                intent: Intent::Extend,
                expected_publication_id: Some(runtime),
            },
        )
        .is_err());
    }
}
