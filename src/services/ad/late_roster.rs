//! Append-only admission of a team after official KotH scoring has started.

use crate::utils::error::{AppError, AppResult};

/// Add a newly accepted participation to an existing official KotH roster.
///
/// The caller owns the per-game advisory lock. Existing epoch rollups receive
/// explicit zero rows so joining late never erases the cost of the rounds that
/// were already played. A game without an official KotH snapshot is an A&D-only
/// game, or has not completed KotH startup yet; its ordinary roster query will
/// include the new accepted participation when startup eventually succeeds.
pub(crate) async fn admit_late_koth_participation(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    participation_id: i32,
) -> AppResult<bool> {
    let snapshot: Option<(bool, i64)> = sqlx::query_as(
        r#"SELECT EXISTS (
                     SELECT 1
                       FROM jsonb_array_elements(config.roster_snapshot) roster(item)
                      WHERE CASE jsonb_typeof(roster.item)
                              WHEN 'number' THEN (roster.item #>> '{}')::integer
                              WHEN 'object' THEN
                                NULLIF(roster.item->>'participationId', '')::integer
                              ELSE NULL
                            END = $2
                   ) AS already_present,
                   jsonb_array_length(config.roster_snapshot)::bigint AS roster_count
              FROM "KothOfficialConfigs" config
             WHERE config.game_id = $1
             FOR UPDATE"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((already_present, roster_count)) = snapshot else {
        return Ok(false);
    };
    if already_present {
        return Ok(false);
    }
    if roster_count
        >= i64::try_from(crate::services::ad::engine::koth_api::MAX_LEADERBOARD_TEAMS)
            .unwrap_or(i64::MAX)
    {
        return Err(AppError::bad_request(
            "The KotH roster has reached its supported team limit",
        ));
    }

    sqlx::query(
        r#"UPDATE "KothOfficialConfigs"
              SET roster_snapshot = roster_snapshot || jsonb_build_array($2)
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // Preserve the event-from-start denominator for the overall KotH score.
    // These rows are bounded by the number of already materialized epochs and
    // use the existing `(game, epoch)` primary key as their source.
    sqlx::query(
        r#"INSERT INTO "KothEpochTeamRollups"
             (game_id, epoch, participation_id, points, epoch_weight,
              acquisition_rate, control_rate, sla_rate, acquisition_windows,
              controlled_ticks, responsible_ticks, healthy_responsible_ticks,
              cumulative_points_numerator, cumulative_epoch_weight,
              cumulative_acquisition_numerator, cumulative_control_numerator,
              cumulative_sla_numerator, cumulative_rate_weight,
              cumulative_acquisition_windows, cumulative_controlled_ticks,
              cumulative_responsible_ticks, cumulative_healthy_responsible_ticks)
           SELECT rollup.game_id, rollup.epoch, $2, 0.0, rollup.epoch_weight,
                  0.0, 0.0, 0.0, 0, 0, 0, 0, 0.0,
                  SUM(rollup.epoch_weight) OVER (ORDER BY rollup.epoch),
                  0.0, 0.0, 0.0, 0.0, 0, 0, 0, 0
             FROM "KothEpochRollups" rollup
            WHERE rollup.game_id = $1
            ORDER BY rollup.epoch
           ON CONFLICT (game_id, epoch, participation_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // Keep per-hill aggregates consistent with the overall zero-score prefix.
    // One existing row supplies the immutable hill weights for each epoch.
    sqlx::query(
        r#"WITH representative AS (
             SELECT DISTINCT ON (hill.epoch, hill.challenge_id)
                    hill.game_id, hill.epoch, hill.challenge_id,
                    hill.service_weight, hill.evidence_fraction,
                    hill.epoch_fraction
               FROM "KothEpochHillRollups" hill
              WHERE hill.game_id = $1
              ORDER BY hill.epoch, hill.challenge_id, hill.participation_id
           ), weighted AS (
             SELECT representative.*,
                    SUM(epoch_fraction * evidence_fraction) OVER (
                      PARTITION BY challenge_id ORDER BY epoch
                    ) AS cumulative_score_weight,
                    SUM(epoch_fraction * evidence_fraction * service_weight) OVER (
                      PARTITION BY challenge_id ORDER BY epoch
                    ) AS cumulative_rate_weight
               FROM representative
           )
           INSERT INTO "KothEpochHillRollups"
             (game_id, epoch, participation_id, challenge_id, service_weight,
              evidence_fraction, epoch_fraction, local_points,
              acquisition_rate, control_rate, sla_rate, acquisition_windows,
              controlled_ticks, responsible_ticks, healthy_responsible_ticks,
              cumulative_points_numerator, cumulative_score_weight,
              cumulative_acquisition_numerator, cumulative_control_numerator,
              cumulative_sla_numerator, cumulative_rate_weight,
              cumulative_acquisition_windows, cumulative_controlled_ticks,
              cumulative_responsible_ticks, cumulative_healthy_responsible_ticks)
           SELECT game_id, epoch, $2, challenge_id, service_weight,
                  evidence_fraction, epoch_fraction, 0.0,
                  0.0, 0.0, 0.0, 0, 0, 0, 0, 0.0,
                  cumulative_score_weight, 0.0, 0.0, 0.0,
                  cumulative_rate_weight, 0, 0, 0, 0
             FROM weighted
            ORDER BY epoch, challenge_id
           ON CONFLICT (game_id, epoch, participation_id, challenge_id)
           DO NOTHING"#,
    )
    .bind(game_id)
    .bind(participation_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(true)
}

#[cfg(test)]
#[path = "late_roster_tests.rs"]
mod tests;
