//! Durable admission and replay receipts for player container lifecycle work.

use super::*;

const CLAIM_LOCK: &str = "rsctf:player-container-operation-admission";
const MAX_DEPLOYMENT_OPERATIONS: i64 = 32;

#[derive(Debug)]
pub(crate) struct ClaimedOperation {
    pub operation_id: Uuid,
    pub publication_id: Uuid,
}

pub(crate) enum ClaimOutcome {
    Owned(ClaimedOperation),
    Recovered(ContainerInfoModel),
}

#[derive(sqlx::FromRow)]
struct OperationRow {
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

fn validate_identity(
    row: &OperationRow,
    scope_key: &str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
    intent: &str,
) -> AppResult<()> {
    if row.scope_key != scope_key
        || row.actor_user_id != actor_user_id
        || row.game_id != game_id
        || row.participation_id != participation_id
        || row.challenge_id != challenge_id
        || row.intent != intent
    {
        return Err(AppError::conflict(
            "Container operation identity was reused for another intent",
        ));
    }
    Ok(())
}

pub(crate) async fn claim_create(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
    scope_key: &str,
    actor_user_id: Uuid,
    game_id: i32,
    participation_id: Option<i32>,
    challenge_id: i32,
) -> AppResult<ClaimOutcome> {
    let mut tx = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(CLAIM_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
              SET state = 'Failed', updated_at_utc = clock_timestamp()
            WHERE state = 'Running' AND lease_expires_at_utc <= clock_timestamp()"#,
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let existing = sqlx::query_as::<_, OperationRow>(
        r#"SELECT scope_key, actor_user_id, game_id, participation_id, challenge_id,
                  intent, publication_id, state, result,
                  lease_expires_at_utc > clock_timestamp() AS lease_active
             FROM "PlayerContainerOperations"
            WHERE operation_id = $1
            FOR UPDATE"#,
    )
    .bind(operation_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(row) = existing {
        validate_identity(
            &row,
            scope_key,
            actor_user_id,
            game_id,
            participation_id,
            challenge_id,
            "Create",
        )?;
        if row.state == "Succeeded" {
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
            .map_err(|error| AppError::internal(error.to_string()))?;
            if still_published {
                let model =
                    serde_json::from_value(row.result.ok_or_else(|| {
                        AppError::internal("container result receipt is missing")
                    })?)
                    .map_err(|error| AppError::internal(error.to_string()))?;
                tx.commit()
                    .await
                    .map_err(|error| AppError::internal(error.to_string()))?;
                return Ok(ClaimOutcome::Recovered(model));
            }
        }
        if row.state == "Running" && row.lease_active {
            tx.commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Err(AppError::overloaded(
                "Container operation is already running",
                2,
            ));
        }
        let active: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM "PlayerContainerOperations"
                WHERE state = 'Running' AND lease_expires_at_utc > clock_timestamp()"#,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if active >= MAX_DEPLOYMENT_OPERATIONS {
            tx.commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Err(AppError::overloaded(
                "Container provisioning capacity is busy",
                2,
            ));
        }
        let reclaimed = sqlx::query(
            r#"UPDATE "PlayerContainerOperations"
                  SET state = 'Running', result = NULL,
                      updated_at_utc = clock_timestamp(),
                      lease_expires_at_utc = clock_timestamp() + interval '2 minutes'
                WHERE operation_id = $1"#,
        )
        .bind(operation_id)
        .execute(&mut *tx)
        .await;
        match reclaimed {
            Ok(_) => {}
            Err(error) if crate::utils::error::is_unique_violation(&error) => {
                tx.rollback()
                    .await
                    .map_err(|rollback| AppError::internal(rollback.to_string()))?;
                return Err(AppError::overloaded(
                    "Another container operation is active for this team",
                    2,
                ));
            }
            Err(error) => return Err(AppError::internal(error.to_string())),
        }
        tx.commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(ClaimOutcome::Owned(ClaimedOperation {
            operation_id,
            publication_id: row.publication_id,
        }));
    }

    let active: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "PlayerContainerOperations"
            WHERE state = 'Running' AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if active >= MAX_DEPLOYMENT_OPERATIONS {
        tx.commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::overloaded(
            "Container provisioning capacity is busy",
            2,
        ));
    }
    let publication_id = Uuid::new_v4();
    let inserted = sqlx::query(
        r#"INSERT INTO "PlayerContainerOperations"
               (operation_id, scope_key, actor_user_id, game_id, participation_id,
                challenge_id, intent, publication_id, state, lease_expires_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, 'Create', $7, 'Running',
                   clock_timestamp() + interval '2 minutes')
           ON CONFLICT (operation_id) DO NOTHING"#,
    )
    .bind(operation_id)
    .bind(scope_key)
    .bind(actor_user_id)
    .bind(game_id)
    .bind(participation_id)
    .bind(challenge_id)
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
        Err(error) => return Err(AppError::internal(error.to_string())),
    }
    tx.commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ClaimOutcome::Owned(ClaimedOperation {
        operation_id,
        publication_id,
    }))
}

pub(crate) async fn complete_create(
    pool: &sqlx::PgPool,
    operation: &ClaimedOperation,
    result: &ContainerInfoModel,
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
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Container operation ownership changed before publication",
        ));
    }
    Ok(())
}

pub(crate) async fn fail_create(pool: &sqlx::PgPool, operation: &ClaimedOperation) {
    if let Err(error) = sqlx::query(
        r#"UPDATE "PlayerContainerOperations"
              SET state = 'Failed', updated_at_utc = clock_timestamp(),
                  lease_expires_at_utc = clock_timestamp() + interval '15 minutes'
            WHERE operation_id = $1 AND publication_id = $2 AND state = 'Running'"#,
    )
    .bind(operation.operation_id)
    .bind(operation.publication_id)
    .execute(pool)
    .await
    {
        tracing::warn!(operation_id = %operation.operation_id, %error, "failed to mark container operation failed");
    }
}

pub(crate) async fn purge_terminal(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    sqlx::query(
        r#"WITH stale AS (
               UPDATE "PlayerContainerOperations"
                  SET state = 'Failed', updated_at_utc = clock_timestamp()
                WHERE state = 'Running' AND lease_expires_at_utc <= clock_timestamp()
           ), victims AS (
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
    .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_operation_budget_is_bounded() {
        assert!((1..=64).contains(&MAX_DEPLOYMENT_OPERATIONS));
    }
}
