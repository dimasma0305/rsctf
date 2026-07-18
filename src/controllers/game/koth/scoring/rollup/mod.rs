use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection, PgPool};

use super::{
    count, epoch_weight_fraction, evidence_fraction, hill_evidence_fraction, score_evidence_rows,
    HillEpochMetaRow, KothScoringSnapshot,
};
use crate::utils::error::{AppError, AppResult};

mod persistence;
use persistence::{insert_hill_rows, insert_team_rows};

const ROLLUP_LOCK_NAMESPACE: i32 = 0x4b4f_5448;

#[derive(Clone, Debug, FromRow)]
pub(super) struct RollupHeaderRow {
    pub(super) epoch: i32,
    pub(super) cumulative_scorable_ticks: i64,
    pub(super) cumulative_eligible_windows: i64,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct TeamRollupRow {
    pub(super) participation_id: i32,
    pub(super) cumulative_points_numerator: f64,
    pub(super) cumulative_epoch_weight: f64,
    pub(super) cumulative_acquisition_numerator: f64,
    pub(super) cumulative_control_numerator: f64,
    pub(super) cumulative_sla_numerator: f64,
    pub(super) cumulative_rate_weight: f64,
    pub(super) cumulative_acquisition_windows: i64,
    pub(super) cumulative_controlled_ticks: i64,
    pub(super) cumulative_responsible_ticks: i64,
    pub(super) cumulative_healthy_responsible_ticks: i64,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct HillRollupRow {
    pub(super) participation_id: i32,
    pub(super) challenge_id: i32,
    pub(super) service_weight: f64,
    pub(super) cumulative_points_numerator: f64,
    pub(super) cumulative_score_weight: f64,
    pub(super) cumulative_acquisition_numerator: f64,
    pub(super) cumulative_control_numerator: f64,
    pub(super) cumulative_sla_numerator: f64,
    pub(super) cumulative_rate_weight: f64,
    pub(super) cumulative_acquisition_windows: i64,
    pub(super) cumulative_controlled_ticks: i64,
    pub(super) cumulative_responsible_ticks: i64,
    pub(super) cumulative_healthy_responsible_ticks: i64,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct RecentTeamEpochRow {
    pub(super) participation_id: i32,
    pub(super) epoch: i32,
    pub(super) points: f64,
    pub(super) epoch_weight: f64,
    pub(super) cumulative_points_numerator: f64,
    pub(super) cumulative_epoch_weight: f64,
}

#[derive(Debug, FromRow)]
struct RollupGameRow {
    epoch_ticks: Option<i32>,
    cycle_ticks: Option<i32>,
    scoring_start_round: Option<i32>,
    end_time_utc: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Default)]
struct PreviousTeamTotals {
    points_numerator: f64,
    epoch_weight: f64,
    acquisition_numerator: f64,
    control_numerator: f64,
    sla_numerator: f64,
    rate_weight: f64,
    acquisition_windows: i64,
    controlled_ticks: i64,
    responsible_ticks: i64,
    healthy_responsible_ticks: i64,
}

#[derive(Clone, Copy, Debug, Default)]
struct PreviousHillTotals {
    points_numerator: f64,
    score_weight: f64,
    acquisition_numerator: f64,
    control_numerator: f64,
    sla_numerator: f64,
    rate_weight: f64,
    acquisition_windows: i64,
    controlled_ticks: i64,
    responsible_ticks: i64,
    healthy_responsible_ticks: i64,
}

#[derive(Debug)]
struct ComputedTeamRow {
    participation_id: i32,
    points: f64,
    epoch_weight: f64,
    acquisition_rate: f64,
    control_rate: f64,
    sla_rate: f64,
    acquisition_windows: i64,
    controlled_ticks: i64,
    responsible_ticks: i64,
    healthy_responsible_ticks: i64,
    totals: PreviousTeamTotals,
}

#[derive(Debug)]
struct ComputedHillRow {
    participation_id: i32,
    challenge_id: i32,
    service_weight: f64,
    evidence_fraction: f64,
    epoch_fraction: f64,
    local_points: f64,
    acquisition_rate: f64,
    control_rate: f64,
    sla_rate: f64,
    acquisition_windows: i64,
    controlled_ticks: i64,
    responsible_ticks: i64,
    healthy_responsible_ticks: i64,
    totals: PreviousHillTotals,
}

pub(crate) async fn lock_epoch_rollups(
    connection: &mut PgConnection,
    game_id: i32,
) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(ROLLUP_LOCK_NAMESPACE)
        .bind(game_id)
        .execute(connection)
        .await
        .map(|_| ())
        .map_err(|error| AppError::internal(error.to_string()))
}

/// Reopen a persisted partial tail when an event is extended. Shortening can
/// change the official cutoff anywhere, so it conservatively rebuilds all epochs.
pub(crate) async fn invalidate_rollups_for_end_change(
    connection: &mut PgConnection,
    game_id: i32,
    previous_end: DateTime<Utc>,
    next_end: DateTime<Utc>,
) -> AppResult<()> {
    if previous_end == next_end {
        return Ok(());
    }
    lock_epoch_rollups(connection, game_id).await?;
    sqlx::query(
        r#"DELETE FROM "KothEpochRollups" rollup
              USING "KothOfficialConfigs" config
             WHERE rollup.game_id = $1
               AND config.game_id = rollup.game_id
               AND ($2 OR rollup.round_count < config.epoch_ticks
                       OR rollup.finalized_round = rollup.end_round)"#,
    )
    .bind(game_id)
    .bind(next_end < previous_end)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Persist every newly immutable KotH epoch behind a per-game transaction lock.
pub(super) async fn ensure_epoch_rollups(
    pool: &PgPool,
    game_id: i32,
    now: DateTime<Utc>,
) -> AppResult<i32> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    lock_epoch_rollups(&mut transaction, game_id).await?;

    let game = sqlx::query_as::<_, RollupGameRow>(
        r#"SELECT config.epoch_ticks, config.cycle_ticks,
                  config.scoring_start_round,
                  game.end_time_utc
             FROM "Games" game
             LEFT JOIN "KothOfficialConfigs" config
               ON config.game_id = game.id
            WHERE game.id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(game) = game else {
        return Err(AppError::not_found("Game not found"));
    };
    let cycle_ticks = game.cycle_ticks.unwrap_or(1).clamp(1, 64);
    let Some(official_start_round) = game.scoring_start_round else {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(cycle_ticks);
    };
    let epoch_ticks = game.epoch_ticks.unwrap_or(1).clamp(1, 64);
    let ended = now >= game.end_time_utc;

    let previous_epoch = sqlx::query_scalar::<_, Option<i32>>(
        r#"SELECT MAX(epoch) FROM "KothEpochRollups"
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(0);
    let range_start =
        official_start_round.saturating_add(previous_epoch.saturating_mul(epoch_ticks));
    let round_cutoff = ended.then_some(game.end_time_utc);
    let checker_cutoff = ended.then_some(game.end_time_utc);
    let (meta, evidence) = super::evidence::load_evidence(
        &mut transaction,
        game_id,
        official_start_round,
        range_start,
        epoch_ticks,
        round_cutoff,
        checker_cutoff,
        ended,
    )
    .await?;
    let roster_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT value::integer
                 FROM "KothOfficialConfigs" config,
                      LATERAL jsonb_array_elements_text(config.roster_snapshot) value
                WHERE config.game_id = $1
                ORDER BY value::integer"#,
    )
    .bind(game_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut epochs: Vec<i32> = meta.iter().map(|row| row.epoch).collect();
    epochs.sort_unstable();
    epochs.dedup();
    let latest_visible_round = meta.iter().map(|row| row.end_round).max().unwrap_or(0);
    for epoch in epochs {
        let epoch_meta: Vec<_> = meta
            .iter()
            .filter(|row| row.epoch == epoch)
            .cloned()
            .collect();
        let epoch_evidence: Vec<_> = evidence
            .iter()
            .filter(|row| row.epoch == epoch)
            .cloned()
            .collect();
        let Some(first) = epoch_meta.first() else {
            continue;
        };
        let consistent = epoch_meta.iter().all(|row| {
            row.start_round == first.start_round
                && row.end_round == first.end_round
                && row.round_count == first.round_count
        });
        if !consistent {
            return Err(AppError::internal(format!(
                "KotH epoch {epoch} has inconsistent hill round ranges"
            )));
        }
        let complete_results = epoch_meta.iter().all(|row| {
            row.result_count == row.round_count && row.all_finalized && row.max_checked_at.is_some()
        });
        let full = first.round_count == i64::from(epoch_ticks);
        let partial_tail = ended && first.round_count < i64::from(epoch_ticks);
        if !complete_results || (!full && !partial_tail) {
            break;
        }
        let scored = score_evidence_rows(
            &epoch_meta,
            &epoch_evidence,
            &roster_ids,
            epoch_ticks,
            ended,
        )?;
        materialize_epoch(
            &mut transaction,
            game_id,
            epoch_ticks,
            epoch,
            if first.end_round < latest_visible_round {
                first.end_round.saturating_add(1)
            } else {
                first.end_round
            },
            &epoch_meta,
            &roster_ids,
            &scored,
        )
        .await?;
    }

    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(cycle_ticks)
}

async fn materialize_epoch(
    connection: &mut PgConnection,
    game_id: i32,
    epoch_ticks: i32,
    epoch: i32,
    finalized_round: i32,
    meta: &[HillEpochMetaRow],
    roster_ids: &[i32],
    scored: &KothScoringSnapshot,
) -> AppResult<()> {
    let exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
              SELECT 1 FROM "KothEpochRollups"
               WHERE game_id = $1 AND epoch = $2
           )"#,
    )
    .bind(game_id)
    .bind(epoch)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if exists {
        return Ok(());
    }
    let Some(first) = meta.first() else {
        return Ok(());
    };
    let previous_header = sqlx::query_as::<_, RollupHeaderRow>(
        r#"SELECT epoch, cumulative_scorable_ticks, cumulative_eligible_windows
             FROM "KothEpochRollups"
            WHERE game_id = $1 AND epoch < $2
            ORDER BY epoch DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(epoch)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let previous_teams = load_previous_teams(connection, game_id, epoch - 1).await?;
    let previous_hills = load_previous_hills(connection, game_id, epoch - 1).await?;
    let (teams, hills) = compute_rows(
        meta,
        roster_ids,
        scored,
        epoch_ticks,
        &previous_teams,
        &previous_hills,
    )?;
    let scorable_ticks = meta
        .iter()
        .map(|row| row.scorable_ticks.max(0))
        .sum::<i64>();
    let eligible_windows = meta
        .iter()
        .map(|row| row.eligible_windows.max(0))
        .sum::<i64>();
    let evidence_finalized_at = meta
        .iter()
        .filter_map(|row| row.max_checked_at)
        .max()
        .ok_or_else(|| AppError::internal("immutable KotH epoch has no checker timestamp"))?;
    let prior_scorable = previous_header
        .as_ref()
        .map_or(0, |row| row.cumulative_scorable_ticks);
    let prior_windows = previous_header
        .as_ref()
        .map_or(0, |row| row.cumulative_eligible_windows);
    let weight = epoch_weight_fraction(meta, epoch_ticks);

    sqlx::query(
        r#"INSERT INTO "KothEpochRollups"
             (game_id, epoch, start_round, end_round,
              round_count, epoch_weight, finalized_round, evidence_finalized_at,
              scorable_ticks, eligible_windows, cumulative_scorable_ticks,
              cumulative_eligible_windows)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
           ON CONFLICT (game_id, epoch) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(epoch)
    .bind(first.start_round)
    .bind(first.end_round)
    .bind(first.round_count as i32)
    .bind(weight)
    .bind(finalized_round)
    .bind(evidence_finalized_at)
    .bind(scorable_ticks)
    .bind(eligible_windows)
    .bind(prior_scorable.saturating_add(scorable_ticks))
    .bind(prior_windows.saturating_add(eligible_windows))
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    insert_team_rows(connection, game_id, epoch, &teams).await?;
    insert_hill_rows(connection, game_id, epoch, &hills).await
}

async fn load_previous_teams(
    connection: &mut PgConnection,
    game_id: i32,
    epoch: i32,
) -> AppResult<HashMap<i32, PreviousTeamTotals>> {
    let rows = sqlx::query_as::<_, TeamRollupRow>(
        r#"SELECT participation_id, cumulative_points_numerator,
                  cumulative_epoch_weight, cumulative_acquisition_numerator,
                  cumulative_control_numerator, cumulative_sla_numerator,
                  cumulative_rate_weight, cumulative_acquisition_windows,
                  cumulative_controlled_ticks, cumulative_responsible_ticks,
                  cumulative_healthy_responsible_ticks
             FROM "KothEpochTeamRollups"
            WHERE game_id = $1 AND epoch = $2"#,
    )
    .bind(game_id)
    .bind(epoch)
    .fetch_all(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.participation_id,
                PreviousTeamTotals {
                    points_numerator: row.cumulative_points_numerator,
                    epoch_weight: row.cumulative_epoch_weight,
                    acquisition_numerator: row.cumulative_acquisition_numerator,
                    control_numerator: row.cumulative_control_numerator,
                    sla_numerator: row.cumulative_sla_numerator,
                    rate_weight: row.cumulative_rate_weight,
                    acquisition_windows: row.cumulative_acquisition_windows,
                    controlled_ticks: row.cumulative_controlled_ticks,
                    responsible_ticks: row.cumulative_responsible_ticks,
                    healthy_responsible_ticks: row.cumulative_healthy_responsible_ticks,
                },
            )
        })
        .collect())
}

