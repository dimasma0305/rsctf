use super::service_lock_key;
use crate::app_state::SharedState;
use crate::services::ad::maintenance::database_error;
use crate::services::worker::WorkerHandle;
use crate::services::worker_store::WorkerStore;
use crate::utils::error::{AppError, AppResult};
use sqlx::PgConnection;

pub(in crate::services::ad) enum WriteIntent {
    Retain,
    Publish,
}

pub(in crate::services::ad) async fn lock_reset_assignment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game: i32,
    service: i32,
    backend: &str,
    intent: WriteIntent,
) -> AppResult<bool> {
    let Some(handle) = WorkerHandle::from_backend_id(backend)? else {
        return Ok(true);
    };
    let owner = sqlx::query_as::<_, (i32, i32)>(
        r#"SELECT participation_id,challenge_id FROM "AdTeamServices" WHERE id=$1 AND game_id=$2"#,
    )
    .bind(service)
    .bind(game)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    let Some((participant, challenge)) = owner else {
        return Ok(false);
    };
    // Provisioning locks the workload before updating the service. Take the
    // same order here, and recheck the owner after acquiring the service lock.
    if !lock_eligible_assignment(tx, participant, challenge, handle, intent).await? {
        return Ok(false);
    }
    let locked = sqlx::query_scalar::<_, i32>(
        r#"SELECT id FROM "AdTeamServices" WHERE id=$1 AND game_id=$2
            AND participation_id=$3 AND challenge_id=$4 FOR NO KEY UPDATE"#,
    )
    .bind(service)
    .bind(game)
    .bind(participant)
    .bind(challenge)
    .fetch_optional(&mut **tx)
    .await
    .map_err(database_error)?;
    Ok(locked.is_some())
}

/// The workload row is locked through the endpoint write. The network owner
/// also checks the current worker session when it opens a data stream.
pub(super) async fn lock_eligible_assignment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    participant: i32,
    challenge: i32,
    handle: WorkerHandle,
    intent: WriteIntent,
) -> AppResult<bool> {
    let conn: &mut PgConnection = tx;
    let owned = sqlx::query_scalar::<_, bool>(
        r#"SELECT true FROM "WorkerWorkloads" w
          WHERE w.id=$1 AND w.assignment_id=$2 AND w.generation=$3
            AND w.owner_kind='ad-team-service' AND w.owner_key=$4
            AND w.desired_state='Present' AND w.spec->>'gameKind'='attackDefense'
          FOR SHARE OF w"#,
    )
    .bind(handle.workload_id)
    .bind(handle.assignment_id)
    .bind(handle.generation)
    .bind(service_lock_key(participant, challenge))
    .fetch_optional(&mut *conn)
    .await
    .map_err(database_error)?;
    if owned != Some(true) {
        return Ok(false);
    }
    if matches!(intent, WriteIntent::Publish)
        && WorkerStore::ready_workload_session_on(conn, handle.workload_id)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .is_none()
    {
        return Ok(false);
    }
    Ok(true)
}

/// The assignment and team owner are immutable. The worker backend checks the
/// generation again at deletion, so a concurrent revision survives a late cleanup.
async fn check_destroy_owner(
    state: &SharedState,
    participant: i32,
    challenge: i32,
    id: &str,
) -> AppResult<()> {
    let Some(handle) = WorkerHandle::from_backend_id(id)? else {
        return Ok(());
    };
    let owned = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM "WorkerWorkloads"
           WHERE id=$1 AND assignment_id=$2 AND owner_kind='ad-team-service' AND owner_key=$3
             AND spec->>'gameKind'='attackDefense' AND (generation=$4 OR desired_state='Absent'))"#,
    )
    .bind(handle.workload_id)
    .bind(handle.assignment_id)
    .bind(service_lock_key(participant, challenge))
    .bind(handle.generation)
    .fetch_one(state.pg())
    .await
    .map_err(database_error)?;
    if owned {
        Ok(())
    } else {
        Err(AppError::conflict(
            "The worker assignment does not match this team's service revision",
        ))
    }
}

pub(crate) async fn destroy_team_backend(
    state: &SharedState,
    participant: i32,
    challenge: i32,
    id: &str,
) -> AppResult<()> {
    check_destroy_owner(state, participant, challenge, id).await?;
    state.containers.destroy(id).await
}

pub(crate) async fn destroy_team_backend_after_capture(
    state: &SharedState,
    participant: i32,
    challenge: i32,
    id: &str,
) -> AppResult<()> {
    check_destroy_owner(state, participant, challenge, id).await?;
    crate::services::traffic::destroy_container_after_capture_fence(state, id).await
}
