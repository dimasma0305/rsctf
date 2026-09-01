//! Shared policy for changing an existing team roster.

use sqlx::PgConnection;

use crate::utils::error::{AppError, AppResult};

async fn load_roster_state(
    connection: &mut PgConnection,
    team_id: i32,
) -> AppResult<(bool, bool, bool, Option<i32>)> {
    sqlx::query_as(
        r#"WITH checked_at AS MATERIALIZED (
               SELECT clock_timestamp() AS value
           ), target AS MATERIALIZED (
               SELECT id, locked FROM "Teams" WHERE id = $1
           )
           SELECT target.locked,
                  COALESCE(blocker.active_scoring, FALSE),
                  COALESCE(blocker.active, FALSE),
                  blocker.game_id
             FROM target
             CROSS JOIN checked_at
             LEFT JOIN LATERAL (
                 SELECT game.id AS game_id,
                        participation.status IN ($2, $3)
                        AND (game.ad_scoring_start_round IS NOT NULL
                             OR game.koth_scoring_start_round IS NOT NULL)
                        AND (
                            game.end_time_utc > checked_at.value
                            OR EXISTS (
                                SELECT 1 FROM "AdRounds" round
                                 WHERE round.game_id = game.id
                                   AND round.finalized = FALSE
                            )
                        ) AS active_scoring,
                        game.end_time_utc > checked_at.value AS active
                   FROM "Participations" participation
                   JOIN "Games" game ON game.id = participation.game_id
                  WHERE participation.team_id = target.id
                    AND (
                        (
                            participation.status IN ($2, $3)
                            AND (game.ad_scoring_start_round IS NOT NULL
                                 OR game.koth_scoring_start_round IS NOT NULL)
                            AND (
                                game.end_time_utc > checked_at.value
                                OR EXISTS (
                                    SELECT 1 FROM "AdRounds" round
                                     WHERE round.game_id = game.id
                                       AND round.finalized = FALSE
                                )
                            )
                        )
                        OR (target.locked AND game.end_time_utc > checked_at.value)
                    )
                  ORDER BY active_scoring DESC, game.id
                  LIMIT 1
             ) blocker ON TRUE"#,
    )
    .bind(team_id)
    .bind(crate::utils::enums::ParticipationStatus::Accepted as i16)
    .bind(crate::utils::enums::ParticipationStatus::Suspended as i16)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Team not found"))
}

fn reject_frozen_state(
    (locked, active_scoring, active, _blocking_game): (bool, bool, bool, Option<i32>),
) -> AppResult<()> {
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
/// roster immutable. The caller already owns `team-roster:{team_id}`. Only the
/// oldest current blocker is fenced before one final predicate read; historical
/// participations never turn one profile mutation into an unbounded lock set.
pub(crate) async fn ensure_roster_change_allowed(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_id: i32,
) -> AppResult<()> {
    let state = load_roster_state(transaction, team_id).await?;
    let Some(game_id) = state.3 else {
        return Ok(());
    };
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        transaction,
        &crate::services::ad_engine::game_lock_key(game_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    // The blocker can end while its fence is being acquired. Re-evaluate at
    // the mutation's linearization point; a concurrent activation that starts
    // after this point is ordered after the completed profile mutation.
    reject_frozen_state(load_roster_state(transaction, team_id).await?)
}

#[cfg(test)]
#[path = "roster_policy_tests.rs"]
mod tests;
