//! Durable admission, detached ownership, and replay receipts for player
//! container lifecycle work.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::*;

#[path = "operations_recovery.rs"]
mod recovery;
pub(crate) use recovery::sweep;

#[cfg(test)]
#[path = "operations_tests.rs"]
mod tests;

const CLAIM_LOCK: &str = "rsctf:player-container-operation-admission";
const MAX_DEPLOYMENT_OPERATIONS: i64 = 32;
const MAX_LOCAL_OPERATIONS: usize = 4;
const OPERATION_DEADLINE: Duration = Duration::from_secs(120);
const RESULT_WAIT_DEADLINE: Duration = Duration::from_secs(125);
const MAX_LOCAL_RESULT_KEYS: usize = 256;
const MANAGED_REAP_PENDING_SQL: &str = r#"SELECT EXISTS(
       SELECT 1
         FROM "ManagedContainerReapOperations" reap
         JOIN "Containers" container
           ON container.id = reap.container_id
          AND container.container_id = reap.backend_id
        WHERE reap.scope_key = $1
           OR container.id = $2
)"#;

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

pub(crate) struct OperationRequest {
    pub operation_id: Uuid,
    pub may_adopt_stale: bool,
}

pub(crate) fn operation_request(headers: &HeaderMap) -> AppResult<OperationRequest> {
    let Some(value) = headers.get("x-rsctf-operation-id") else {
        return Ok(OperationRequest {
            operation_id: Uuid::new_v4(),
            // A legacy caller cannot resend a server-generated identity. Its
            // next request may adopt only a compatible ambiguous stale row.
            may_adopt_stale: true,
        });
    };
    let value = value
        .to_str()
        .map_err(|_| AppError::bad_request("Invalid container operation ID"))?;
    let operation_id = Uuid::parse_str(value)
        .map_err(|_| AppError::bad_request("Invalid container operation ID"))?;
    if operation_id.is_nil() {
        return Err(AppError::bad_request(
            "Container operation ID must be opaque",
        ));
    }
    Ok(OperationRequest {
        operation_id,
        may_adopt_stale: false,
    })
}

#[derive(Debug)]
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
    runtime_started: bool,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

