//! Revisioned team invitation credentials and aggregate BYOC reconciliation.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, LazyLock};

use super::*;
use crate::utils::codec::random_hex;

const INVITE_RECONCILIATION_CONCURRENCY: usize = 2;
static INVITE_RECONCILIATION_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> = LazyLock::new(|| {
    Arc::new(tokio::sync::Semaphore::new(
        INVITE_RECONCILIATION_CONCURRENCY,
    ))
});

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInviteModel {
    pub code: String,
    pub revision: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInviteRotateRequest {
    pub operation_id: Uuid,
    pub expected_revision: i64,
}

type InviteRotationTeam = (Uuid, String, i64);

fn require_invite_rotation_team(team: Option<InviteRotationTeam>) -> AppResult<InviteRotationTeam> {
    team.ok_or_else(|| AppError::not_found("Team not found"))
}

fn require_authoritative_invite_result(
    result_revision: i64,
    current_revision: i64,
) -> AppResult<()> {
    if result_revision == current_revision {
        Ok(())
    } else {
        Err(AppError::conflict(format!(
            "A newer invite code exists at revision {current_revision}"
        )))
    }
}

/// `GET /api/team/{id}/invite` — current invite code (captain only).
pub async fn invite_code(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<TeamInviteModel>> {
    let (captain_id, name, token, revision) = sqlx::query_as::<_, (Uuid, String, String, i64)>(
        r#"SELECT captain_id, name, invite_token, invite_revision
                 FROM "Teams" WHERE id = $1 AND deletion_pending = FALSE"#,
    )
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
    if captain_id != user.id {
        return Err(AppError::Forbidden);
    }
    Ok(RequestResponse::ok(TeamInviteModel {
        code: format!("{name}:{id}:{token}"),
        revision,
    }))
}

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

async fn reconcile_invite_rotation(
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

/// `PUT /api/team/{id}/invite` — regenerate the invite token (captain only).
pub async fn update_invite_token(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(request): Json<TeamInviteRotateRequest>,
) -> AppResult<RequestResponse<TeamInviteModel>> {
    if request.operation_id.is_nil() || request.expected_revision < 1 {
        return Err(AppError::bad_request(
            "Invite rotation requires an operation ID and observed revision",
        ));
    }
    let mut roster = acquire_roster_mutation(st.pg(), id).await?;
    let team = sqlx::query_as::<_, InviteRotationTeam>(
        r#"SELECT captain_id, name, invite_revision
             FROM "Teams" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (captain_id, team_name, current_revision) = require_invite_rotation_team(team)?;
    if captain_id != user.id {
        return Err(AppError::Forbidden);
    }
    let stored = sqlx::query_as::<_, (Uuid, i64, i64, String)>(
        r#"SELECT actor_user_id, expected_revision, result_revision, result_token
             FROM "TeamInviteOperations"
            WHERE team_id = $1 AND operation_id = $2"#,
    )
    .bind(id)
    .bind(request.operation_id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let (token, result_revision) = if let Some((actor, expected, revision, token)) = stored {
        if actor != user.id || expected != request.expected_revision {
            return Err(AppError::conflict(
                "The operation ID is already bound to another invite rotation",
            ));
        }
        require_authoritative_invite_result(revision, current_revision)?;
        (token, revision)
    } else {
        require_team_mutable(roster.transaction_mut(), id).await?;
        ensure_roster_change_allowed(roster.transaction_mut(), id).await?;
        if current_revision != request.expected_revision {
            return Err(AppError::conflict(format!(
                "Invite code changed; current revision is {current_revision}"
            )));
        }
        let token = random_hex(16);
        let revision = current_revision + 1;
        sqlx::query(r#"UPDATE "Teams" SET invite_token = $2, invite_revision = $3 WHERE id = $1"#)
            .bind(id)
            .bind(&token)
            .bind(revision)
            .execute(&mut **roster.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        sqlx::query(
            r#"INSERT INTO "TeamInviteOperations"
                 (team_id, operation_id, actor_user_id, expected_revision,
                  result_revision, result_token)
               VALUES ($1, $2, $3, $4, $5, $6)"#,
        )
        .bind(id)
        .bind(request.operation_id)
        .bind(user.id)
        .bind(current_revision)
        .bind(revision)
        .bind(&token)
        .execute(&mut **roster.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        (token, revision)
    };
    let _local_owner = roster.release_for_external().await?;
    reconcile_invite_rotation(&st, id, request.operation_id).await?;
    // Another captain tab may rotate while this request performs external BYOC
    // teardown. Never disclose an operation-owned code after it has ceased to
    // be the authoritative credential.
    let authoritative_revision: i64 =
        sqlx::query_scalar(r#"SELECT invite_revision FROM "Teams" WHERE id = $1"#)
            .bind(id)
            .fetch_optional(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::not_found("Team not found"))?;
    require_authoritative_invite_result(result_revision, authoritative_revision)?;
    Ok(RequestResponse::ok(TeamInviteModel {
        code: format!("{team_name}:{id}:{token}"),
        revision: result_revision,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_rotation_team_is_not_found() {
        let error = require_invite_rotation_team(None).unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn superseded_rotation_never_discloses_an_old_invite_code() {
        assert!(require_authoritative_invite_result(4, 4).is_ok());
        let error = require_authoritative_invite_result(4, 5).unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    }
}
