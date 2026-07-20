//! Shared policy for changing an existing team roster.

use crate::utils::error::{AppError, AppResult};

/// Reject an addition or removal while an existing participation makes the
/// roster immutable. The caller already owns `team-roster:{team_id}`; game
/// locks are acquired in ascending order so registration, invitation, public
/// removal, and game edits observe one cross-replica ordering.
pub(crate) async fn ensure_roster_change_allowed(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
) -> AppResult<()> {
    let game_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT DISTINCT game_id
              FROM "Participations"
             WHERE team_id = $1
             ORDER BY game_id"#,
    )
    .bind(team_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    for game_id in game_ids {
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            transaction,
            &crate::services::ad_engine::game_lock_key(game_id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    let state: Option<(bool, bool, bool)> = sqlx::query_as(
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
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((locked, active_scoring, active)) = state else {
        return Err(AppError::not_found("Team not found"));
    };
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

#[cfg(test)]
#[path = "roster_policy_tests.rs"]
mod tests;
