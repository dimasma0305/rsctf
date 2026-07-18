use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};

mod evidence;
mod rollup;

use rollup::load_rollup_snapshot;
pub(crate) use rollup::{invalidate_rollups_for_end_change, lock_epoch_rollups};

use super::scoring_formula::{
    aggregate_epoch_hills, average_weighted_epochs, evidence_fraction, score_epoch_hill,
    KothEpochHillEvidence, WeightedHillScore,
};
use crate::utils::database::begin_read_only_repeatable_read;
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Debug, FromRow)]
struct HillEpochMetaRow {
    challenge_id: i32,
    epoch: i32,
    start_round: i32,
    end_round: i32,
    service_weight: f64,
    round_count: i64,
    result_count: i64,
    scorable_ticks: i64,
    eligible_windows: i64,
    all_finalized: bool,
    max_checked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, FromRow)]
struct TeamEvidenceRow {
    participation_id: i32,
    challenge_id: i32,
    epoch: i32,
    acquisition_windows: i64,
    controlled_ticks: i64,
    responsible_ticks: i64,
    healthy_responsible_ticks: i64,
    personal_scorable_ticks: i64,
    personal_eligible_windows: i64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct KothCellAggregate {
    pub(super) settled_points: f64,
    pub(super) projected_points: f64,
    pub(super) acquisition_rate: f64,
    pub(super) control_rate: f64,
    pub(super) reliability_rate: f64,
    pub(super) acquisition_windows: i64,
    pub(super) controlled_ticks: i64,
    pub(super) responsible_ticks: i64,
    pub(super) healthy_responsible_ticks: i64,
    projected_weight: f64,
    settled_weight: f64,
    service_weight: f64,
}

#[derive(Clone, Debug)]
pub(super) struct KothEpochAggregate {
    pub(super) epoch: i32,
    pub(super) points: f64,
    pub(super) epoch_weight: f64,
    pub(super) finalized: bool,
    /// Exact cumulative prefix used by the timeline. Recent durable epochs can
    /// start after epoch one, so replaying only the displayed suffix is wrong.
    pub(super) cumulative_points_numerator: f64,
    pub(super) cumulative_epoch_weight: f64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct KothTeamAggregate {
    pub(super) settled_total: f64,
    pub(super) projected_total: f64,
    pub(super) acquisition_rate: f64,
    pub(super) control_rate: f64,
    pub(super) reliability_rate: f64,
    pub(super) cells: HashMap<i32, KothCellAggregate>,
    pub(super) epochs: Vec<KothEpochAggregate>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct KothScoringSnapshot {
    pub(super) teams: HashMap<i32, KothTeamAggregate>,
    pub(super) fully_settled: bool,
}

#[derive(Clone, Debug)]
struct ScoredCellEpoch {
    challenge_id: i32,
    score: super::scoring_formula::KothEpochHillScore,
    evidence_fraction: f64,
    epoch_fraction: f64,
    finalized: bool,
    acquisition_windows: i64,
    controlled_ticks: i64,
    responsible_ticks: i64,
    healthy_responsible_ticks: i64,
}

fn count(value: i64) -> u64 {
    value.max(0) as u64
}

/// Whether this hill has any field-wide scorable evidence in the epoch.
///
/// A wholly unavailable hill must not consume service weight as though every
/// team scored zero. Once at least one scorable tick exists, void samples stay
/// excluded from personal denominators without reducing the hill's influence.
fn hill_evidence_fraction(scorable_ticks: u64) -> f64 {
    if scorable_ticks == 0 {
        0.0
    } else {
        1.0
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

fn epoch_weight_fraction<'a>(
    meta: impl IntoIterator<Item = &'a HillEpochMetaRow>,
    epoch_ticks: i32,
) -> f64 {
    let mut rows = meta.into_iter();
    let Some(first) = rows.next() else {
        return 0.0;
    };
    let expected = epoch_ticks.max(1) as u64;
    let (total_weight, played, complete) = rows.fold(
        (
            first.service_weight,
            first.service_weight * count(first.scorable_ticks) as f64,
            count(first.round_count) == expected,
        ),
        |(total_weight, played, complete), row| {
            (
                total_weight + row.service_weight,
                played + row.service_weight * count(row.scorable_ticks) as f64,
                complete && count(row.round_count) == expected,
            )
        },
    );
    if played <= 0.0 {
        0.0
    } else if complete {
        1.0
    } else {
        ratio(played, total_weight * expected as f64).clamp(0.0, 1.0)
    }
}

fn score_evidence_rows(
    meta: &[HillEpochMetaRow],
    team_evidence: &[TeamEvidenceRow],
    roster_ids: &[i32],
    epoch_ticks: i32,
    event_ended: bool,
) -> AppResult<KothScoringSnapshot> {
    if meta.is_empty() {
        return Ok(KothScoringSnapshot::default());
    }

    let team_evidence: HashMap<(i32, i32, i32), &TeamEvidenceRow> = team_evidence
        .iter()
        .map(|row| ((row.participation_id, row.challenge_id, row.epoch), row))
        .collect();

    let mut scored_by_team_epoch = BTreeMap::<(i32, i32), Vec<ScoredCellEpoch>>::new();
    let mut epoch_hills = BTreeMap::<i32, HashMap<i32, (f64, f64, bool)>>::new();
    let mut meta_by_epoch = BTreeMap::<i32, Vec<&HillEpochMetaRow>>::new();
    for row in meta {
        meta_by_epoch.entry(row.epoch).or_default().push(row);
        let round_count = count(row.round_count);
        let scorable_ticks = count(row.scorable_ticks);
        let evidence_weight = hill_evidence_fraction(scorable_ticks);
        let epoch_fraction = if round_count == epoch_ticks as u64 {
            1.0
        } else {
            evidence_fraction(scorable_ticks, epoch_ticks as u64)
        };
        let finalized = row.all_finalized
            && row.result_count == row.round_count
            && (row.round_count == i64::from(epoch_ticks) || event_ended);
        epoch_hills.entry(row.epoch).or_default().insert(
            row.challenge_id,
            (row.service_weight, evidence_weight, finalized),
        );
        for &participation_id in roster_ids {
            let team = team_evidence.get(&(participation_id, row.challenge_id, row.epoch));
            let acquisition_windows = team.map_or(0, |team| team.acquisition_windows);
            let controlled_ticks = team.map_or(0, |team| team.controlled_ticks);
            let responsible_ticks = team.map_or(0, |team| team.responsible_ticks);
            let healthy_responsible_ticks = team.map_or(0, |team| team.healthy_responsible_ticks);
            let personal_scorable_ticks = team.map_or(0, |team| team.personal_scorable_ticks);
            let personal_eligible_windows = team.map_or(0, |team| team.personal_eligible_windows);
            let evidence = KothEpochHillEvidence {
                scorable_ticks: personal_scorable_ticks,
                acquisition_windows,
                eligible_windows: personal_eligible_windows,
                controlled_ticks,
                responsible_ticks,
                healthy_responsible_ticks,
                service_weight: row.service_weight,
            };
            let score = score_epoch_hill(&evidence)
                .map_err(|error| AppError::internal(error.to_string()))?;
            scored_by_team_epoch
                .entry((participation_id, row.epoch))
                .or_default()
                .push(ScoredCellEpoch {
                    challenge_id: row.challenge_id,
                    score,
                    evidence_fraction: evidence_weight,
                    epoch_fraction,
                    finalized,
                    acquisition_windows,
                    controlled_ticks,
                    responsible_ticks,
                    healthy_responsible_ticks,
                });
        }
    }

    let epoch_weights: HashMap<i32, f64> = meta_by_epoch
        .into_iter()
        .map(|(epoch, rows)| (epoch, epoch_weight_fraction(rows, epoch_ticks)))
        .collect();

    let mut teams = HashMap::<i32, KothTeamAggregate>::new();
    for ((participation_id, epoch), cells) in scored_by_team_epoch {
        let weighted_hills: Vec<_> = cells
            .iter()
            .map(|cell| WeightedHillScore {
                score: cell.score,
                evidence_fraction: cell.evidence_fraction,
            })
            .collect();
        let epoch_score = aggregate_epoch_hills(&weighted_hills)
            .map_err(|error| AppError::internal(error.to_string()))?;
        let finalized = cells.iter().all(|cell| cell.finalized);
        let epoch_weight = epoch_weights.get(&epoch).copied().unwrap_or(0.0);
        let team = teams.entry(participation_id).or_default();
        team.epochs.push(KothEpochAggregate {
            epoch,
            points: epoch_score.points,
            epoch_weight,
            finalized,
            cumulative_points_numerator: 0.0,
            cumulative_epoch_weight: 0.0,
        });

        for cell in cells {
            let aggregate = team.cells.entry(cell.challenge_id).or_default();
            let weight = cell.epoch_fraction * cell.evidence_fraction;
            aggregate.service_weight = cell.score.service_weight;
            aggregate.projected_weight += weight;
            aggregate.projected_points += cell.score.local_points * weight;
            aggregate.acquisition_rate += cell.score.acquisition_rate * weight;
            aggregate.control_rate += cell.score.control_rate * weight;
            aggregate.reliability_rate += cell.score.reliability_rate * weight;
            aggregate.acquisition_windows += cell.acquisition_windows;
            aggregate.controlled_ticks += cell.controlled_ticks;
            aggregate.responsible_ticks += cell.responsible_ticks;
            aggregate.healthy_responsible_ticks += cell.healthy_responsible_ticks;
            if cell.finalized {
                aggregate.settled_weight += weight;
                aggregate.settled_points += cell.score.local_points * weight;
            }
        }
    }

    for team in teams.values_mut() {
        team.projected_total = average_weighted_epochs(
            &team
                .epochs
                .iter()
                .map(|epoch| (epoch.points, epoch.epoch_weight))
                .collect::<Vec<_>>(),
        )
        .map_err(|error| AppError::internal(error.to_string()))?;
        team.settled_total = average_weighted_epochs(
            &team
                .epochs
                .iter()
                .filter(|epoch| epoch.finalized)
                .map(|epoch| (epoch.points, epoch.epoch_weight))
                .collect::<Vec<_>>(),
        )
        .map_err(|error| AppError::internal(error.to_string()))?;

        let mut team_rate_weight = 0.0;
        for aggregate in team.cells.values_mut() {
            aggregate.projected_points =
                ratio(aggregate.projected_points, aggregate.projected_weight);
            aggregate.settled_points = ratio(aggregate.settled_points, aggregate.settled_weight);
            aggregate.acquisition_rate =
                ratio(aggregate.acquisition_rate, aggregate.projected_weight);
            aggregate.control_rate = ratio(aggregate.control_rate, aggregate.projected_weight);
            aggregate.reliability_rate =
                ratio(aggregate.reliability_rate, aggregate.projected_weight);
            let rate_weight = aggregate.projected_weight * aggregate.service_weight;
            team.acquisition_rate += aggregate.acquisition_rate * rate_weight;
            team.control_rate += aggregate.control_rate * rate_weight;
            team.reliability_rate += aggregate.reliability_rate * rate_weight;
            team_rate_weight += rate_weight;
        }
        team.acquisition_rate = ratio(team.acquisition_rate, team_rate_weight);
        team.control_rate = ratio(team.control_rate, team_rate_weight);
        team.reliability_rate = ratio(team.reliability_rate, team_rate_weight);
        team.epochs.sort_by_key(|epoch| epoch.epoch);
        let mut points_numerator = 0.0;
        let mut epoch_weight = 0.0;
        for epoch in &mut team.epochs {
            points_numerator += epoch.points * epoch.epoch_weight;
            epoch_weight += epoch.epoch_weight;
            epoch.cumulative_points_numerator = points_numerator;
            epoch.cumulative_epoch_weight = epoch_weight;
        }
    }

    let fully_settled = event_ended
        && !epoch_hills.is_empty()
        && epoch_hills
            .values()
            .all(|hills| hills.values().all(|(_, _, finalized)| *finalized));
    Ok(KothScoringSnapshot {
        teams,
        fully_settled,
    })
}

fn merge_rollup_prefix(
    roster_ids: &[i32],
    rollup_header: Option<&rollup::RollupHeaderRow>,
    rollup_teams: Vec<rollup::TeamRollupRow>,
    rollup_hills: Vec<rollup::HillRollupRow>,
    recent_epochs: Vec<rollup::RecentTeamEpochRow>,
    mut raw: KothScoringSnapshot,
    event_ended: bool,
    current_epoch: i32,
) -> KothScoringSnapshot {
    let rollup_teams: HashMap<_, _> = rollup_teams
        .into_iter()
        .map(|row| (row.participation_id, row))
        .collect();
    let mut hills_by_team = HashMap::<i32, Vec<rollup::HillRollupRow>>::new();
    for row in rollup_hills {
        hills_by_team
            .entry(row.participation_id)
            .or_default()
            .push(row);
    }
    let mut recent_by_team = HashMap::<i32, Vec<rollup::RecentTeamEpochRow>>::new();
    for row in recent_epochs {
        recent_by_team
            .entry(row.participation_id)
            .or_default()
            .push(row);
    }

    let mut teams = HashMap::with_capacity(roster_ids.len());
    for &participation_id in roster_ids {
        let previous = rollup_teams.get(&participation_id);
        let mut aggregate = KothTeamAggregate::default();
        let mut points_numerator = previous.map_or(0.0, |row| row.cumulative_points_numerator);
        let mut settled_points_numerator = points_numerator;
        let mut projected_epoch_weight = previous.map_or(0.0, |row| row.cumulative_epoch_weight);
        let mut settled_epoch_weight = projected_epoch_weight;
        let mut acquisition_numerator =
            previous.map_or(0.0, |row| row.cumulative_acquisition_numerator);
        let mut control_numerator = previous.map_or(0.0, |row| row.cumulative_control_numerator);
        let mut reliability_numerator = previous.map_or(0.0, |row| row.cumulative_sla_numerator);
        let mut rate_weight = previous.map_or(0.0, |row| row.cumulative_rate_weight);

        for row in hills_by_team.remove(&participation_id).unwrap_or_default() {
            aggregate.cells.insert(
                row.challenge_id,
                KothCellAggregate {
                    settled_points: ratio(
                        row.cumulative_points_numerator,
                        row.cumulative_score_weight,
                    ),
                    projected_points: ratio(
                        row.cumulative_points_numerator,
                        row.cumulative_score_weight,
                    ),
                    acquisition_rate: ratio(
                        row.cumulative_acquisition_numerator,
                        row.cumulative_rate_weight,
                    ),
                    control_rate: ratio(
                        row.cumulative_control_numerator,
                        row.cumulative_rate_weight,
                    ),
                    reliability_rate: ratio(
                        row.cumulative_sla_numerator,
                        row.cumulative_rate_weight,
                    ),
                    acquisition_windows: row.cumulative_acquisition_windows,
                    controlled_ticks: row.cumulative_controlled_ticks,
                    responsible_ticks: row.cumulative_responsible_ticks,
                    healthy_responsible_ticks: row.cumulative_healthy_responsible_ticks,
                    projected_weight: row.cumulative_score_weight,
                    settled_weight: row.cumulative_score_weight,
                    service_weight: row.service_weight,
                },
            );
        }

        let mut raw_team = raw.teams.remove(&participation_id).unwrap_or_default();
        for epoch in &mut raw_team.epochs {
            points_numerator += epoch.points * epoch.epoch_weight;
            projected_epoch_weight += epoch.epoch_weight;
            epoch.cumulative_points_numerator = points_numerator;
            epoch.cumulative_epoch_weight = projected_epoch_weight;
            if epoch.finalized {
                settled_points_numerator += epoch.points * epoch.epoch_weight;
                settled_epoch_weight += epoch.epoch_weight;
            }
        }
        let raw_rate_weight = raw_team
            .cells
            .values()
            .map(|cell| cell.projected_weight * cell.service_weight)
            .sum::<f64>();
        acquisition_numerator += raw_team.acquisition_rate * raw_rate_weight;
        control_numerator += raw_team.control_rate * raw_rate_weight;
        reliability_numerator += raw_team.reliability_rate * raw_rate_weight;
        rate_weight += raw_rate_weight;

        for (challenge_id, raw_cell) in raw_team.cells {
            let cell = aggregate.cells.entry(challenge_id).or_default();
            let projected_points_numerator = cell.projected_points * cell.projected_weight
                + raw_cell.projected_points * raw_cell.projected_weight;
            let settled_cell_numerator = cell.settled_points * cell.settled_weight
                + raw_cell.settled_points * raw_cell.settled_weight;
            let acquisition_cell_numerator = cell.acquisition_rate * cell.projected_weight
                + raw_cell.acquisition_rate * raw_cell.projected_weight;
            let control_cell_numerator = cell.control_rate * cell.projected_weight
                + raw_cell.control_rate * raw_cell.projected_weight;
            let reliability_cell_numerator = cell.reliability_rate * cell.projected_weight
                + raw_cell.reliability_rate * raw_cell.projected_weight;
            cell.projected_weight += raw_cell.projected_weight;
            cell.settled_weight += raw_cell.settled_weight;
            cell.projected_points = ratio(projected_points_numerator, cell.projected_weight);
            cell.settled_points = ratio(settled_cell_numerator, cell.settled_weight);
            cell.acquisition_rate = ratio(acquisition_cell_numerator, cell.projected_weight);
            cell.control_rate = ratio(control_cell_numerator, cell.projected_weight);
            cell.reliability_rate = ratio(reliability_cell_numerator, cell.projected_weight);
            cell.acquisition_windows = cell
                .acquisition_windows
                .saturating_add(raw_cell.acquisition_windows);
            cell.controlled_ticks = cell
                .controlled_ticks
                .saturating_add(raw_cell.controlled_ticks);
            cell.responsible_ticks = cell
                .responsible_ticks
                .saturating_add(raw_cell.responsible_ticks);
            cell.healthy_responsible_ticks = cell
                .healthy_responsible_ticks
                .saturating_add(raw_cell.healthy_responsible_ticks);
            cell.service_weight = raw_cell.service_weight;
        }

        aggregate.projected_total = ratio(points_numerator, projected_epoch_weight);
        aggregate.settled_total = ratio(settled_points_numerator, settled_epoch_weight);
        aggregate.acquisition_rate = ratio(acquisition_numerator, rate_weight);
        aggregate.control_rate = ratio(control_numerator, rate_weight);
        aggregate.reliability_rate = ratio(reliability_numerator, rate_weight);
        aggregate.epochs = recent_by_team
            .remove(&participation_id)
            .unwrap_or_default()
            .into_iter()
            .map(|row| KothEpochAggregate {
                epoch: row.epoch,
                points: row.points,
                epoch_weight: row.epoch_weight,
                finalized: true,
                cumulative_points_numerator: row.cumulative_points_numerator,
                cumulative_epoch_weight: row.cumulative_epoch_weight,
            })
            .chain(raw_team.epochs)
            .collect();
        aggregate.epochs.sort_by_key(|epoch| epoch.epoch);
        teams.insert(participation_id, aggregate);
    }

    KothScoringSnapshot {
        teams,
        fully_settled: event_ended
            && current_epoch > 0
            && rollup_header.is_some_and(|header| header.epoch >= current_epoch),
    }
}

pub(super) async fn load_koth_scoring(
    pool: &PgPool,
    game_id: i32,
    cutoff: Option<DateTime<Utc>>,
    event_ended: bool,
) -> AppResult<KothScoringSnapshot> {
    let Some(official) = evidence::load_official_config(pool, game_id).await? else {
        return Ok(KothScoringSnapshot {
            fully_settled: event_ended,
            ..KothScoringSnapshot::default()
        });
    };
    let start_round = official.scoring_start_round;
    let epoch_ticks = official.epoch_ticks.clamp(1, 64);
    // Live boards stay read-only; the round driver refreshes immutable rollups
    // after evidence is sealed. Preserve a post-event repair path in case a
    // replica crashed between finalization and that derived write.
    if event_ended {
        rollup::ensure_epoch_rollups(pool, game_id, Utc::now()).await?;
    }

    let checker_cutoff = cutoff;
    let mut transaction = begin_read_only_repeatable_read(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let (rollup_header, rollup_teams, rollup_hills, recent_epochs) =
        load_rollup_snapshot(&mut transaction, game_id, cutoff, checker_cutoff).await?;
    let first_raw_epoch = rollup_header.as_ref().map_or(1, |header| header.epoch + 1);
    let raw_start = start_round.saturating_add((first_raw_epoch - 1).saturating_mul(epoch_ticks));
    let (meta, team_evidence) = evidence::load_evidence(
        &mut transaction,
        game_id,
        start_round,
        raw_start,
        epoch_ticks,
        cutoff,
        checker_cutoff,
        event_ended,
    )
    .await?;
    let effective_roster = sqlx::query_scalar::<_, i32>(
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
    let latest_round = sqlx::query_scalar::<_, Option<i32>>(
        r#"SELECT MAX(number) FROM "AdRounds"
            WHERE game_id = $1 AND number >= $2
              AND ($3::timestamptz IS NULL
                   OR (NOT $4 AND start_time_utc <= $3)
                   OR ($4 AND start_time_utc < $3))"#,
    )
    .bind(game_id)
    .bind(start_round)
    .bind(cutoff)
    .bind(event_ended)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let raw = score_evidence_rows(
        &meta,
        &team_evidence,
        &effective_roster,
        epoch_ticks,
        event_ended,
    )?;
    let current_epoch = latest_round.map_or(0, |round| ((round - start_round) / epoch_ticks) + 1);
    Ok(merge_rollup_prefix(
        &effective_roster,
        rollup_header.as_ref(),
        rollup_teams,
        rollup_hills,
        recent_epochs,
        raw,
        event_ended,
        current_epoch,
    ))
}

/// Materialize newly immutable KotH epochs away from player request latency.
/// Games without an official crown-cycle snapshot remain untouched.
pub(crate) async fn refresh_epoch_rollups(
    pool: &PgPool,
    game_id: i32,
    now: DateTime<Utc>,
) -> AppResult<()> {
    if evidence::load_official_config(pool, game_id)
        .await?
        .is_none()
    {
        return Ok(());
    }
    rollup::ensure_epoch_rollups(pool, game_id, now)
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests;