async fn load_previous_hills(
    connection: &mut PgConnection,
    game_id: i32,
    epoch: i32,
) -> AppResult<HashMap<(i32, i32), PreviousHillTotals>> {
    let rows = sqlx::query_as::<_, HillRollupRow>(
        r#"SELECT participation_id, challenge_id, service_weight,
                  cumulative_points_numerator, cumulative_score_weight,
                  cumulative_acquisition_numerator, cumulative_control_numerator,
                  cumulative_sla_numerator, cumulative_rate_weight,
                  cumulative_acquisition_windows, cumulative_controlled_ticks,
                  cumulative_responsible_ticks,
                  cumulative_healthy_responsible_ticks
             FROM "KothEpochHillRollups"
            WHERE game_id = $1 AND epoch = $2"#,
    )
    .bind(game_id)
    .bind(epoch)
    .fetch_all(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                (row.participation_id, row.challenge_id),
                PreviousHillTotals {
                    points_numerator: row.cumulative_points_numerator,
                    score_weight: row.cumulative_score_weight,
                    acquisition_numerator: row.cumulative_acquisition_numerator,
                    control_numerator: row.cumulative_control_numerator,
                    sla_numerator: row.cumulative_sla_numerator,
                    rate_weight: row.cumulative_rate_weight,
                    acquisition_windows: row.cumulative_acquisition_windows,
                    controlled_ticks: row.cumulative_controlled_ticks,
                    responsible_ticks: row.cumulative_responsible_ticks,
                    healthy_responsible_ticks: row.cumulative_healthy_responsible_ticks,
                },
            )
        })
        .collect())
}

