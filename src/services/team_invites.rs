//! Durable reconciliation of committed team invite rotations.

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use std::sync::{Arc, LazyLock};
use uuid::Uuid;

const INVITE_RECONCILIATION_CONCURRENCY: usize = 2;
static INVITE_RECONCILIATION_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> = LazyLock::new(|| {
    Arc::new(tokio::sync::Semaphore::new(
        INVITE_RECONCILIATION_CONCURRENCY,
    ))
});

async fn claim_invite_reconciliation_slot(
    pool: &sqlx::PgPool,
    lease_token: Uuid,
) -> AppResult<bool> {
    let claimed = sqlx::query_scalar::<_, i16>(
        r#"WITH candidate AS (
               SELECT slot_id FROM "TeamInviteReconciliationSlots"
                WHERE lease_token IS NULL OR expires_at_utc <= clock_timestamp()
                ORDER BY slot_id FOR UPDATE SKIP LOCKED LIMIT 1
           )
           UPDATE "TeamInviteReconciliationSlots" slot
              SET lease_token = $1,
                  expires_at_utc = clock_timestamp() + INTERVAL '15 minutes'
             FROM candidate
            WHERE slot.slot_id = candidate.slot_id
           RETURNING slot.slot_id"#,
    )
    .bind(lease_token)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(claimed.is_some())
}

async fn release_invite_reconciliation_slot(
    pool: &sqlx::PgPool,
    lease_token: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "TeamInviteReconciliationSlots"
              SET lease_token = NULL, expires_at_utc = NULL
            WHERE lease_token = $1"#,
    )
    .bind(lease_token)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn reconcile_invite_rotation_inner(
    st: &SharedState,
    team_id: i32,
    operation_id: Uuid,
) -> AppResult<()> {
    let key = format!("team-invite-reconcile:{team_id}");
    let mut lease =
        crate::utils::single_flight::PgSessionAdvisoryLock::acquire_roster(st.pg(), &key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let work: AppResult<()> = async {
        let pending = sqlx::query_as::<_, (bool, i64)>(
            r#"SELECT reconciled_at_utc IS NULL, result_revision
             FROM "TeamInviteOperations"
            WHERE team_id = $1 AND operation_id = $2"#,
        )
        .bind(team_id)
        .bind(operation_id)
        .fetch_optional(lease.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::conflict("Invite rotation operation expired"))?;
        if pending.0 {
            st.byoc.disconnect_team(&st.db, team_id).await?;
            // A successful disconnect also covers every older credential rotation.
            // Fence the update by the revision observed before teardown so a newer
            // rotation committed while this external work ran remains pending and
            // performs its own disconnect.
            sqlx::query(
                r#"UPDATE "TeamInviteOperations"
                      SET reconciled_at_utc = clock_timestamp()
                    WHERE team_id = $1 AND result_revision <= $2
                      AND reconciled_at_utc IS NULL"#,
            )
            .bind(team_id)
            .bind(pending.1)
            .execute(lease.connection_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
        sqlx::query(
            r#"WITH expired AS (
                   SELECT team_id, operation_id FROM "TeamInviteOperations"
                    WHERE reconciled_at_utc IS NOT NULL
                      AND created_at_utc < clock_timestamp() - INTERVAL '30 days'
                    ORDER BY created_at_utc, team_id, operation_id
                    LIMIT 128
               )
               DELETE FROM "TeamInviteOperations" operation USING expired
                WHERE operation.team_id = expired.team_id
                  AND operation.operation_id = expired.operation_id"#,
        )
        .execute(lease.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        Ok(())
    }
    .await;
    let unlocked = lease
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()));
    work.and(unlocked)
}

pub(crate) async fn reconcile_invite_rotation(
    st: &SharedState,
    team_id: i32,
    operation_id: Uuid,
) -> AppResult<()> {
    let reconciled: Option<bool> = sqlx::query_scalar(
        r#"SELECT reconciled_at_utc IS NOT NULL FROM "TeamInviteOperations"
            WHERE team_id = $1 AND operation_id = $2"#,
    )
    .bind(team_id)
    .bind(operation_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    match reconciled {
        Some(true) => return Ok(()),
        None => return Err(AppError::conflict("Invite rotation operation expired")),
        Some(false) => {}
    }

    let permit = INVITE_RECONCILIATION_SLOTS
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(1))?;
    let slot_token = Uuid::new_v4();
    if !claim_invite_reconciliation_slot(st.pg(), slot_token).await? {
        drop(permit);
        return Err(AppError::too_many_requests(1));
    }
    let result = reconcile_invite_rotation_inner(st, team_id, operation_id).await;
    let released = release_invite_reconciliation_slot(st.pg(), slot_token).await;
    drop(permit);
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// Retry a bounded page of committed invite rotations whose aggregate BYOC
/// teardown did not finish before the originating request ended. PostgreSQL
/// advisory ownership inside `reconcile_invite_rotation` keeps this safe when
/// several replicas observe the same pending row.
pub(crate) async fn recover_pending_invite_rotations(
    st: &SharedState,
    limit: i64,
) -> AppResult<u64> {
    let rows = sqlx::query_as::<_, (i32, Uuid)>(
        r#"SELECT team_id, operation_id
             FROM "TeamInviteOperations"
            WHERE reconciled_at_utc IS NULL
            ORDER BY created_at_utc, team_id, operation_id
            LIMIT $1"#,
    )
    .bind(limit.clamp(1, 16))
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut recovered = 0_u64;
    for (team_id, operation_id) in rows {
        match reconcile_invite_rotation(st, team_id, operation_id).await {
            Ok(()) => recovered = recovered.saturating_add(1),
            Err(error) => tracing::warn!(
                %error,
                team_id,
                %operation_id,
                "invite rotation reconciliation remains pending"
            ),
        }
    }
    Ok(recovered)
}
