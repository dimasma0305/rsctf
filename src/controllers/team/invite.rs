//! Revisioned team invitation credentials and aggregate BYOC reconciliation.

use serde::{Deserialize, Serialize};

use super::*;
use crate::utils::codec::random_hex;

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

/// `GET /api/team/{id}/invite` — current invite code (captain only).
pub async fn invite_code(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<TeamInviteModel>> {
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;
    let revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT invite_revision FROM "Teams" WHERE id = $1 AND deletion_pending = FALSE"#,
    )
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
    Ok(RequestResponse::ok(TeamInviteModel {
        code: team.invite_code(),
        revision,
    }))
}

async fn reconcile_invite_rotation(
    st: &SharedState,
    team_id: i32,
    operation_id: Uuid,
) -> AppResult<()> {
    let key = format!("team-invite-reconcile:{team_id}");
    let mut lease =
        crate::utils::single_flight::PgSessionAdvisoryLock::acquire_roster(st.pg(), &key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    let pending = sqlx::query_scalar::<_, bool>(
        r#"SELECT reconciled_at_utc IS NULL
             FROM "TeamInviteOperations"
            WHERE team_id = $1 AND operation_id = $2"#,
    )
    .bind(team_id)
    .bind(operation_id)
    .fetch_optional(lease.connection_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("Invite rotation operation expired"))?;
    if pending {
        st.byoc.disconnect_team(&st.db, team_id).await?;
        sqlx::query(
            r#"UPDATE "TeamInviteOperations"
                  SET reconciled_at_utc = clock_timestamp()
                WHERE team_id = $1 AND operation_id = $2
                  AND reconciled_at_utc IS NULL"#,
        )
        .bind(team_id)
        .bind(operation_id)
        .execute(lease.connection_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    lease
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
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
    require_team_mutable(roster.transaction_mut(), id).await?;
    let team = load_team(&st, id).await?;
    require_captain(&team, &user)?;
    let current_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT invite_revision FROM "Teams" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(id)
    .fetch_one(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
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
        if revision != current_revision {
            return Err(AppError::conflict(format!(
                "A newer invite code exists at revision {current_revision}"
            )));
        }
        (token, revision)
    } else {
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
        sqlx::query(
            r#"WITH expired AS (
                   SELECT team_id, operation_id FROM "TeamInviteOperations"
                    WHERE created_at_utc < clock_timestamp() - INTERVAL '30 days'
                    ORDER BY created_at_utc, team_id, operation_id
                    LIMIT 128
               )
               DELETE FROM "TeamInviteOperations" operation USING expired
                WHERE operation.team_id = expired.team_id
                  AND operation.operation_id = expired.operation_id"#,
        )
        .execute(&mut **roster.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        (token, revision)
    };
    let _local_owner = roster.release_for_external().await?;
    reconcile_invite_rotation(&st, id, request.operation_id).await?;
    Ok(RequestResponse::ok(TeamInviteModel {
        code: format!("{}:{}:{}", team.name, id, token),
        revision: result_revision,
    }))
}