fn compute_rows(
    meta: &[HillEpochMetaRow],
    roster_ids: &[i32],
    scored: &KothScoringSnapshot,
    epoch_ticks: i32,
    previous_teams: &HashMap<i32, PreviousTeamTotals>,
    previous_hills: &HashMap<(i32, i32), PreviousHillTotals>,
) -> AppResult<(Vec<ComputedTeamRow>, Vec<ComputedHillRow>)> {
    let epoch = meta.first().map_or(0, |row| row.epoch);
    let mut teams = Vec::with_capacity(roster_ids.len());
    let mut hills = Vec::with_capacity(roster_ids.len().saturating_mul(meta.len()));
    for &participation_id in roster_ids {
        let aggregate = scored
            .teams
            .get(&participation_id)
            .cloned()
            .unwrap_or_default();
        let epoch_score = aggregate
            .epochs
            .iter()
            .find(|summary| summary.epoch == epoch)
            .ok_or_else(|| AppError::internal("KotH rollup is missing a team epoch score"))?;
        let rate_weight = aggregate
            .cells
            .values()
            .map(|cell| cell.projected_weight * cell.service_weight)
            .sum::<f64>();
        let acquisition_windows = aggregate
            .cells
            .values()
            .map(|cell| cell.acquisition_windows)
            .sum::<i64>();
        let controlled_ticks = aggregate
            .cells
            .values()
            .map(|cell| cell.controlled_ticks)
            .sum::<i64>();
        let responsible_ticks = aggregate
            .cells
            .values()
            .map(|cell| cell.responsible_ticks)
            .sum::<i64>();
        let healthy_responsible_ticks = aggregate
            .cells
            .values()
            .map(|cell| cell.healthy_responsible_ticks)
            .sum::<i64>();
        let prior = previous_teams
            .get(&participation_id)
            .copied()
            .unwrap_or_default();
        teams.push(ComputedTeamRow {
            participation_id,
            points: epoch_score.points,
            epoch_weight: epoch_score.epoch_weight,
            acquisition_rate: aggregate.acquisition_rate,
            control_rate: aggregate.control_rate,
            sla_rate: aggregate.reliability_rate,
            acquisition_windows,
            controlled_ticks,
            responsible_ticks,
            healthy_responsible_ticks,
            totals: PreviousTeamTotals {
                points_numerator: prior.points_numerator
                    + epoch_score.points * epoch_score.epoch_weight,
                epoch_weight: prior.epoch_weight + epoch_score.epoch_weight,
                acquisition_numerator: prior.acquisition_numerator
                    + aggregate.acquisition_rate * rate_weight,
                control_numerator: prior.control_numerator + aggregate.control_rate * rate_weight,
                sla_numerator: prior.sla_numerator + aggregate.reliability_rate * rate_weight,
                rate_weight: prior.rate_weight + rate_weight,
                acquisition_windows: prior
                    .acquisition_windows
                    .saturating_add(acquisition_windows),
                controlled_ticks: prior.controlled_ticks.saturating_add(controlled_ticks),
                responsible_ticks: prior.responsible_ticks.saturating_add(responsible_ticks),
                healthy_responsible_ticks: prior
                    .healthy_responsible_ticks
                    .saturating_add(healthy_responsible_ticks),
            },
        });

        for row in meta {
            let cell = aggregate
                .cells
                .get(&row.challenge_id)
                .ok_or_else(|| AppError::internal("KotH rollup is missing a team-hill score"))?;
            let prior = previous_hills
                .get(&(participation_id, row.challenge_id))
                .copied()
                .unwrap_or_default();
            let score_weight = cell.projected_weight;
            let healthy_ticks = cell.healthy_responsible_ticks;
            hills.push(ComputedHillRow {
                participation_id,
                challenge_id: row.challenge_id,
                service_weight: cell.service_weight,
                evidence_fraction: hill_evidence_fraction(count(row.scorable_ticks)),
                epoch_fraction: if row.round_count == i64::from(epoch_ticks) {
                    1.0
                } else {
                    evidence_fraction(count(row.scorable_ticks), epoch_ticks.max(1) as u64)
                },
                local_points: cell.projected_points,
                acquisition_rate: cell.acquisition_rate,
                control_rate: cell.control_rate,
                sla_rate: cell.reliability_rate,
                acquisition_windows: cell.acquisition_windows,
                controlled_ticks: cell.controlled_ticks,
                responsible_ticks: cell.responsible_ticks,
                healthy_responsible_ticks: healthy_ticks,
                totals: PreviousHillTotals {
                    points_numerator: prior.points_numerator + cell.projected_points * score_weight,
                    score_weight: prior.score_weight + score_weight,
                    acquisition_numerator: prior.acquisition_numerator
                        + cell.acquisition_rate * score_weight,
                    control_numerator: prior.control_numerator + cell.control_rate * score_weight,
                    sla_numerator: prior.sla_numerator + cell.reliability_rate * score_weight,
                    rate_weight: prior.rate_weight + score_weight,
                    acquisition_windows: prior
                        .acquisition_windows
                        .saturating_add(cell.acquisition_windows),
                    controlled_ticks: prior.controlled_ticks.saturating_add(cell.controlled_ticks),
                    responsible_ticks: prior
                        .responsible_ticks
                        .saturating_add(cell.responsible_ticks),
                    healthy_responsible_ticks: prior
                        .healthy_responsible_ticks
                        .saturating_add(healthy_ticks),
                },
            });
        }
    }
    Ok((teams, hills))
}

