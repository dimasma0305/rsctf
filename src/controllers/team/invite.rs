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

use crate::services::team_invites::reconcile_invite_rotation;

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
