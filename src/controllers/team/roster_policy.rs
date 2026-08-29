//! Shared policy for changing an existing team roster.

use sqlx::PgConnection;

use crate::utils::error::{AppError, AppResult};

const MAX_ROSTER_POLICY_GAME_LOCKS: usize = 32;

const RELEVANT_GAME_LOCKS_SQL: &str = r#"WITH checked_at AS MATERIALIZED (
               SELECT clock_timestamp() AS value
           )
           SELECT DISTINCT participation.game_id
             FROM "Participations" participation
             JOIN "Games" game ON game.id = participation.game_id
             CROSS JOIN checked_at
            WHERE participation.team_id = $1
              AND (
                  game.end_time_utc > checked_at.value
                  OR (
                      participation.status IN ($2, $3)
                      AND (game.ad_scoring_start_round IS NOT NULL
                           OR game.koth_scoring_start_round IS NOT NULL)
                      AND EXISTS (
                          SELECT 1
                            FROM "AdRounds" round
                           WHERE round.game_id = game.id
                             AND round.finalized = FALSE
                      )
                  )
              )
            ORDER BY participation.game_id
            LIMIT $4"#;

fn validate_game_lock_count(game_ids: Vec<i32>) -> AppResult<Vec<i32>> {
    if game_ids.len() > MAX_ROSTER_POLICY_GAME_LOCKS {
        return Err(AppError::overloaded(
            "Team participates in too many active games for a safe roster change",
            2,
        ));
    }
    Ok(game_ids)
}

async fn load_roster_state(
    connection: &mut PgConnection,
    team_id: i32,
) -> AppResult<(bool, bool, bool)> {
    sqlx::query_as(
        r#"WITH checked_at AS MATERIALIZED (
               SELECT clock_timestamp() AS value
           )
           SELECT team.locked,
                  COALESCE(bool_or(
                      participation.status IN ($2, $3)
                      AND (game.ad_scoring_start_round IS NOT NULL
                           OR game.koth_scoring_start_round IS NOT NULL)
                      AND (
                          game.end_time_utc > checked_at.value
                          OR EXISTS (
                              SELECT 1
                                FROM "AdRounds" round
                               WHERE round.game_id = game.id
                                 AND round.finalized = FALSE
                          )
                      )
                  ), FALSE) AS active_scoring,
                  COALESCE(bool_or(
                      game.end_time_utc > checked_at.value
                  ), FALSE) AS active
             FROM "Teams" team
             CROSS JOIN checked_at
             LEFT JOIN "Participations" participation ON participation.team_id = team.id
             LEFT JOIN "Games" game ON game.id = participation.game_id
            WHERE team.id = $1
            GROUP BY team.locked"#,
    )
    .bind(team_id)
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))
}

fn reject_frozen_state((locked, active_scoring, active): (bool, bool, bool)) -> AppResult<()> {
    if active_scoring {
        return Err(AppError::bad_request(
            "Team membership cannot change after A&D/KotH epoch scoring has started",
        ));
    }
    if locked && active {
        return Err(AppError::bad_request("Team is locked by an active game"));
    }
    Ok(())
}

/// Cheap early rejection before reading a multipart profile body. This is only
/// an optimisation: callers must repeat [`ensure_roster_change_allowed`] under
/// the canonical roster and ordered game fences before publishing a mutation.
pub(super) async fn preflight_roster_change_allowed(
    connection: &mut PgConnection,
    team_id: i32,
) -> AppResult<()> {
    reject_frozen_state(load_roster_state(connection, team_id).await?)
}

/// Reject an addition or removal while an existing participation makes the
/// roster immutable. The caller already owns `team-roster:{team_id}`; game
/// locks are acquired in ascending order so registration, invitation, public
/// removal, and game edits observe one cross-replica ordering.
pub(crate) async fn ensure_roster_change_allowed(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
) -> AppResult<()> {
    let game_ids: Vec<i32> = sqlx::query_scalar(RELEVANT_GAME_LOCKS_SQL)
        .bind(team_id)
        .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
        .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
        .bind((MAX_ROSTER_POLICY_GAME_LOCKS + 1) as i64)
        .fetch_all(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    for game_id in validate_game_lock_count(game_ids)? {
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            transaction,
            &crate::services::ad_engine::game_lock_key(game_id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    reject_frozen_state(load_roster_state(transaction, team_id).await?)
}

#[cfg(test)]
#[path = "roster_policy_tests.rs"]
mod tests;

#[cfg(test)]
mod bounded_lock_tests {
    use super::{validate_game_lock_count, RELEVANT_GAME_LOCKS_SQL};

    #[test]
    fn historical_games_are_filtered_and_lock_work_is_bounded() {
        assert!(RELEVANT_GAME_LOCKS_SQL.contains("game.end_time_utc > checked_at.value"));
        assert!(RELEVANT_GAME_LOCKS_SQL.contains("round.finalized = FALSE"));
        assert!(RELEVANT_GAME_LOCKS_SQL.contains("LIMIT $4"));
        assert!(validate_game_lock_count((0..32).collect()).is_ok());
        let error = validate_game_lock_count((0..33).collect()).unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
}
