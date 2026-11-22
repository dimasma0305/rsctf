use super::*;
use axum::{routing, Json, Router};

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct PolicyState {
    enabled: bool,
    revision: i64,
    editable: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Update {
    enabled: bool,
    expected_revision: i64,
}

pub(crate) fn router() -> Router<SharedState> {
    Router::new().route(
        "/api/edit/games/{id}/ad/patch-activity",
        routing::get(get).put(update),
    )
}

const POLICY: &str = r#"SELECT COALESCE(p.enabled,false) enabled, COALESCE(p.revision,0) revision,
    g.start_time_utc > clock_timestamp() AND NOT EXISTS
      (SELECT 1 FROM "AdRounds" r WHERE r.game_id=g.id) editable
    FROM "Games" g LEFT JOIN "AdPatchActivityPolicies" p ON p.game_id=g.id
    WHERE g.id=$1 AND NOT g.deletion_pending"#;

fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

async fn get(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game): Path<i32>,
) -> AppResult<Json<PolicyState>> {
    manager_or_admin(&st, &user, game).await?;
    let state = sqlx::query_as(POLICY)
        .bind(game)
        .fetch_optional(st.pg())
        .await
        .map_err(db_error)?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    Ok(Json(state))
}

async fn update(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game): Path<i32>,
    Json(change): Json<Update>,
) -> AppResult<Json<PolicyState>> {
    manager_or_admin(&st, &user, game).await?;
    let mut tx = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(db_error)?;
    if !user.is_admin()
        && sqlx::query_scalar::<_, i32>(
            r#"SELECT game_id FROM "GameManagers" WHERE game_id=$1 AND user_id=$2 FOR SHARE"#,
        )
        .bind(game)
        .bind(user.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .is_none()
    {
        return Err(AppError::Forbidden);
    }
    sqlx::query_scalar::<_, i32>(
        r#"SELECT id FROM "Games" WHERE id=$1 AND NOT deletion_pending FOR UPDATE"#,
    )
    .bind(game)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let mut policy: PolicyState = sqlx::query_as(POLICY)
        .bind(game)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
    if let DesiredStateDecision::Transition { next_revision } = decide_desired_state(
        policy.enabled,
        policy.revision,
        change.enabled,
        change.expected_revision,
        "Patch activity",
    )? {
        if !policy.editable {
            return Err(AppError::conflict(
                "Choose patch visibility before the game starts",
            ));
        }
        sqlx::query(r#"INSERT INTO "AdPatchActivityPolicies"(game_id,enabled,revision)
             VALUES($1,$2,$3) ON CONFLICT(game_id) DO UPDATE
             SET enabled=EXCLUDED.enabled,revision=EXCLUDED.revision,changed_at=EXCLUDED.changed_at"#)
            .bind(game).bind(change.enabled).bind(next_revision).execute(&mut *tx).await.map_err(db_error)?;
        policy.enabled = change.enabled;
        policy.revision = next_revision;
    }
    tx.commit().await.map_err(db_error)?;
    Ok(Json(policy))
}