pub(super) async fn load_rollup_snapshot(
    connection: &mut PgConnection,
    game_id: i32,
    round_cutoff: Option<DateTime<Utc>>,
    checker_cutoff: Option<DateTime<Utc>>,
) -> AppResult<(
    Option<RollupHeaderRow>,
    Vec<TeamRollupRow>,
    Vec<HillRollupRow>,
    Vec<RecentTeamEpochRow>,
)> {
    let header = sqlx::query_as::<_, RollupHeaderRow>(
        r#"SELECT epoch, cumulative_scorable_ticks, cumulative_eligible_windows
             FROM "KothEpochRollups" rollup
            WHERE game_id = $1
              AND ($2::timestamptz IS NULL OR rollup.finalized_round <= COALESCE(
                    (SELECT MAX(round.number) FROM "AdRounds" round
                      WHERE round.game_id = $1 AND round.start_time_utc <= $2), 0
                  ))
              AND ($3::timestamptz IS NULL
                   OR rollup.evidence_finalized_at <= $3)
              AND ($3::timestamptz IS NULL OR NOT EXISTS (
                    SELECT 1 FROM "KothEpochRollups" prior
                     WHERE prior.game_id = rollup.game_id
                       AND prior.epoch <= rollup.epoch
                       AND prior.evidence_finalized_at > $3
                  ))
            ORDER BY epoch DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(round_cutoff)
    .bind(checker_cutoff)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(header) = header else {
        return Ok((None, Vec::new(), Vec::new(), Vec::new()));
    };
    let teams = sqlx::query_as::<_, TeamRollupRow>(
        r#"SELECT participation_id, cumulative_points_numerator,
                  cumulative_epoch_weight, cumulative_acquisition_numerator,
                  cumulative_control_numerator, cumulative_sla_numerator,
                  cumulative_rate_weight, cumulative_acquisition_windows,
                  cumulative_controlled_ticks, cumulative_responsible_ticks,
                  cumulative_healthy_responsible_ticks
             FROM "KothEpochTeamRollups"
            WHERE game_id = $1 AND epoch = $2
            ORDER BY participation_id"#,
    )
    .bind(game_id)
    .bind(header.epoch)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let hills = sqlx::query_as::<_, HillRollupRow>(
        r#"SELECT participation_id, challenge_id, service_weight,
                  cumulative_points_numerator, cumulative_score_weight,
                  cumulative_acquisition_numerator, cumulative_control_numerator,
                  cumulative_sla_numerator, cumulative_rate_weight,
                  cumulative_acquisition_windows, cumulative_controlled_ticks,
                  cumulative_responsible_ticks,
                  cumulative_healthy_responsible_ticks
             FROM "KothEpochHillRollups"
            WHERE game_id = $1 AND epoch = $2
            ORDER BY participation_id, challenge_id"#,
    )
    .bind(game_id)
    .bind(header.epoch)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let recent = sqlx::query_as::<_, RecentTeamEpochRow>(
        r#"WITH recent_epochs AS (
               SELECT epoch FROM "KothEpochRollups"
                WHERE game_id = $1 AND epoch <= $2
                ORDER BY epoch DESC LIMIT 3
           )
           SELECT team.participation_id, team.epoch, team.points, team.epoch_weight,
                  team.cumulative_points_numerator,
                  team.cumulative_epoch_weight
             FROM "KothEpochTeamRollups" team
             JOIN recent_epochs recent ON recent.epoch = team.epoch
            WHERE team.game_id = $1
            ORDER BY team.participation_id, team.epoch"#,
    )
    .bind(game_id)
    .bind(header.epoch)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((Some(header), teams, hills, recent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controllers::game::koth::scoring::{
        KothCellAggregate, KothEpochAggregate, KothTeamAggregate,
    };

    #[test]
    fn cumulative_rows_keep_the_prior_prefix_exact() {
        let meta = vec![HillEpochMetaRow {
            challenge_id: 7,
            epoch: 2,
            start_round: 9,
            end_round: 16,
            service_weight: 1.0,
            round_count: 8,
            result_count: 8,
            scorable_ticks: 8,
            eligible_windows: 2,
            all_finalized: true,
            max_checked_at: Some(Utc::now()),
        }];
        let cell = KothCellAggregate {
            settled_points: 50.0,
            projected_points: 50.0,
            acquisition_rate: 0.5,
            control_rate: 0.5,
            reliability_rate: 1.0,
            acquisition_windows: 1,
            controlled_ticks: 4,
            responsible_ticks: 4,
            healthy_responsible_ticks: 4,
            projected_weight: 1.0,
            settled_weight: 1.0,
            service_weight: 1.0,
        };
        let snapshot = KothScoringSnapshot {
            teams: HashMap::from([(
                3,
                KothTeamAggregate {
                    settled_total: 50.0,
                    projected_total: 50.0,
                    acquisition_rate: 0.5,
                    control_rate: 0.5,
                    reliability_rate: 1.0,
                    cells: HashMap::from([(7, cell)]),
                    epochs: vec![KothEpochAggregate {
                        epoch: 2,
                        points: 50.0,
                        epoch_weight: 1.0,
                        finalized: true,
                        cumulative_points_numerator: 50.0,
                        cumulative_epoch_weight: 1.0,
                    }],
                },
            )]),
            fully_settled: false,
        };
        let previous_teams = HashMap::from([(
            3,
            PreviousTeamTotals {
                points_numerator: 30.0,
                epoch_weight: 1.0,
                acquisition_numerator: 0.25,
                control_numerator: 0.25,
                sla_numerator: 1.0,
                rate_weight: 1.0,
                acquisition_windows: 1,
                controlled_ticks: 2,
                responsible_ticks: 2,
                healthy_responsible_ticks: 2,
            },
        )]);
        let previous_hills = HashMap::from([(
            (3, 7),
            PreviousHillTotals {
                points_numerator: 30.0,
                score_weight: 1.0,
                acquisition_numerator: 0.25,
                control_numerator: 0.25,
                sla_numerator: 1.0,
                rate_weight: 1.0,
                acquisition_windows: 1,
                controlled_ticks: 2,
                responsible_ticks: 2,
                healthy_responsible_ticks: 2,
            },
        )]);

        let (teams, hills) =
            compute_rows(&meta, &[3], &snapshot, 8, &previous_teams, &previous_hills).unwrap();
        assert_eq!(teams[0].totals.points_numerator, 80.0);
        assert_eq!(teams[0].totals.epoch_weight, 2.0);
        assert_eq!(teams[0].totals.controlled_ticks, 6);
        assert_eq!(hills[0].totals.points_numerator, 80.0);
        assert_eq!(hills[0].totals.score_weight, 2.0);
        assert_eq!(hills[0].totals.acquisition_windows, 2);
    }

    #[test]
    fn void_only_epoch_has_zero_rollup_evidence_weight() {
        let meta = vec![HillEpochMetaRow {
            challenge_id: 7,
            epoch: 1,
            start_round: 1,
            end_round: 8,
            service_weight: 1.0,
            round_count: 8,
            result_count: 8,
            scorable_ticks: 0,
            eligible_windows: 0,
            all_finalized: true,
            max_checked_at: Some(Utc::now()),
        }];
        assert_eq!(epoch_weight_fraction(&meta, 8), 0.0);

        let cell = KothCellAggregate {
            service_weight: 1.0,
            ..KothCellAggregate::default()
        };
        let snapshot = KothScoringSnapshot {
            teams: HashMap::from([(
                3,
                KothTeamAggregate {
                    cells: HashMap::from([(7, cell)]),
                    epochs: vec![KothEpochAggregate {
                        epoch: 1,
                        points: 0.0,
                        epoch_weight: 0.0,
                        finalized: true,
                        cumulative_points_numerator: 0.0,
                        cumulative_epoch_weight: 0.0,
                    }],
                    ..KothTeamAggregate::default()
                },
            )]),
            fully_settled: false,
        };

        let (_, hills) =
            compute_rows(&meta, &[3], &snapshot, 8, &HashMap::new(), &HashMap::new()).unwrap();

        assert_eq!(hills[0].evidence_fraction, 0.0);
        assert_eq!(hills[0].totals.score_weight, 0.0);
    }
}