fn validate_identity(
    row: &OperationRow,
    scope_key: &str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
    intent: Intent,
    expected_publication_id: Option<Uuid>,
) -> AppResult<()> {
    if row.scope_key != scope_key
        || row.actor_user_id != actor_user_id
        || row.game_id != game_id
        || row.participation_id != participation_id
        || row.challenge_id != challenge_id
        || row.intent != intent.as_str()
        || expected_publication_id.is_some_and(|expected| expected != row.publication_id)
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

async fn create_is_still_published(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    row: &OperationRow,
) -> AppResult<bool> {
    let published = match row.participation_id {
        Some(participation_id) => {
            sqlx::query_scalar(
                r#"SELECT EXISTS(
                   SELECT 1
                     FROM "GameInstances" instance
                     JOIN "Containers" container ON container.id = instance.container_id
                    WHERE instance.participation_id = $1
                      AND instance.challenge_id = $2
                      AND container.id = $3 AND container.status = $4
               )"#,
            )
            .bind(participation_id)
            .bind(row.challenge_id)
            .bind(row.publication_id)
            .bind(ContainerStatus::Running as i16)
            .fetch_one(&mut **transaction)
            .await
        }
        None => {
            sqlx::query_scalar(
                r#"SELECT EXISTS(
                   SELECT 1
                     FROM "GameChallenges" challenge
                     JOIN "Containers" container
                       ON container.id = challenge.shared_container_id
                    WHERE challenge.id = $1 AND challenge.game_id = $2
                      AND container.id = $3 AND container.status = $4
               )"#,
            )
            .bind(row.challenge_id)
            .bind(row.game_id)
            .bind(row.publication_id)
            .bind(ContainerStatus::Running as i16)
            .fetch_one(&mut **transaction)
            .await
        }
    };
    published.map_err(database_error)
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
    expected_definition_fence: Option<&str>,
    may_adopt_stale: bool,
) -> AppResult<ClaimOutcome<T>> {
    let mut tx = match tokio::time::timeout(
        Duration::from_millis(250),
        crate::utils::database::begin_sqlx_transaction(pool),
    )
    .await
    {
        Ok(result) => result.map_err(database_error)?,
        Err(_) => {
            return Err(AppError::overloaded(
                "Container operation admission is busy",
                1,
            ));
        }
    };
    let admitted =
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(CLAIM_LOCK)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
    if !admitted {
        tx.rollback().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "Container operation admission is busy",
            1,
        ));
    }
    let runtime_lock_key = match participation_id {
        Some(participation_id) => format!("game-container:{participation_id}"),
        None => format!("shared-container:{challenge_id}"),
    };
    let runtime_available: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&runtime_lock_key)
            .fetch_one(&mut *tx)
            .await
            .map_err(database_error)?;
    if !runtime_available {
        tx.rollback().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "Container lifecycle reconciliation is busy",
            2,
        ));
    }
    let teardown_pending: bool = sqlx::query_scalar(MANAGED_REAP_PENDING_SQL)
        .bind(&runtime_lock_key)
        .bind(expected_publication_id.unwrap_or(Uuid::nil()))
        .fetch_one(&mut *tx)
        .await
        .map_err(database_error)?;
    if teardown_pending {
        tx.rollback().await.map_err(database_error)?;
        return Err(AppError::overloaded(
            "Container teardown is being reconciled",
            2,
        ));
    }
    sqlx::query(
        r#"WITH expired AS (
               SELECT operation_id
                 FROM "PlayerContainerOperations"
                WHERE state <> 'Running'
                  AND updated_at_utc < clock_timestamp() - interval '24 hours'
                ORDER BY updated_at_utc, operation_id
                LIMIT 64
           )
           DELETE FROM "PlayerContainerOperations" operation
            USING expired WHERE operation.operation_id = expired.operation_id"#,
    )
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    let existing = sqlx::query_as::<_, OperationRow>(
        r#"SELECT operation_id, scope_key, actor_user_id, game_id, participation_id, challenge_id,
                  intent, publication_id, state, result,
                  lease_expires_at_utc > clock_timestamp() AS lease_active,
                  runtime_started
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
            scope_key,
            actor_user_id,
            game_id,
            participation_id,
            challenge_id,
            intent,
            expected_publication_id,
        )?;
        if row.state == "Succeeded" {
            if matches!(intent, Intent::Create) && !create_is_still_published(&mut tx, &row).await?
            {
                sqlx::query(
                    r#"UPDATE "PlayerContainerOperations"
                          SET state = 'Failed', result = NULL,
                              updated_at_utc = clock_timestamp(),
                              lease_expires_at_utc = clock_timestamp()
                        WHERE operation_id = $1 AND state = 'Succeeded'"#,
                )
                .bind(row.operation_id)
                .execute(&mut *tx)
                .await
                .map_err(database_error)?;
                row.state = "Failed".to_string();
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
                      lease_expires_at_utc = clock_timestamp() + interval '3 minutes',
                      runtime_started = CASE WHEN state = 'Failed' THEN FALSE
                                             ELSE runtime_started END,
                      definition_fence = CASE WHEN state = 'Failed' THEN NULL
                                              ELSE definition_fence END,
                      backend_id = CASE WHEN state = 'Failed' THEN NULL
                                        ELSE backend_id END
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

    // Collapse only the immediate shared opener herd. Each caller still gets
    // its own durable exact-replay row; older opens take the normal path so
    // backend liveness is probed and the shared lease is refreshed.
    if matches!(intent, Intent::Create) && participation_id.is_none() {
        let reusable = sqlx::query_as::<_, (Uuid, serde_json::Value, Option<String>)>(
            r#"SELECT operation.publication_id, operation.result,
                      operation.definition_fence
                 FROM "PlayerContainerOperations" operation
                 JOIN "Containers" container ON container.id = operation.publication_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = operation.challenge_id
                  AND challenge.shared_container_id = container.id
                WHERE operation.scope_key = $1 AND operation.game_id = $2
                  AND operation.challenge_id = $3 AND operation.intent = 'Create'
                  AND operation.state = 'Succeeded' AND container.status = $4
                  AND operation.updated_at_utc >= clock_timestamp() - interval '5 seconds'
                  AND operation.definition_fence = $5
             ORDER BY operation.updated_at_utc DESC, operation.operation_id
                LIMIT 1"#,
        )
        .bind(scope_key)
        .bind(game_id)
        .bind(challenge_id)
        .bind(ContainerStatus::Running as i16)
        .bind(expected_definition_fence)
        .fetch_optional(&mut *tx)
        .await
        .map_err(database_error)?;
        if let Some((publication_id, stored, definition_fence)) = reusable {
            if expected_publication_id.is_some_and(|expected| expected != publication_id) {
                tx.rollback().await.map_err(database_error)?;
                return Err(AppError::conflict(
                    "Shared container ownership changed; refresh and retry",
                ));
            }
            sqlx::query(
                r#"INSERT INTO "PlayerContainerOperations"
                       (operation_id, scope_key, actor_user_id, game_id, participation_id,
                        challenge_id, intent, publication_id, state, result,
                        lease_expires_at_utc, definition_fence)
                   VALUES ($1, $2, $3, $4, NULL, $5, 'Create', $6, 'Succeeded', $7,
                           clock_timestamp() + interval '24 hours', $8)"#,
            )
            .bind(operation_id)
            .bind(scope_key)
            .bind(actor_user_id)
            .bind(game_id)
            .bind(challenge_id)
            .bind(publication_id)
            .bind(&stored)
            .bind(definition_fence)
            .execute(&mut *tx)
            .await
            .map_err(database_error)?;
            let result = serde_json::from_value(stored)
                .map_err(|error| AppError::internal(error.to_string()))?;
            tx.commit().await.map_err(database_error)?;
            return Ok(ClaimOutcome::Recovered(result));
        }
    }

    let stale_scope = sqlx::query_as::<_, OperationRow>(
        r#"SELECT operation_id, scope_key, actor_user_id, game_id, participation_id, challenge_id,
                  intent, publication_id, state, result,
                  lease_expires_at_utc > clock_timestamp() AS lease_active,
                  runtime_started
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
        let compatible = row.game_id == game_id
            && row.participation_id == participation_id
            && row.challenge_id == challenge_id
            && row.intent == intent.as_str()
            && expected_publication_id.is_none_or(|expected| expected == row.publication_id);
        if !row.runtime_started {
            // Pre-launch validation, capacity, and database failures cannot
            // have created an unowned workload. Terminalize them regardless
            // of challenge/intent so one participation scope never wedges.
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
        } else if may_adopt_stale && compatible {
            // A headerless legacy caller cannot resend the server-generated
            // identity. Resume only the exact ambiguous launch; an explicit
            // client key is never silently aliased to this older operation.
            if active_count(&mut tx).await? >= MAX_DEPLOYMENT_OPERATIONS {
                tx.commit().await.map_err(database_error)?;
                return Err(AppError::overloaded(
                    "Container provisioning capacity is busy",
                    2,
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
        } else {
            tx.commit().await.map_err(database_error)?;
            return Err(AppError::conflict(
                "A stale container launch must be reconciled with its original operation ID",
            ));
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
                challenge_id, intent, publication_id, state, lease_expires_at_utc,
                definition_fence)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'Running',
                   clock_timestamp() + interval '3 minutes', $9)
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
    .bind(expected_definition_fence)
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
    expected_publication_id: Option<Uuid>,
    expected_definition_fence: Option<&str>,
    may_adopt_stale: bool,
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
        expected_publication_id,
        expected_definition_fence,
        may_adopt_stale,
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
    may_adopt_stale: bool,
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
        None,
        may_adopt_stale,
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
    may_adopt_stale: bool,
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
        None,
        may_adopt_stale,
    )
    .await
}

pub(crate) async fn bind_definition(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
    definition_fence: &str,
) -> AppResult<()> {
    if definition_fence.is_empty() || definition_fence.len() > 256 {
        return Err(AppError::internal(
            "container definition fence exceeds its durable bound",
        ));
    }
    let stored = sqlx::query_scalar::<_, String>(
        r#"UPDATE "PlayerContainerOperations"
              SET definition_fence = COALESCE(definition_fence, $3),
                  updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'
        RETURNING definition_fence"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .bind(definition_fence)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?
    .ok_or_else(|| {
        AppError::conflict("Container operation ownership changed before definition binding")
    })?;
    if stored != definition_fence {
        return Err(AppError::conflict(
            "The challenge workload changed after this container operation began",
        ));
    }
    Ok(())
}

/// Persist the irreversible launch boundary before asking a runtime backend
/// to create/adopt a workload. An expired row with this bit set can only be
/// resumed by its exact ID (or a compatible headerless legacy adoption).
pub(crate) async fn mark_runtime_started(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
              SET runtime_started = TRUE, updated_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND publication_id = $2
              AND state = 'Running' AND definition_fence IS NOT NULL"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Container operation ownership changed before runtime launch",
        ));
    }
    Ok(())
}

pub(crate) async fn record_backend(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
    backend_id: &str,
) -> AppResult<()> {
    if backend_id.is_empty() || backend_id.len() > 512 {
        return Err(AppError::internal(
            "container backend identity exceeds its durable bound",
        ));
    }
    let stored = sqlx::query_scalar::<_, String>(
        r#"UPDATE "PlayerContainerOperations"
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
    .ok_or_else(|| AppError::conflict("Container operation ownership changed after launch"))?;
    if stored != backend_id {
        return Err(AppError::conflict(
            "Container operation resolved to another backend runtime",
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

async fn settle_failed_work(pool: &sqlx::PgPool, operation: &ClaimedOperation) {
    let terminalized = match sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
              SET state = 'Failed', result = NULL,
                  lease_expires_at_utc = clock_timestamp(),
                  updated_at_utc = clock_timestamp()
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
            tracing::warn!(operation_id = %operation.operation_id, %error, "failed to terminalize pre-launch container operation");
            false
        }
    };
    if terminalized {
        return;
    }
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
        tracing::warn!(operation_id = %operation.operation_id, %error, "failed to release ambiguous container operation for exact retry");
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
            settle_failed_work(&pool, &operation).await;
            return Err(AppError::overloaded(
                "Local container operation capacity is busy",
                2,
            ));
        };
        let result = match tokio::time::timeout(OPERATION_DEADLINE, work).await {
            Ok(result) => result,
            Err(_) => {
                settle_failed_work(&pool, &operation).await;
                return Err(AppError::overloaded("Container operation timed out", 2));
            }
        };
        match &result {
            Ok(value) => complete(&pool, &operation, value).await?,
            Err(_) => settle_failed_work(&pool, &operation).await,
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
