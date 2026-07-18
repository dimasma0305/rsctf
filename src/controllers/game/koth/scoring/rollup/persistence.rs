use sqlx::PgConnection;

use super::{ComputedHillRow, ComputedTeamRow};
use crate::utils::error::{AppError, AppResult};

pub(super) async fn insert_team_rows(
    connection: &mut PgConnection,
    game_id: i32,
    epoch: i32,
    rows: &[ComputedTeamRow],
) -> AppResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let participation_ids: Vec<_> = rows.iter().map(|row| row.participation_id).collect();
    let points: Vec<_> = rows.iter().map(|row| row.points).collect();
    let epoch_weights: Vec<_> = rows.iter().map(|row| row.epoch_weight).collect();
    let acquisition_rates: Vec<_> = rows.iter().map(|row| row.acquisition_rate).collect();
    let control_rates: Vec<_> = rows.iter().map(|row| row.control_rate).collect();
    let sla_rates: Vec<_> = rows.iter().map(|row| row.sla_rate).collect();
    let acquisition_windows: Vec<_> = rows.iter().map(|row| row.acquisition_windows).collect();
    let controlled_ticks: Vec<_> = rows.iter().map(|row| row.controlled_ticks).collect();
    let responsible_ticks: Vec<_> = rows.iter().map(|row| row.responsible_ticks).collect();
    let healthy_ticks: Vec<_> = rows
        .iter()
        .map(|row| row.healthy_responsible_ticks)
        .collect();
    let cumulative_points: Vec<_> = rows.iter().map(|row| row.totals.points_numerator).collect();
    let cumulative_epoch_weight: Vec<_> = rows.iter().map(|row| row.totals.epoch_weight).collect();
    let cumulative_acquisition: Vec<_> = rows
        .iter()
        .map(|row| row.totals.acquisition_numerator)
        .collect();
    let cumulative_control: Vec<_> = rows
        .iter()
        .map(|row| row.totals.control_numerator)
        .collect();
    let cumulative_sla: Vec<_> = rows.iter().map(|row| row.totals.sla_numerator).collect();
    let cumulative_rate_weight: Vec<_> = rows.iter().map(|row| row.totals.rate_weight).collect();
    let cumulative_windows: Vec<_> = rows
        .iter()
        .map(|row| row.totals.acquisition_windows)
        .collect();
    let cumulative_controlled: Vec<_> =
        rows.iter().map(|row| row.totals.controlled_ticks).collect();
    let cumulative_responsible: Vec<_> = rows
        .iter()
        .map(|row| row.totals.responsible_ticks)
        .collect();
    let cumulative_healthy: Vec<_> = rows
        .iter()
        .map(|row| row.totals.healthy_responsible_ticks)
        .collect();

    sqlx::query(
        r#"INSERT INTO "KothEpochTeamRollups"
             (game_id, epoch, participation_id, points,
              epoch_weight, acquisition_rate, control_rate, sla_rate,
              acquisition_windows, controlled_ticks, responsible_ticks,
              healthy_responsible_ticks, cumulative_points_numerator,
              cumulative_epoch_weight, cumulative_acquisition_numerator,
              cumulative_control_numerator, cumulative_sla_numerator,
              cumulative_rate_weight, cumulative_acquisition_windows,
              cumulative_controlled_ticks, cumulative_responsible_ticks,
              cumulative_healthy_responsible_ticks)
           SELECT $1, $2, row.*
             FROM UNNEST(
               $3::integer[], $4::float8[], $5::float8[], $6::float8[],
               $7::float8[], $8::float8[], $9::bigint[], $10::bigint[],
               $11::bigint[], $12::bigint[], $13::float8[], $14::float8[],
               $15::float8[], $16::float8[], $17::float8[], $18::float8[],
               $19::bigint[], $20::bigint[], $21::bigint[], $22::bigint[]
             ) AS row(
               participation_id, points, epoch_weight, acquisition_rate,
               control_rate, sla_rate, acquisition_windows, controlled_ticks,
               responsible_ticks, healthy_responsible_ticks,
               cumulative_points_numerator, cumulative_epoch_weight,
               cumulative_acquisition_numerator, cumulative_control_numerator,
               cumulative_sla_numerator, cumulative_rate_weight,
               cumulative_acquisition_windows, cumulative_controlled_ticks,
               cumulative_responsible_ticks,
               cumulative_healthy_responsible_ticks
             )
           ON CONFLICT (game_id, epoch, participation_id)
           DO NOTHING"#,
    )
    .bind(game_id)
    .bind(epoch)
    .bind(&participation_ids)
    .bind(&points)
    .bind(&epoch_weights)
    .bind(&acquisition_rates)
    .bind(&control_rates)
    .bind(&sla_rates)
    .bind(&acquisition_windows)
    .bind(&controlled_ticks)
    .bind(&responsible_ticks)
    .bind(&healthy_ticks)
    .bind(&cumulative_points)
    .bind(&cumulative_epoch_weight)
    .bind(&cumulative_acquisition)
    .bind(&cumulative_control)
    .bind(&cumulative_sla)
    .bind(&cumulative_rate_weight)
    .bind(&cumulative_windows)
    .bind(&cumulative_controlled)
    .bind(&cumulative_responsible)
    .bind(&cumulative_healthy)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn insert_hill_rows(
    connection: &mut PgConnection,
    game_id: i32,
    epoch: i32,
    rows: &[ComputedHillRow],
) -> AppResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let participation_ids: Vec<_> = rows.iter().map(|row| row.participation_id).collect();
    let challenge_ids: Vec<_> = rows.iter().map(|row| row.challenge_id).collect();
    let service_weights: Vec<_> = rows.iter().map(|row| row.service_weight).collect();
    let evidence_fractions: Vec<_> = rows.iter().map(|row| row.evidence_fraction).collect();
    let epoch_fractions: Vec<_> = rows.iter().map(|row| row.epoch_fraction).collect();
    let local_points: Vec<_> = rows.iter().map(|row| row.local_points).collect();
    let acquisition_rates: Vec<_> = rows.iter().map(|row| row.acquisition_rate).collect();
    let control_rates: Vec<_> = rows.iter().map(|row| row.control_rate).collect();
    let sla_rates: Vec<_> = rows.iter().map(|row| row.sla_rate).collect();
    let acquisition_windows: Vec<_> = rows.iter().map(|row| row.acquisition_windows).collect();
    let controlled_ticks: Vec<_> = rows.iter().map(|row| row.controlled_ticks).collect();
    let responsible_ticks: Vec<_> = rows.iter().map(|row| row.responsible_ticks).collect();
    let healthy_ticks: Vec<_> = rows
        .iter()
        .map(|row| row.healthy_responsible_ticks)
        .collect();
    let cumulative_points: Vec<_> = rows.iter().map(|row| row.totals.points_numerator).collect();
    let cumulative_score_weight: Vec<_> = rows.iter().map(|row| row.totals.score_weight).collect();
    let cumulative_acquisition: Vec<_> = rows
        .iter()
        .map(|row| row.totals.acquisition_numerator)
        .collect();
    let cumulative_control: Vec<_> = rows
        .iter()
        .map(|row| row.totals.control_numerator)
        .collect();
    let cumulative_sla: Vec<_> = rows.iter().map(|row| row.totals.sla_numerator).collect();
    let cumulative_rate_weight: Vec<_> = rows.iter().map(|row| row.totals.rate_weight).collect();
    let cumulative_windows: Vec<_> = rows
        .iter()
        .map(|row| row.totals.acquisition_windows)
        .collect();
    let cumulative_controlled: Vec<_> =
        rows.iter().map(|row| row.totals.controlled_ticks).collect();
    let cumulative_responsible: Vec<_> = rows
        .iter()
        .map(|row| row.totals.responsible_ticks)
        .collect();
    let cumulative_healthy: Vec<_> = rows
        .iter()
        .map(|row| row.totals.healthy_responsible_ticks)
        .collect();

    sqlx::query(
        r#"INSERT INTO "KothEpochHillRollups"
             (game_id, epoch, participation_id, challenge_id,
              service_weight, evidence_fraction, epoch_fraction, local_points,
              acquisition_rate, control_rate, sla_rate, acquisition_windows,
              controlled_ticks, responsible_ticks, healthy_responsible_ticks,
              cumulative_points_numerator, cumulative_score_weight,
              cumulative_acquisition_numerator, cumulative_control_numerator,
              cumulative_sla_numerator, cumulative_rate_weight,
              cumulative_acquisition_windows, cumulative_controlled_ticks,
              cumulative_responsible_ticks,
              cumulative_healthy_responsible_ticks)
           SELECT $1, $2, row.*
             FROM UNNEST(
               $3::integer[], $4::integer[], $5::float8[], $6::float8[],
               $7::float8[], $8::float8[], $9::float8[], $10::float8[],
               $11::float8[], $12::bigint[], $13::bigint[], $14::bigint[],
               $15::bigint[], $16::float8[], $17::float8[], $18::float8[],
               $19::float8[], $20::float8[], $21::float8[], $22::bigint[],
               $23::bigint[], $24::bigint[], $25::bigint[]
             ) AS row(
               participation_id, challenge_id, service_weight,
               evidence_fraction, epoch_fraction, local_points,
               acquisition_rate, control_rate, sla_rate, acquisition_windows,
               controlled_ticks, responsible_ticks, healthy_responsible_ticks,
               cumulative_points_numerator, cumulative_score_weight,
               cumulative_acquisition_numerator, cumulative_control_numerator,
               cumulative_sla_numerator, cumulative_rate_weight,
               cumulative_acquisition_windows, cumulative_controlled_ticks,
               cumulative_responsible_ticks,
               cumulative_healthy_responsible_ticks
             )
           ON CONFLICT (
             game_id, epoch, participation_id, challenge_id
           ) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(epoch)
    .bind(&participation_ids)
    .bind(&challenge_ids)
    .bind(&service_weights)
    .bind(&evidence_fractions)
    .bind(&epoch_fractions)
    .bind(&local_points)
    .bind(&acquisition_rates)
    .bind(&control_rates)
    .bind(&sla_rates)
    .bind(&acquisition_windows)
    .bind(&controlled_ticks)
    .bind(&responsible_ticks)
    .bind(&healthy_ticks)
    .bind(&cumulative_points)
    .bind(&cumulative_score_weight)
    .bind(&cumulative_acquisition)
    .bind(&cumulative_control)
    .bind(&cumulative_sla)
    .bind(&cumulative_rate_weight)
    .bind(&cumulative_windows)
    .bind(&cumulative_controlled)
    .bind(&cumulative_responsible)
    .bind(&cumulative_healthy)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
