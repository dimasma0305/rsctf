//! Authenticated operator entry points for a scoring-boundary maintenance window.
use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::services::ad::maintenance::{self, MaintenanceWindow};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

pub(crate) fn router() -> axum::Router<SharedState> {
    axum::Router::new()
        .route(
            "/api/edit/games/{id}/ad/maintenance",
            axum::routing::get(get).post(request),
        )
        .route(
            "/api/edit/games/{id}/ad/maintenance/resume",
            limited(Policy::Concurrency, axum::routing::post(resume)),
        )
        .route(
            "/api/edit/games/{id}/ad/maintenance/cancel",
            axum::routing::post(cancel),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MaintenanceRequest {
    operation_id: Uuid,
}

pub(crate) async fn get(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<Option<MaintenanceWindow>>> {
    super::manager_or_admin(&st, &user, game_id).await?;
    Ok(RequestResponse::ok(
        maintenance::current(st.pg(), game_id).await?,
    ))
}

pub(crate) async fn request(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    Json(request): Json<MaintenanceRequest>,
) -> AppResult<RequestResponse<Option<MaintenanceWindow>>> {
    super::manager_or_admin(&st, &user, game_id).await?;
    if request.operation_id.is_nil() {
        return Err(AppError::bad_request("operationId is required"));
    }
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let connection = &mut **control.transaction_mut();
    let active: bool = sqlx::query_scalar(
        r#"SELECT NOT deletion_pending AND end_time_utc > clock_timestamp()
             AND (ad_scoring_start_round IS NOT NULL OR koth_scoring_start_round IS NOT NULL)
             FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(maintenance::database_error)?;
    if !active {
        return Err(AppError::conflict(
            "Live maintenance requires an ongoing scored competition",
        ));
    }
    let (challenges, windows): (i64, i64) = sqlx::query_as(
        r#"SELECT
         (SELECT COUNT(*) FROM "GameChallenges" WHERE game_id = $1 AND "Type" IN (4,5)),
         (SELECT COUNT(*) FROM "CompetitionMaintenanceWindows" WHERE game_id = $1)"#,
    )
    .bind(game_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(maintenance::database_error)?;
    let existing: Option<i32> =
        sqlx::query_scalar(r#"SELECT game_id FROM "CompetitionMaintenanceWindows" WHERE id = $1"#)
            .bind(request.operation_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(maintenance::database_error)?;
    if existing.is_some_and(|id| id != game_id) {
        return Err(AppError::conflict("operationId is already in use"));
    }
    if existing.is_none() && (challenges > 256 || windows >= 128) {
        return Err(AppError::conflict(
            "Maintenance supports up to 256 engine challenges and 128 windows per event",
        ));
    }
    sqlx::query(
        r#"INSERT INTO "CompetitionMaintenanceWindows" (id, game_id, actor_id, state)
             VALUES ($1, $2, $3, 'Requested') ON CONFLICT DO NOTHING"#,
    )
    .bind(request.operation_id)
    .bind(game_id)
    .bind(user.id)
    .execute(&mut *connection)
    .await
    .map_err(maintenance::database_error)?;
    control.release().await?;
    maintenance::enter_if_due(&st, game_id).await?;
    Ok(RequestResponse::ok(
        maintenance::current(st.pg(), game_id).await?,
    ))
}

pub(crate) async fn resume(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    Json(request): Json<MaintenanceRequest>,
) -> AppResult<RequestResponse<crate::services::control_jobs::ControlJobModel>> {
    super::manager_or_admin(&st, &user, game_id).await?;
    let window = maintenance::current(st.pg(), game_id)
        .await?
        .filter(|window| matches!(window.state.as_str(), "Editing" | "Applying"))
        .ok_or_else(|| {
            AppError::conflict("Wait for the maintenance boundary before applying changes")
        })?;
    let input = serde_json::json!({"windowId": window.id});
    let fingerprint = crate::services::control_jobs::fingerprint(&input)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::CompetitionResume,
        &format!("game:{game_id}:maintenance"),
        game_id,
        None,
        request.operation_id,
        &fingerprint,
        input,
    )
    .await?;
    crate::services::control_jobs::kick(st);
    Ok(RequestResponse::ok(job))
}

pub(crate) async fn cancel(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    Json(request): Json<MaintenanceRequest>,
) -> AppResult<RequestResponse<Option<MaintenanceWindow>>> {
    super::manager_or_admin(&st, &user, game_id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let changed = sqlx::query(r#"UPDATE "CompetitionMaintenanceWindows" SET state='Cancelled',
        completed_at=clock_timestamp() WHERE id=$1 AND game_id=$2 AND state IN ('Requested','Cancelled')"#)
        .bind(request.operation_id).bind(game_id).execute(&mut **control.transaction_mut())
        .await.map_err(maintenance::database_error)?;
    if changed.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Maintenance has already opened; apply changes before resuming",
        ));
    }
    control.release().await?;
    Ok(RequestResponse::ok(
        maintenance::current(st.pg(), game_id).await?,
    ))
}
