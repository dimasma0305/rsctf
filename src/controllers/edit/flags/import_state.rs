//! Durable admission, lease, and attachment-staging state for flag imports.

use super::*;

const MAX_PENDING_FLAG_IMPORTS: i64 = 64;

#[derive(Debug)]
pub(super) enum FlagImportReservation {
    Acquired {
        lease_token: Uuid,
        recovered_attachment_ids: Vec<i32>,
    },
    Replayed(FlagImportResult),
}

pub(super) async fn abandon_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) {
    if let Err(error) = sqlx::query(
        r#"WITH removed AS (
               DELETE FROM "FlagImportOperations"
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_token = $3
              RETURNING 1
           )
           UPDATE "FlagImportSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token = $3 AND EXISTS (SELECT 1 FROM removed)"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(lease_token)
    .execute(pool)
    .await
    {
        tracing::warn!(%error, challenge_id, %operation_id, "failed to abandon flag import reservation");
    }
}

pub(super) async fn reconcile_staged_flag_attachments(st: &SharedState) -> AppResult<()> {
    let rows = sqlx::query_as::<_, (i32, Uuid, Vec<i32>)>(
        r#"WITH expired AS (
               UPDATE "FlagImportOperations"
                  SET state = 2, completed_at_utc = clock_timestamp()
                WHERE (challenge_id, operation_id) IN (
                    SELECT challenge_id, operation_id
                      FROM "FlagImportOperations"
                     WHERE state = 0
                       AND lease_expires_at_utc <= clock_timestamp()
                       AND created_at_utc < clock_timestamp() - INTERVAL '1 hour'
                     ORDER BY created_at_utc, challenge_id, operation_id
                     LIMIT 4 FOR UPDATE SKIP LOCKED
                )
              RETURNING lease_token
           ), released AS (
               UPDATE "FlagImportSlots"
                  SET lease_token = NULL, expires_at_utc = NULL
                WHERE lease_token IN (SELECT lease_token FROM expired)
           )
           SELECT challenge_id, operation_id, staged_attachment_ids
             FROM "FlagImportOperations"
            WHERE state IN (1, 2) AND cardinality(staged_attachment_ids) > 0
            ORDER BY completed_at_utc, challenge_id, operation_id
            LIMIT 4"#,
    )
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for (challenge_id, operation_id, staged_ids) in rows {
        let remaining = cleanup_attachment_ids(st, &staged_ids).await;
        sqlx::query(
            r#"UPDATE "FlagImportOperations" SET staged_attachment_ids = $3
                WHERE challenge_id = $1 AND operation_id = $2 AND state IN (1, 2)"#,
        )
        .bind(challenge_id)
        .bind(operation_id)
        .bind(&remaining)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

async fn claim_flag_import_slot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease_token: Uuid,
) -> AppResult<bool> {
    let claimed = sqlx::query_scalar::<_, i16>(
        r#"WITH candidate AS (
               SELECT slot_id FROM "FlagImportSlots"
                WHERE lease_token IS NULL OR expires_at_utc <= clock_timestamp()
                ORDER BY slot_id FOR UPDATE SKIP LOCKED LIMIT 1
           )
           UPDATE "FlagImportSlots" slot
              SET lease_token = $1,
                  expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
             FROM candidate
            WHERE slot.slot_id = candidate.slot_id
           RETURNING slot.slot_id"#,
    )
    .bind(lease_token)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed.is_some())
}

