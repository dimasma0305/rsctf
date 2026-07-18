use std::collections::HashMap;

use sqlx::{FromRow, PgConnection};

use super::evidence::EvidenceAggregateRow;
use super::{score_epoch_service, EpochServiceEvidence};
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Debug, FromRow)]
pub(super) struct ServiceRollupRow {
    pub participation_id: i32,
    pub challenge_id: i32,
    pub cumulative_points_numerator: f64,
    pub cumulative_epoch_weight: f64,
    pub cumulative_offense_numerator: f64,
    pub cumulative_defense_numerator: f64,
    pub cumulative_sla_numerator: f64,
    pub cumulative_capture_count: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct PreviousServiceTotals {
    points_numerator: f64,
    epoch_weight: f64,
    offense_numerator: f64,
    defense_numerator: f64,
    sla_numerator: f64,
    capture_count: i64,
}

#[derive(Clone, Copy, Debug)]
struct ComputedServiceValues {
    local_points: f64,
    offense_rate: f64,
    defense_rate: f64,
    sla_rate: f64,
    cumulative_points_numerator: f64,
    cumulative_epoch_weight: f64,
    cumulative_offense_numerator: f64,
    cumulative_defense_numerator: f64,
    cumulative_sla_numerator: f64,
    cumulative_capture_count: i64,
}

fn count(value: i64) -> i64 {
    value.max(0)
}

fn evidence(row: &EvidenceAggregateRow) -> EpochServiceEvidence {
    EpochServiceEvidence {
        opportunity_count: count(row.opportunity_count) as u64,
        capture_count: count(row.capture_count) as u64,
        rarity_sum: row.rarity_sum,
        defense_opportunity_count: count(row.defense_opportunity_count) as u64,
        protected_opportunity_count: count(row.protected_opportunity_count) as u64,
        sla_credit_sum: row.sla_credit_sum,
        sla_tick_count: count(row.sla_tick_count) as u64,
        service_weight: row.service_weight,
    }
}

fn compute_service_values(
    rows: &[EvidenceAggregateRow],
    epoch_weight: f64,
    previous: &[ServiceRollupRow],
) -> AppResult<Vec<ComputedServiceValues>> {
    let previous: HashMap<_, _> = previous
        .iter()
        .map(|row| {
            (
                (row.participation_id, row.challenge_id),
                PreviousServiceTotals {
                    points_numerator: row.cumulative_points_numerator,
                    epoch_weight: row.cumulative_epoch_weight,
                    offense_numerator: row.cumulative_offense_numerator,
                    defense_numerator: row.cumulative_defense_numerator,
                    sla_numerator: row.cumulative_sla_numerator,
                    capture_count: row.cumulative_capture_count,
                },
            )
        })
        .collect();
    let mut team_weights = HashMap::<i32, f64>::new();
    for row in rows {
        *team_weights.entry(row.participation_id).or_default() += row.service_weight;
    }

    rows.iter()
        .map(|row| {
            let score = score_epoch_service(&evidence(row))
                .map_err(|error| AppError::internal(error.to_string()))?;
            let prior = previous
                .get(&(row.participation_id, row.challenge_id))
                .copied()
                .unwrap_or_default();
            let team_weight = team_weights
                .get(&row.participation_id)
                .copied()
                .filter(|weight| *weight > 0.0)
                .ok_or_else(|| AppError::internal("A&D team has no service score weight"))?;
            let point_contribution = score.local_points * score.service_weight / team_weight;
            Ok(ComputedServiceValues {
                local_points: score.local_points,
                offense_rate: score.offense_rate,
                defense_rate: score.defense_rate,
                sla_rate: score.sla_rate,
                cumulative_points_numerator: prior.points_numerator
                    + point_contribution * epoch_weight,
                cumulative_epoch_weight: prior.epoch_weight + epoch_weight,
                cumulative_offense_numerator: prior.offense_numerator
                    + score.offense_rate * epoch_weight,
                cumulative_defense_numerator: prior.defense_numerator
                    + score.defense_rate * epoch_weight,
                cumulative_sla_numerator: prior.sla_numerator + score.sla_rate * epoch_weight,
                cumulative_capture_count: prior
                    .capture_count
                    .saturating_add(count(row.capture_count)),
            })
        })
        .collect()
}

pub(super) async fn load_service_rollups(
    connection: &mut PgConnection,
    game_id: i32,
    epoch: i32,
) -> AppResult<Vec<ServiceRollupRow>> {
    sqlx::query_as::<_, ServiceRollupRow>(
        r#"SELECT participation_id, challenge_id,
                  cumulative_points_numerator, cumulative_epoch_weight,
                  cumulative_offense_numerator, cumulative_defense_numerator,
                  cumulative_sla_numerator, cumulative_capture_count
             FROM "AdEpochServiceRollups"
            WHERE game_id = $1 AND epoch = $2
            ORDER BY participation_id, challenge_id"#,
    )
    .bind(game_id)
    .bind(epoch)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn insert_service_rows(
    connection: &mut PgConnection,
    game_id: i32,
    epoch: i32,
    epoch_weight: f64,
    rows: &[EvidenceAggregateRow],
    previous: &[ServiceRollupRow],
) -> AppResult<()> {
    let computed = compute_service_values(rows, epoch_weight, previous)?;
    let participation_ids: Vec<_> = rows.iter().map(|row| row.participation_id).collect();
    let challenge_ids: Vec<_> = rows.iter().map(|row| row.challenge_id).collect();
    let weights: Vec<_> = rows.iter().map(|row| row.service_weight).collect();
    let opportunities: Vec<_> = rows
        .iter()
        .map(|row| count(row.opportunity_count))
        .collect();
    let captures: Vec<_> = rows.iter().map(|row| count(row.capture_count)).collect();
    let rarity: Vec<_> = rows.iter().map(|row| row.rarity_sum).collect();
    let defense_opportunities: Vec<_> = rows
        .iter()
        .map(|row| count(row.defense_opportunity_count))
        .collect();
    let protected: Vec<_> = rows
        .iter()
        .map(|row| count(row.protected_opportunity_count))
        .collect();
    let sla_credit: Vec<_> = rows.iter().map(|row| row.sla_credit_sum).collect();
    let sla_ticks: Vec<_> = rows.iter().map(|row| count(row.sla_tick_count)).collect();
    let closing_status: Vec<_> = rows.iter().map(|row| row.closing_sla_status).collect();
    let closing_credit: Vec<_> = rows.iter().map(|row| row.closing_sla_credit).collect();
    let local_points: Vec<_> = computed.iter().map(|row| row.local_points).collect();
    let offense_rates: Vec<_> = computed.iter().map(|row| row.offense_rate).collect();
    let defense_rates: Vec<_> = computed.iter().map(|row| row.defense_rate).collect();
    let sla_rates: Vec<_> = computed.iter().map(|row| row.sla_rate).collect();
    let points_numerators: Vec<_> = computed
        .iter()
        .map(|row| row.cumulative_points_numerator)
        .collect();
    let epoch_weights: Vec<_> = computed
        .iter()
        .map(|row| row.cumulative_epoch_weight)
        .collect();
    let offense_numerators: Vec<_> = computed
        .iter()
        .map(|row| row.cumulative_offense_numerator)
        .collect();
    let defense_numerators: Vec<_> = computed
        .iter()
        .map(|row| row.cumulative_defense_numerator)
        .collect();
    let sla_numerators: Vec<_> = computed
        .iter()
        .map(|row| row.cumulative_sla_numerator)
        .collect();
    let cumulative_captures: Vec<_> = computed
        .iter()
        .map(|row| row.cumulative_capture_count)
        .collect();

    sqlx::query(
        r#"INSERT INTO "AdEpochServiceRollups"
             (game_id, epoch, participation_id, challenge_id, service_weight,
              opportunity_count, capture_count, rarity_sum,
              defense_opportunity_count, protected_opportunity_count,
              sla_credit_sum, sla_tick_count, closing_sla_status, closing_sla_credit,
              local_points, offense_rate, defense_rate, sla_rate,
              cumulative_points_numerator, cumulative_epoch_weight,
              cumulative_offense_numerator, cumulative_defense_numerator,
              cumulative_sla_numerator, cumulative_capture_count)
           SELECT $1, $2, row.*
             FROM UNNEST(
               $3::integer[], $4::integer[], $5::float8[], $6::bigint[],
               $7::bigint[], $8::float8[], $9::bigint[], $10::bigint[],
               $11::float8[], $12::bigint[], $13::smallint[], $14::float8[],
               $15::float8[], $16::float8[], $17::float8[], $18::float8[],
               $19::float8[], $20::float8[], $21::float8[], $22::float8[],
               $23::float8[], $24::bigint[]
             ) AS row(
               participation_id, challenge_id, service_weight, opportunity_count,
               capture_count, rarity_sum, defense_opportunity_count,
               protected_opportunity_count, sla_credit_sum, sla_tick_count,
               closing_sla_status, closing_sla_credit, local_points, offense_rate,
               defense_rate, sla_rate, cumulative_points_numerator,
               cumulative_epoch_weight, cumulative_offense_numerator,
               cumulative_defense_numerator, cumulative_sla_numerator,
               cumulative_capture_count
             )
           ON CONFLICT (game_id, epoch, participation_id, challenge_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(epoch)
    .bind(&participation_ids)
    .bind(&challenge_ids)
    .bind(&weights)
    .bind(&opportunities)
    .bind(&captures)
    .bind(&rarity)
    .bind(&defense_opportunities)
    .bind(&protected)
    .bind(&sla_credit)
    .bind(&sla_ticks)
    .bind(&closing_status)
    .bind(&closing_credit)
    .bind(&local_points)
    .bind(&offense_rates)
    .bind(&defense_rates)
    .bind(&sla_rates)
    .bind(&points_numerators)
    .bind(&epoch_weights)
    .bind(&offense_numerators)
    .bind(&defense_numerators)
    .bind(&sla_numerators)
    .bind(&cumulative_captures)
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence_row() -> EvidenceAggregateRow {
        EvidenceAggregateRow {
            participation_id: 1,
            challenge_id: 2,
            epoch: 2,
            service_weight: 1.0,
            opportunity_count: 10,
            capture_count: 5,
            rarity_sum: 1.0,
            defense_opportunity_count: 10,
            protected_opportunity_count: 8,
            sla_credit_sum: 9.0,
            sla_tick_count: 10,
            eligible_flags_total: 10,
            captured_flags_total: 5,
            accepted_captures_total: 5,
            defense_opportunities_total: 10,
            protected_opportunities_total: 8,
            closing_sla_status: Some(0),
            closing_sla_credit: Some(1.0),
        }
    }

    #[test]
    fn partial_epoch_extends_service_cumulative_values_exactly() {
        let previous = ServiceRollupRow {
            participation_id: 1,
            challenge_id: 2,
            cumulative_points_numerator: 40.0,
            cumulative_epoch_weight: 1.0,
            cumulative_offense_numerator: 0.4,
            cumulative_defense_numerator: 0.6,
            cumulative_sla_numerator: 0.8,
            cumulative_capture_count: 7,
        };
        let row = evidence_row();
        let score = score_epoch_service(&evidence(&row)).unwrap();
        let result = compute_service_values(&[row], 0.5, &[previous]).unwrap();
        let result = result[0];

        assert!((result.cumulative_epoch_weight - 1.5).abs() < 1e-12);
        assert!(
            (result.cumulative_points_numerator - (40.0 + score.local_points * 0.5)).abs() < 1e-12
        );
        assert!(
            (result.cumulative_offense_numerator - (0.4 + score.offense_rate * 0.5)).abs() < 1e-12
        );
        assert_eq!(result.cumulative_capture_count, 12);
    }

    #[test]
    fn unequal_service_weights_produce_additive_team_contributions() {
        let left = evidence_row();
        let mut right = evidence_row();
        right.challenge_id = 3;
        right.service_weight = 1.2;
        right.capture_count = 10;
        right.protected_opportunity_count = 10;
        right.sla_credit_sum = 10.0;
        let left_score = score_epoch_service(&evidence(&left)).unwrap();
        let right_score = score_epoch_service(&evidence(&right)).unwrap();
        let expected_team = (left_score.local_points * left.service_weight
            + right_score.local_points * right.service_weight)
            / (left.service_weight + right.service_weight);

        let computed = compute_service_values(&[left, right], 0.25, &[]).unwrap();
        let displayed_sum = computed
            .iter()
            .map(|row| row.cumulative_points_numerator / row.cumulative_epoch_weight)
            .sum::<f64>();
        assert!((displayed_sum - expected_team).abs() < 1e-12);
    }

    #[test]
    fn each_epoch_normalizes_its_own_service_weights() {
        let mut first_left = evidence_row();
        first_left.service_weight = 0.8;
        let mut first_right = evidence_row();
        first_right.challenge_id = 3;
        first_right.service_weight = 1.2;
        first_right.capture_count = 10;
        let first =
            compute_service_values(&[first_left.clone(), first_right.clone()], 1.0, &[]).unwrap();
        let previous: Vec<_> = [first_left.clone(), first_right.clone()]
            .into_iter()
            .zip(first)
            .map(|(row, value)| ServiceRollupRow {
                participation_id: row.participation_id,
                challenge_id: row.challenge_id,
                cumulative_points_numerator: value.cumulative_points_numerator,
                cumulative_epoch_weight: value.cumulative_epoch_weight,
                cumulative_offense_numerator: value.cumulative_offense_numerator,
                cumulative_defense_numerator: value.cumulative_defense_numerator,
                cumulative_sla_numerator: value.cumulative_sla_numerator,
                cumulative_capture_count: value.cumulative_capture_count,
            })
            .collect();

        let mut second_left = first_left;
        second_left.service_weight = 1.2;
        let mut second_right = first_right;
        second_right.service_weight = 0.8;
        let second_scores = [
            score_epoch_service(&evidence(&second_left)).unwrap(),
            score_epoch_service(&evidence(&second_right)).unwrap(),
        ];
        let second = compute_service_values(&[second_left, second_right], 0.5, &previous).unwrap();
        let displayed_sum = second
            .iter()
            .map(|row| row.cumulative_points_numerator / row.cumulative_epoch_weight)
            .sum::<f64>();
        let first_team = previous
            .iter()
            .map(|row| row.cumulative_points_numerator)
            .sum::<f64>();
        let second_team =
            (second_scores[0].local_points * 1.2 + second_scores[1].local_points * 0.8) / 2.0;
        let expected = (first_team + second_team * 0.5) / 1.5;
        assert!((displayed_sum - expected).abs() < 1e-12);
    }
}
