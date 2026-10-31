//! Live operational edits are admitted only between completed scoring epochs.
//!
//! A maintenance window keeps the official scoring policy and retained evidence
//! intact. Runtime definitions can change while the engine is paused; resumption
//! records the effective round and replaces affected runtimes before reopening play.

use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

#[cfg(test)]
mod pg_tests;
mod resume;
pub(crate) use resume::execute_resume;

#[derive(Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MaintenanceWindow {
    pub id: Uuid,
    pub game_id: i32,
    pub state: String,
    pub effective_from_round: Option<i32>,
    pub error: Option<String>,
}

pub(crate) fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

/// Every supplied scoring domain must finish its epoch. KotH transport changes
/// use this epoch fence; A&D-only operational edits need the completed-round fence.
pub(crate) fn is_boundary(next: i32, domains: &[(Option<i32>, i32)]) -> bool {
    next > 0
        && domains.iter().all(|(start, ticks)| match start {
            None => true,
            Some(start) => *ticks > 0 && next >= *start && (next - start) % ticks == 0,
        })
}

pub(crate) async fn current(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<Option<MaintenanceWindow>> {
    sqlx::query_as(
        r#"SELECT id, game_id, state, effective_from_round, error
             FROM "CompetitionMaintenanceWindows" WHERE game_id = $1
            ORDER BY created_at DESC, id DESC LIMIT 1"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(database_error)
}

pub(crate) async fn editing_allowed(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (
             SELECT 1 FROM "CompetitionMaintenanceWindows" maintenance
             JOIN "Games" game ON game.id = maintenance.game_id
             WHERE maintenance.game_id = $1 AND maintenance.state = 'Editing'
               AND game.ad_scoring_paused
               AND maintenance.effective_from_round = (
                 SELECT COALESCE(MAX(number), 0) + 1 FROM "AdRounds" WHERE game_id = $1
               )
           )"#,
    )
    .bind(game_id)
    .fetch_one(connection)
    .await
    .map_err(database_error)
}

pub(crate) async fn resume_required(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<bool> {
    sqlx::query_scalar(
        r#"SELECT EXISTS (SELECT 1 FROM "CompetitionMaintenanceWindows"
             WHERE game_id = $1 AND state IN ('Editing','Applying'))"#,
    )
    .bind(game_id)
    .fetch_one(connection)
    .await
    .map_err(database_error)
}

/// Called after the last pipeline has completed and before opening a new round.
/// The game lock serializes this fence with pause commands and round preparation.
pub(crate) async fn enter_if_due(
    st: &crate::app_state::SharedState,
    game_id: i32,
) -> AppResult<bool> {
    let pending: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (SELECT 1 FROM "CompetitionMaintenanceWindows"
             WHERE game_id = $1 AND state = 'Requested')"#,
    )
    .bind(game_id)
    .fetch_one(st.pg())
    .await
    .map_err(database_error)?;
    if !pending {
        return Ok(false);
    }
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let connection = &mut **control.transaction_mut();
    let (koth_start, koth_ticks): (Option<i32>, i32) = sqlx::query_as(
        r#"SELECT koth_scoring_start_round, koth_epoch_ticks
             FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(database_error)?;
    let latest: Option<(i32, bool)> = sqlx::query_as(
        r#"SELECT number, pipeline_completed_at IS NOT NULL AND end_time_utc <= clock_timestamp()
             FROM "AdRounds" WHERE game_id = $1 ORDER BY number DESC LIMIT 1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(database_error)?;
    let next = latest.map_or(1, |row| row.0.saturating_add(1));
    if latest.is_some_and(|row| !row.1) || !is_boundary(next, &[(koth_start, koth_ticks)]) {
        return Ok(false);
    }
    let changed = sqlx::query(
        r#"UPDATE "CompetitionMaintenanceWindows" SET state = 'Editing',
             effective_from_round = $2, before_snapshot = (
               SELECT COALESCE(jsonb_agg((rsctf_competition_definition(challenge) || jsonb_build_object(
                 'observer_revision', (SELECT revision FROM "KothApiObserverRevisions"
                     WHERE challenge_id = challenge.id))) ORDER BY challenge.id), '[]'::jsonb)
               FROM "GameChallenges" challenge WHERE challenge.game_id = $1 AND challenge."Type" IN (4,5)
             ), entered_at = clock_timestamp()
           WHERE game_id = $1 AND state = 'Requested'"#,
    ).bind(game_id).bind(next).execute(&mut *connection).await.map_err(database_error)?;
    if changed.rows_affected() == 0 {
        return Ok(false);
    }
    sqlx::query(
        r#"UPDATE "Games" SET ad_scoring_paused = TRUE,
         ad_scoring_paused_at = clock_timestamp(), ad_control_revision = ad_control_revision + 1
         WHERE id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *connection)
    .await
    .map_err(database_error)?;
    control.release().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[test]
    fn maintenance_never_splits_either_scoring_epoch() {
        let domains = [(Some(1), 6), (Some(1), 24)];
        assert!(!super::is_boundary(7, &domains));
        assert!(!super::is_boundary(24, &domains));
        assert!(super::is_boundary(25, &domains));
        assert!(super::is_boundary(1, &[(None, 0)]));
        assert!(!super::is_boundary(1, &[(Some(1), 0)]));
        assert!(!super::is_boundary(0, &[]));
    }
}