pub(super) async fn reserve_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    request_digest: &[u8],
) -> AppResult<FlagImportReservation> {
    let lease_token = Uuid::new_v4();
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let admission_owner: bool = sqlx::query_scalar(
        "SELECT pg_try_advisory_xact_lock(hashtextextended('rsctf:flag-import-admission', 0))",
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !admission_owner {
        return Err(AppError::too_many_requests(1));
    }

    let stored =
        sqlx::query_as::<_, (Uuid, Vec<u8>, i16, Option<i32>, Option<i32>, bool, Vec<i32>)>(
            r#"SELECT actor_user_id, request_digest, state, inserted_count,
                  duplicate_count, lease_expires_at_utc <= clock_timestamp(),
                  staged_attachment_ids
             FROM "FlagImportOperations"
            WHERE challenge_id = $1 AND operation_id = $2"#,
        )
        .bind(challenge_id)
        .bind(operation_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut recovered_attachment_ids = Vec::new();
    if let Some(stored) = stored {
        if stored.0 != actor_user_id || stored.1 != request_digest {
            return Err(AppError::conflict(
                "The operation ID is already bound to another flag import",
            ));
        }
        if stored.2 == 1 {
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(FlagImportReservation::Replayed(FlagImportResult {
                inserted: stored.3.unwrap_or_default(),
                duplicates: stored.4.unwrap_or_default(),
            }));
        }
        if stored.2 == 2 {
            return Err(AppError::conflict(
                "This expired flag import can no longer be resumed",
            ));
        }
        if !stored.5 {
            return Err(AppError::conflict(
                "This flag import is still running; retry its operation ID later",
            ));
        }
        if !claim_flag_import_slot(&mut transaction, lease_token).await? {
            return Err(AppError::too_many_requests(1));
        }
        let reclaimed = sqlx::query(
            r#"UPDATE "FlagImportOperations"
                  SET lease_token = $3,
                      lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_expires_at_utc <= clock_timestamp()"#,
        )
        .bind(challenge_id)
        .bind(operation_id)
        .bind(lease_token)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if reclaimed.rows_affected() != 1 {
            return Err(AppError::conflict(
                "This flag import was reclaimed by another request",
            ));
        }
        recovered_attachment_ids = stored.6;
    } else {
        let pending = sqlx::query_scalar::<_, i64>(
            r#"SELECT COUNT(*)::bigint FROM "FlagImportOperations" WHERE state = 0"#,
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if pending >= MAX_PENDING_FLAG_IMPORTS {
            return Err(AppError::too_many_requests(1));
        }
        if !claim_flag_import_slot(&mut transaction, lease_token).await? {
            return Err(AppError::too_many_requests(1));
        }
        sqlx::query(
            r#"INSERT INTO "FlagImportOperations"
                 (challenge_id, operation_id, actor_user_id, request_digest, lease_token)
               VALUES ($1, $2, $3, $4, $5)"#,
        )
        .bind(challenge_id)
        .bind(operation_id)
        .bind(actor_user_id)
        .bind(request_digest)
        .bind(lease_token)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(FlagImportReservation::Acquired {
        lease_token,
        recovered_attachment_ids,
    })
}

pub(super) async fn renew_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) -> AppResult<bool> {
    let renewed = sqlx::query_scalar::<_, i64>(
        r#"WITH slot AS (
               UPDATE "FlagImportSlots"
                  SET expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE lease_token = $3
              RETURNING 1
           ), operation AS (
               UPDATE "FlagImportOperations"
                  SET lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_token = $3 AND EXISTS (SELECT 1 FROM slot)
              RETURNING 1
           ) SELECT COUNT(*)::bigint FROM operation"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(lease_token)
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(renewed == 1)
}

async fn cleanup_attachment_ids(st: &SharedState, attachment_ids: &[i32]) -> Vec<i32> {
    let mut remaining = Vec::new();
    for &attachment_id in attachment_ids {
        if let Err(error) = delete_attachment(st, attachment_id).await {
            tracing::warn!(
                %error,
                attachment_id,
                "failed to clean an unpublished flag attachment"
            );
            remaining.push(attachment_id);
        }
    }
    remaining
}

pub(super) async fn recover_reclaimed_flag_attachments(
    st: &SharedState,
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
    attachment_ids: &[i32],
) -> AppResult<()> {
    if attachment_ids.is_empty() {
        return Ok(());
    }
    let remaining = cleanup_attachment_ids(st, attachment_ids).await;
    if !remaining.is_empty() {
        fail_staged_flag_import(st.pg(), challenge_id, operation_id, lease_token, &remaining).await;
        return Err(AppError::unavailable(
            "A previous flag import attachment could not be rolled back",
        ));
    }
    let cleared = sqlx::query(
        r#"UPDATE "FlagImportOperations" SET staged_attachment_ids = '{}'
            WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
              AND lease_token = $3"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(lease_token)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if cleared.rows_affected() != 1 {
        return Err(AppError::conflict(
            "This flag import was reclaimed while attachments were rolled back",
        ));
    }
    Ok(())
}

pub(super) async fn cleanup_staged_flag_attachments(
    st: &SharedState,
    flags: &[(String, Option<i32>)],
) -> Vec<i32> {
    let ids = flags
        .iter()
        .filter_map(|(_, attachment_id)| *attachment_id)
        .collect::<Vec<_>>();
    cleanup_attachment_ids(st, &ids).await
}

#[cfg(test)]
pub(super) async fn record_staged_flag_attachment(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
    attachment_id: i32,
) -> AppResult<bool> {
    let updated = sqlx::query(
        r#"UPDATE "FlagImportOperations"
              SET staged_attachment_ids = array_append(staged_attachment_ids, $4)
            WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
              AND lease_token = $3 AND lease_expires_at_utc > clock_timestamp()"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(lease_token)
    .bind(attachment_id)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(updated.rows_affected() == 1)
}

pub(super) async fn fail_staged_flag_import(
    pool: &sqlx::PgPool,
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
    remaining_attachment_ids: &[i32],
) {
    if let Err(error) = sqlx::query(
        r#"WITH failed AS (
               UPDATE "FlagImportOperations"
                  SET state = 2, completed_at_utc = clock_timestamp(),
                      staged_attachment_ids = $4
                WHERE challenge_id = $1 AND operation_id = $2 AND state = 0
                  AND lease_token = $3
              RETURNING lease_token
           )
           UPDATE "FlagImportSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token IN (SELECT lease_token FROM failed)"#,
    )
    .bind(challenge_id)
    .bind(operation_id)
    .bind(lease_token)
    .bind(remaining_attachment_ids)
    .execute(pool)
    .await
    {
        tracing::warn!(%error, challenge_id, %operation_id, "failed to retain flag staging rollback state");
    }
}

pub(super) async fn rollback_staged_flag_import(
    st: &SharedState,
    flags: &[(String, Option<i32>)],
    challenge_id: i32,
    operation_id: Uuid,
    lease_token: Uuid,
) {
    let remaining = cleanup_staged_flag_attachments(st, flags).await;
    fail_staged_flag_import(st.pg(), challenge_id, operation_id, lease_token, &remaining).await;
}
