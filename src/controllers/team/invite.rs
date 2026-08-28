//! Revisioned team invitation credentials and aggregate BYOC reconciliation.

use serde::{Deserialize, Serialize};

use super::*;
use crate::utils::codec::random_hex;

const INVITE_RECONCILE_LEASE_SECONDS: i64 = 300;

const CLAIM_INVITE_RECONCILE_SQL: &str = r#"UPDATE "TeamInviteOperations"
       SET reconcile_claim_id = $3,
           reconcile_claim_expires_at_utc =
               clock_timestamp() + make_interval(secs => $4)
     WHERE team_id = $1 AND operation_id = $2
       AND reconciled_at_utc IS NULL
       AND (reconcile_claim_id IS NULL
            OR reconcile_claim_expires_at_utc < clock_timestamp())"#;

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
    // The token and its revision are one credential snapshot. Reading a SeaORM
    // team model and the revision separately could pair an old token with a new
    // revision during a concurrent rotation.
    let (team_name, invite_token, captain_id, revision) =
        sqlx::query_as::<_, (String, String, Uuid, i64)>(
            r#"SELECT name, invite_token, captain_id, invite_revision
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
        code: format!("{team_name}:{id}:{invite_token}"),
        revision,
    }))
}

async fn reconcile_invite_rotation(
    st: &SharedState,
    team_id: i32,
    operation_id: Uuid,
) -> AppResult<()> {
    let claim_id = Uuid::new_v4();
    let mut claim = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let operation = sqlx::query_as::<_, (bool, bool)>(
        r#"SELECT reconciled_at_utc IS NOT NULL,
                  reconcile_claim_id IS NOT NULL
                  AND reconcile_claim_expires_at_utc >= clock_timestamp()
             FROM "TeamInviteOperations"
            WHERE team_id = $1 AND operation_id = $2
            FOR UPDATE"#,
    )
    .bind(team_id)
    .bind(operation_id)
    .fetch_optional(&mut *claim)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("Invite rotation operation expired"))?;
    if operation.0 {
        claim
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(());
    }
    if operation.1 {
        return Err(AppError::overloaded(
            "Invite rotation reconciliation is already running",
            2,
        ));
    }
    let claimed = sqlx::query(CLAIM_INVITE_RECONCILE_SQL)
        .bind(team_id)
        .bind(operation_id)
        .bind(claim_id)
        .bind(INVITE_RECONCILE_LEASE_SECONDS as f64)
        .execute(&mut *claim)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if claimed.rows_affected() != 1 {
        return Err(AppError::overloaded(
            "Invite rotation reconciliation is already running",
            2,
        ));
    }
    claim
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    if let Err(error) = st.byoc.disconnect_team(&st.db, team_id).await {
        if let Err(clear_error) = sqlx::query(
            r#"UPDATE "TeamInviteOperations"
                  SET reconcile_claim_id = NULL,
                      reconcile_claim_expires_at_utc = NULL
                WHERE team_id = $1 AND operation_id = $2
                  AND reconcile_claim_id = $3
                  AND reconciled_at_utc IS NULL"#,
        )
        .bind(team_id)
        .bind(operation_id)
        .bind(claim_id)
        .execute(st.pg())
        .await
        {
            tracing::warn!(team_id, %operation_id, %claim_id, %clear_error, "failed to release invite reconciliation claim");
        }
        return Err(error);
    }

    let finalized = sqlx::query(
        r#"UPDATE "TeamInviteOperations"
              SET reconciled_at_utc = clock_timestamp(),
                  reconcile_claim_id = NULL,
                  reconcile_claim_expires_at_utc = NULL
            WHERE team_id = $1 AND operation_id = $2
              AND reconcile_claim_id = $3
              AND reconciled_at_utc IS NULL"#,
    )
    .bind(team_id)
    .bind(operation_id)
    .bind(claim_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if finalized.rows_affected() == 1 {
        return Ok(());
    }
    let reconciled = sqlx::query_scalar::<_, bool>(
        r#"SELECT reconciled_at_utc IS NOT NULL
             FROM "TeamInviteOperations"
            WHERE team_id = $1 AND operation_id = $2"#,
    )
    .bind(team_id)
    .bind(operation_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(false);
    if reconciled {
        Ok(())
    } else {
        Err(AppError::overloaded(
            "Invite rotation reconciliation ownership changed; retry",
            2,
        ))
    }
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
    let captain_id = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT captain_id FROM "Teams"
            WHERE id = $1 AND deletion_pending = FALSE"#,
    )
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
    if captain_id != user.id {
        return Err(AppError::Forbidden);
    }
    let mut roster = acquire_roster_mutation(st.pg(), id).await?;
    require_team_mutable(roster.transaction_mut(), id).await?;
    ensure_roster_change_allowed(roster.transaction_mut(), id).await?;
    let (team_name, captain_id, current_revision) = sqlx::query_as::<_, (String, Uuid, i64)>(
        r#"SELECT name, captain_id, invite_revision
             FROM "Teams"
            WHERE id = $1 AND deletion_pending = FALSE
            FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **roster.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))?;
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
    roster.release().await?;
    reconcile_invite_rotation(&st, id, request.operation_id).await?;
    Ok(RequestResponse::ok(TeamInviteModel {
        code: format!("{team_name}:{id}:{token}"),
        revision: result_revision,
    }))
}

#[cfg(test)]
mod tests {
    use super::{CLAIM_INVITE_RECONCILE_SQL, INVITE_RECONCILE_LEASE_SECONDS};

    #[test]
    fn reconciliation_is_claimed_without_retaining_a_session_lock() {
        assert_eq!(INVITE_RECONCILE_LEASE_SECONDS, 300);
        assert!(CLAIM_INVITE_RECONCILE_SQL.contains("reconcile_claim_id = $3"));
        assert!(CLAIM_INVITE_RECONCILE_SQL.contains("reconcile_claim_expires_at_utc"));
        assert!(!CLAIM_INVITE_RECONCILE_SQL.contains("pg_advisory"));
    }
}
