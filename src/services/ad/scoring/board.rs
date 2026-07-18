use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use super::evidence::{
    load_epoch_evidence, load_epoch_meta, load_latest_check_statuses, load_stable_services,
    EvidenceAggregateRow, EvidenceRange,
};
use super::rollup::{load_rollup_snapshot, RecentTeamEpochRow, TeamRollupRow};
use super::service_rollup::ServiceRollupRow;
use super::{score_epoch_service, EpochServiceEvidence};
use crate::utils::database::begin_read_only_repeatable_read;
use crate::utils::enums::ChallengeCategory;
use crate::utils::error::{AppError, AppResult};

const TEAM_DETAIL_EPOCH_LIMIT: usize = 3;
const FLAG_LIFETIME_TICKS_DEFAULT: i32 = 5;
const TICK_SECONDS_DEFAULT: i64 = 60;

#[derive(Debug, sqlx::FromRow)]
struct AdScoreboardGameRow {
    hidden: bool,
    epoch_ticks: i32,
    scoring_start_round: Option<i32>,
    flag_lifetime_ticks: Option<i32>,
    tick_seconds: Option<i32>,
    freeze_time_utc: Option<DateTime<Utc>>,
    end_time_utc: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct AdScoreboardRoundRow {
    latest_round: i32,
    current_round_ends_at: DateTime<Utc>,
    tick_seconds: i64,
    finalized: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct AdScoreboardChallengeRow {
    challenge_id: i32,
    title: String,
    category: i16,
}

#[derive(Clone, Debug)]
struct ScoredService {
    challenge_id: i32,
    capture_count: u64,
    offense_rate: f64,
    defense_rate: f64,
    sla_rate: f64,
    service_weight: f64,
    local_points: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdEpochScore {
    pub epoch: i32,
    pub points: f64,
    pub epoch_weight: f64,
    pub finalized: bool,
}

#[derive(Clone, Debug)]
struct ScoredEpoch {
    summary: AdEpochScore,
    services: Vec<ScoredService>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTeamScore {
    pub rank: i32,
    pub participation_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub division: Option<String>,
    pub settled_total: f64,
    pub projected_total: f64,
    pub offense_rate: f64,
    pub defense_rate: f64,
    pub sla_rate: f64,
    pub services: Vec<AdServiceScore>,
    pub epochs: Vec<AdEpochScore>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdServiceScore {
    pub challenge_id: i32,
    pub settled_points: f64,
    pub projected_points: f64,
    pub offense_rate: f64,
    pub defense_rate: f64,
    pub sla_rate: f64,
    pub capture_count: u64,
    pub last_check_status: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdEvidenceStatus {
    pub eligible_flags: u64,
    pub captured_flags: u64,
    pub accepted_captures: u64,
    pub defense_opportunities: u64,
    pub protected_opportunities: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdScoreboardChallenge {
    pub challenge_id: i32,
    pub title: String,
    pub category: ChallengeCategory,
}

/// Apply the public A&D ranking policy. Settled points remain the primary key;
/// the live projection and its component rates resolve exact score ties, while
/// participation id provides an immutable final key.
fn sort_and_rank_team_rows(rows: &mut [AdTeamScore]) {
    rows.sort_by(|left, right| {
        right
            .settled_total
            .partial_cmp(&left.settled_total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                right
                    .projected_total
                    .partial_cmp(&left.projected_total)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                right
                    .offense_rate
                    .partial_cmp(&left.offense_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                right
                    .defense_rate
                    .partial_cmp(&left.defense_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                right
                    .sla_rate
                    .partial_cmp(&left.sla_rate)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.participation_id.cmp(&right.participation_id))
    });
    for (index, row) in rows.iter_mut().enumerate() {
        row.rank = index as i32 + 1;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdScoreboard {
    pub epoch_ticks: i32,
    pub start_round: Option<i32>,
    pub started: bool,
    /// True only when the event has ended and every official epoch is durably
    /// materialized. Awards and podiums must wait for this signal.
    pub fully_settled: bool,
    pub current_epoch: i32,
    pub latest_round: i32,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub current_round_ends_at: Option<DateTime<Utc>>,
    pub tick_seconds: i64,
    pub is_frozen_view: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub freeze: Option<DateTime<Utc>>,
    pub challenges: Vec<AdScoreboardChallenge>,
    pub detail_epoch_limit: usize,
    pub evidence: AdEvidenceStatus,
    pub teams: Vec<AdTeamScore>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub generated_at: DateTime<Utc>,
}

fn count(value: i64) -> u64 {
    value.max(0) as u64
}

fn scoreboard_evidence_cutoff(
    freeze: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
    is_monitor: bool,
) -> Option<DateTime<Utc>> {
    let freeze_cutoff = match freeze {
        Some(freeze) if !is_monitor && now >= freeze && now < end => Some(freeze),
        _ => None,
    };
    if now >= end {
        Some(freeze_cutoff.map_or(end, |value| value.min(end)))
    } else {
        freeze_cutoff
    }
}

fn evidence_from_row(row: &EvidenceAggregateRow) -> EpochServiceEvidence {
    EpochServiceEvidence {
        opportunity_count: count(row.opportunity_count),
        capture_count: count(row.capture_count),
        rarity_sum: row.rarity_sum,
        defense_opportunity_count: count(row.defense_opportunity_count),
        protected_opportunity_count: count(row.protected_opportunity_count),
        sla_credit_sum: row.sla_credit_sum,
        sla_tick_count: count(row.sla_tick_count),
        service_weight: row.service_weight,
    }
}

fn evidence_status(rows: &[EvidenceAggregateRow]) -> AdEvidenceStatus {
    rows.first()
        .map_or_else(AdEvidenceStatus::default, |row| AdEvidenceStatus {
            eligible_flags: count(row.eligible_flags_total),
            captured_flags: count(row.captured_flags_total),
            accepted_captures: count(row.accepted_captures_total),
            defense_opportunities: count(row.defense_opportunities_total),
            protected_opportunities: count(row.protected_opportunities_total),
        })
}

#[cfg(test)]
fn projected_rate(epochs: &[ScoredEpoch], select: fn(&ScoredService) -> f64) -> f64 {
    let (numerator, weight) = rate_components(epochs, select);
    if weight == 0.0 {
        0.0
    } else {
        numerator / weight
    }
}

fn rate_components(epochs: &[ScoredEpoch], select: fn(&ScoredService) -> f64) -> (f64, f64) {
    let weight = epochs
        .iter()
        .flat_map(|epoch| {
            epoch
                .services
                .iter()
                .map(move |service| service.service_weight * epoch.summary.epoch_weight)
        })
        .sum::<f64>();
    let numerator = epochs
        .iter()
        .flat_map(|epoch| {
            epoch.services.iter().map(move |service| {
                select(service) * service.service_weight * epoch.summary.epoch_weight
            })
        })
        .sum::<f64>();
    (numerator, weight)
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator > 0.0 {
        numerator / denominator
    } else {
        0.0
    }
}

fn check_status_label(status: i16) -> &'static str {
    match status {
        0 => "Ok",
        1 => "Mumble",
        2 => "Offline",
        _ => "InternalError",
    }
}

fn merge_service_detail(
    challenge_id: i32,
    previous: Option<&ServiceRollupRow>,
    raw_epochs: &[ScoredEpoch],
    last_check_status: Option<String>,
) -> AdServiceScore {
    let previous_weight = previous.map_or(0.0, |row| row.cumulative_epoch_weight);
    let mut projected_points_numerator =
        previous.map_or(0.0, |row| row.cumulative_points_numerator);
    let mut settled_points_numerator = projected_points_numerator;
    let mut projected_weight = previous_weight;
    let mut settled_weight = previous_weight;
    let mut offense_numerator = previous.map_or(0.0, |row| row.cumulative_offense_numerator);
    let mut defense_numerator = previous.map_or(0.0, |row| row.cumulative_defense_numerator);
    let mut sla_numerator = previous.map_or(0.0, |row| row.cumulative_sla_numerator);
    let mut captures = previous.map_or(0, |row| count(row.cumulative_capture_count));

    for epoch in raw_epochs {
        let Some(service) = epoch
            .services
            .iter()
            .find(|service| service.challenge_id == challenge_id)
        else {
            continue;
        };
        let service_weight = epoch
            .services
            .iter()
            .map(|service| service.service_weight)
            .sum::<f64>();
        if service_weight <= 0.0 {
            continue;
        }
        let weight = epoch.summary.epoch_weight;
        let point_contribution =
            service.local_points * service.service_weight / service_weight * weight;
        projected_points_numerator += point_contribution;
        projected_weight += weight;
        if epoch.summary.finalized {
            settled_points_numerator += point_contribution;
            settled_weight += weight;
        }
        offense_numerator += service.offense_rate * weight;
        defense_numerator += service.defense_rate * weight;
        sla_numerator += service.sla_rate * weight;
        captures = captures.saturating_add(service.capture_count);
    }

    AdServiceScore {
        challenge_id,
        settled_points: ratio(settled_points_numerator, settled_weight),
        projected_points: ratio(projected_points_numerator, projected_weight),
        offense_rate: ratio(offense_numerator, projected_weight),
        defense_rate: ratio(defense_numerator, projected_weight),
        sla_rate: ratio(sla_numerator, projected_weight),
        capture_count: captures,
        last_check_status,
    }
}

/// Build the official epoch-settled board from SQL-aggregated evidence.
///
/// The ranked roster and service set are frozen by flags minted in
/// `scoring_start_round`. Later teams, services, and captures are excluded from
/// every epoch, preventing a one-tick entrant from receiving a full epoch score.
/// PostgreSQL reduces growing raw event tables into one bounded row per frozen
/// team/service/epoch before anything reaches Rust.
pub async fn build_ad_scoreboard(
    pool: &PgPool,
    game_id: i32,
    is_monitor: bool,
    now: DateTime<Utc>,
) -> AppResult<AdScoreboard> {
    // Reject absent/hidden games before entering the potentially writing rollup
    // transaction. Hidden responses are never cached, so without this preflight
    // anonymous misses could repeatedly contend on the per-game rollup lock.
    let visible_end = sqlx::query_scalar::<_, DateTime<Utc>>(
        r#"SELECT end_time_utc FROM "Games" WHERE id = $1 AND hidden = FALSE"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let visible_end = visible_end.ok_or_else(|| AppError::not_found("Game not found"))?;

    // Live score reads are strictly read-only. The round driver materializes
    // immutable rollups off the request path; an ended game keeps this repair
    // fallback so a crash before the final refresh cannot leave ranking stale.
    if now >= visible_end {
        super::rollup::ensure_epoch_rollups(pool, game_id, now).await?;
    }

    // Round creation commits several related tables atomically. Read them from
    // one repeatable-read snapshot so a boundary cannot mix game settings,
    // display metadata, or epoch N+1 with evidence that only saw epoch N.
    let mut transaction = begin_read_only_repeatable_read(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let game = sqlx::query_as::<_, AdScoreboardGameRow>(
        r#"SELECT hidden, ad_epoch_ticks AS epoch_ticks,
                  ad_scoring_start_round AS scoring_start_round,
                  ad_flag_lifetime_ticks AS flag_lifetime_ticks,
                  ad_tick_seconds AS tick_seconds,
                  freeze_time_utc, end_time_utc
             FROM "Games"
            WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    if game.hidden {
        return Err(AppError::not_found("Game not found"));
    }

    // Event end is an evidence boundary for every viewer, including monitors.
    let cutoff =
        scoreboard_evidence_cutoff(game.freeze_time_utc, game.end_time_utc, now, is_monitor);
    let event_end_settlement = now >= game.end_time_utc;

    let round_clock = sqlx::query_as::<_, AdScoreboardRoundRow>(
        r#"SELECT number AS latest_round, end_time_utc AS current_round_ends_at,
                  GREATEST(
                    EXTRACT(EPOCH FROM (end_time_utc - start_time_utc))::bigint,
                    0
                  ) AS tick_seconds,
                  finalized
            FROM "AdRounds"
            WHERE game_id = $1
              AND ($2::timestamptz IS NULL OR CASE WHEN $3::boolean
                     THEN start_time_utc < $2
                     ELSE start_time_utc <= $2 END)
            ORDER BY number DESC
            LIMIT 1"#,
    )
    .bind(game_id)
    .bind(cutoff)
    .bind(event_end_settlement)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let challenge_rows = sqlx::query_as::<_, AdScoreboardChallengeRow>(
        r#"SELECT id AS challenge_id, title, category
             FROM "GameChallenges"
            WHERE game_id = $1
              AND is_enabled = TRUE
              AND review_status = $2
              AND "Type" = $3
            ORDER BY category, id"#,
    )
    .bind(game_id)
    .bind(crate::utils::enums::ChallengeReviewStatus::Active as i16)
    .bind(crate::utils::enums::ChallengeType::AttackDefense as i16)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let epoch_ticks = game.epoch_ticks.clamp(1, 64);
    let scoring_start_round = game.scoring_start_round.map(|round| round.max(1));
    let flag_lifetime_ticks = game
        .flag_lifetime_ticks
        .unwrap_or(FLAG_LIFETIME_TICKS_DEFAULT)
        .clamp(1, 50);
    let cutoff_round = cutoff.and_then(|_| round_clock.as_ref().map(|round| round.latest_round));
    let checker_cutoff = cutoff;
    let (
        services,
        rollup_header,
        rollup_teams,
        rollup_services,
        recent_rollups,
        epoch_meta,
        aggregate_rows,
        latest_checks,
    ) = match scoring_start_round {
        Some(start_round) => {
            let services = load_stable_services(
                &mut transaction,
                game_id,
                start_round,
                cutoff,
                event_end_settlement,
            )
            .await?;
            let (rollup_header, rollup_teams, rollup_services, recent_rollups) =
                load_rollup_snapshot(&mut transaction, game_id, cutoff_round).await?;
            let first_raw_epoch = rollup_header.as_ref().map_or(1, |header| header.epoch + 1);
            let raw_start =
                start_round.saturating_add((first_raw_epoch - 1).saturating_mul(epoch_ticks));
            let raw_round_count = round_clock
                .as_ref()
                .map_or(0, |round| round.latest_round.saturating_sub(raw_start) + 1);
            let raw_round_limit = epoch_ticks.saturating_add(flag_lifetime_ticks);
            if raw_round_count > raw_round_limit {
                return Err(AppError::internal(format!(
                    "A&D scoring rollup is blocked by incomplete checker evidence: \
                         {raw_round_count} unresolved rounds exceeds the \
                         {raw_round_limit}-round limit"
                )));
            }
            let range = EvidenceRange {
                official_start_round: start_round,
                start_round: raw_start,
                end_round: None,
                epoch_ticks,
                round_cutoff: cutoff,
                checker_cutoff,
                attack_cutoff: cutoff,
                event_end_settlement,
            };
            let epoch_meta = load_epoch_meta(&mut transaction, game_id, range).await?;
            let aggregate_rows = load_epoch_evidence(&mut transaction, game_id, range).await?;
            let latest_checks = load_latest_check_statuses(
                &mut transaction,
                game_id,
                start_round,
                &services,
                cutoff,
                checker_cutoff,
                event_end_settlement,
            )
            .await?;
            (
                services,
                rollup_header,
                rollup_teams,
                rollup_services,
                recent_rollups,
                epoch_meta,
                aggregate_rows,
                latest_checks,
            )
        }
        None => (
            Vec::new(),
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ),
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let challenges = challenge_rows
        .into_iter()
        .map(|row| {
            let category =
                <ChallengeCategory as sea_orm::ActiveEnum>::try_from_value(&row.category)
                    .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(AdScoreboardChallenge {
                challenge_id: row.challenge_id,
                title: row.title,
                category,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    let acceptance_closed = now >= game.end_time_utc;
    let current_round = round_clock
        .as_ref()
        .map(|round| round.latest_round)
        .unwrap_or(0);
    let current_epoch = scoring_start_round
        .filter(|start| current_round >= *start)
        .map_or(0, |start| ((current_round - start) / epoch_ticks) + 1);
    let final_round_sealed =
        acceptance_closed && round_clock.as_ref().is_some_and(|round| round.finalized);
    let fully_settled = final_round_sealed
        && scoring_start_round.is_none_or(|_| {
            current_epoch > 0
                && rollup_header
                    .as_ref()
                    .is_some_and(|header| header.epoch >= current_epoch)
        });
    let teams: BTreeMap<i32, (i32, String, Option<String>)> = services
        .iter()
        .map(|service| {
            (
                service.participation_id,
                (
                    service.team_id,
                    service.team_name.clone(),
                    service.division.clone(),
                ),
            )
        })
        .collect();
    let mut services_by_team = HashMap::<i32, Vec<_>>::new();
    for service in &services {
        services_by_team
            .entry(service.participation_id)
            .or_default()
            .push(service);
    }
    let evidence: HashMap<(i32, i32, i32), EpochServiceEvidence> = aggregate_rows
        .iter()
        .map(|row| {
            (
                (row.participation_id, row.challenge_id, row.epoch),
                evidence_from_row(row),
            )
        })
        .collect();
    let raw_status = evidence_status(&aggregate_rows);
    let status = rollup_header
        .as_ref()
        .map_or(raw_status.clone(), |header| AdEvidenceStatus {
            eligible_flags: count(header.cumulative_eligible_flags)
                .saturating_add(raw_status.eligible_flags),
            captured_flags: count(header.cumulative_captured_flags)
                .saturating_add(raw_status.captured_flags),
            accepted_captures: count(header.cumulative_accepted_captures)
                .saturating_add(raw_status.accepted_captures),
            defense_opportunities: count(header.cumulative_defense_opportunities)
                .saturating_add(raw_status.defense_opportunities),
            protected_opportunities: count(header.cumulative_protected_opportunities)
                .saturating_add(raw_status.protected_opportunities),
        });
    let rollup_teams: HashMap<i32, TeamRollupRow> = rollup_teams
        .into_iter()
        .map(|row| (row.participation_id, row))
        .collect();
    let rollup_services: HashMap<(i32, i32), ServiceRollupRow> = rollup_services
        .into_iter()
        .map(|row| ((row.participation_id, row.challenge_id), row))
        .collect();
    let latest_checks: HashMap<(i32, i32), String> = latest_checks
        .into_iter()
        .map(|row| {
            (
                (row.participation_id, row.challenge_id),
                check_status_label(row.status).to_string(),
            )
        })
        .collect();
    let mut recent_by_team: HashMap<i32, Vec<RecentTeamEpochRow>> = HashMap::new();
    for row in recent_rollups {
        recent_by_team
            .entry(row.participation_id)
            .or_default()
            .push(row);
    }

    let mut team_rows = Vec::with_capacity(teams.len());
    for (participation_id, (team_id, team_name, division)) in teams {
        let team_services = services_by_team
            .get(&participation_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut raw_epochs = Vec::with_capacity(epoch_meta.len());
        for meta in &epoch_meta {
            let mut services_out = Vec::new();
            for service in team_services {
                let Some(service_evidence) =
                    evidence.get(&(participation_id, service.challenge_id, meta.epoch))
                else {
                    continue;
                };
                let score = score_epoch_service(service_evidence)
                    .map_err(|error| AppError::internal(error.to_string()))?;
                services_out.push(ScoredService {
                    challenge_id: service.challenge_id,
                    capture_count: service_evidence.capture_count,
                    offense_rate: score.offense_rate,
                    defense_rate: score.defense_rate,
                    sla_rate: score.sla_rate,
                    service_weight: score.service_weight,
                    local_points: score.local_points,
                });
            }
            let points = if services_out.is_empty() {
                0.0
            } else {
                let total_weight = services_out
                    .iter()
                    .map(|service| service.service_weight)
                    .sum::<f64>();
                services_out
                    .iter()
                    .map(|service| service.local_points * service.service_weight)
                    .sum::<f64>()
                    / total_weight
            };
            let epoch_weight =
                meta.round_count.min(i64::from(epoch_ticks)) as f64 / f64::from(epoch_ticks);
            let finalized = meta.all_checks_complete
                && meta.all_finalized
                && (acceptance_closed
                    || (meta.round_count == i64::from(epoch_ticks)
                        && current_round >= meta.end_round.saturating_add(flag_lifetime_ticks)));
            raw_epochs.push(ScoredEpoch {
                summary: AdEpochScore {
                    epoch: meta.epoch,
                    points,
                    epoch_weight,
                    finalized,
                },
                services: services_out,
            });
        }

        let previous = rollup_teams.get(&participation_id);
        let previous_points = previous.map_or(0.0, |row| row.cumulative_points_numerator);
        let previous_epoch_weight = previous.map_or(0.0, |row| row.cumulative_epoch_weight);
        let raw_points = raw_epochs
            .iter()
            .map(|epoch| epoch.summary.points * epoch.summary.epoch_weight)
            .sum::<f64>();
        let raw_epoch_weight = raw_epochs
            .iter()
            .map(|epoch| epoch.summary.epoch_weight)
            .sum::<f64>();
        let settled_raw_points = raw_epochs
            .iter()
            .filter(|epoch| epoch.summary.finalized)
            .map(|epoch| epoch.summary.points * epoch.summary.epoch_weight)
            .sum::<f64>();
        let settled_raw_weight = raw_epochs
            .iter()
            .filter(|epoch| epoch.summary.finalized)
            .map(|epoch| epoch.summary.epoch_weight)
            .sum::<f64>();
        let projected_total = ratio(
            previous_points + raw_points,
            previous_epoch_weight + raw_epoch_weight,
        );
        let settled_total = ratio(
            previous_points + settled_raw_points,
            previous_epoch_weight + settled_raw_weight,
        );
        let (raw_offense, raw_rate_weight) =
            rate_components(&raw_epochs, |service| service.offense_rate);
        let (raw_defense, _) = rate_components(&raw_epochs, |service| service.defense_rate);
        let (raw_sla, _) = rate_components(&raw_epochs, |service| service.sla_rate);
        let previous_rate_weight = previous.map_or(0.0, |row| row.cumulative_rate_weight);
        let offense_rate = ratio(
            previous.map_or(0.0, |row| row.cumulative_offense_numerator) + raw_offense,
            previous_rate_weight + raw_rate_weight,
        );
        let defense_rate = ratio(
            previous.map_or(0.0, |row| row.cumulative_defense_numerator) + raw_defense,
            previous_rate_weight + raw_rate_weight,
        );
        let sla_rate = ratio(
            previous.map_or(0.0, |row| row.cumulative_sla_numerator) + raw_sla,
            previous_rate_weight + raw_rate_weight,
        );

        let service_scores = team_services
            .iter()
            .map(|service| {
                merge_service_detail(
                    service.challenge_id,
                    rollup_services.get(&(participation_id, service.challenge_id)),
                    &raw_epochs,
                    latest_checks
                        .get(&(participation_id, service.challenge_id))
                        .cloned(),
                )
            })
            .collect();

        let mut epochs: Vec<AdEpochScore> = recent_by_team
            .remove(&participation_id)
            .unwrap_or_default()
            .into_iter()
            .map(|row| AdEpochScore {
                epoch: row.epoch,
                points: row.points,
                epoch_weight: row.epoch_weight,
                finalized: true,
            })
            .chain(raw_epochs.into_iter().map(|epoch| epoch.summary))
            .collect();
        epochs.sort_by_key(|epoch| epoch.epoch);
        if epochs.len() > TEAM_DETAIL_EPOCH_LIMIT {
            epochs.drain(..epochs.len() - TEAM_DETAIL_EPOCH_LIMIT);
        }
        team_rows.push(AdTeamScore {
            rank: 0,
            participation_id,
            team_id,
            team_name,
            division,
            settled_total,
            projected_total,
            offense_rate,
            defense_rate,
            sla_rate,
            services: service_scores,
            epochs,
        });
    }

    sort_and_rank_team_rows(&mut team_rows);

    Ok(AdScoreboard {
        epoch_ticks,
        start_round: scoring_start_round,
        started: scoring_start_round.is_some(),
        fully_settled,
        current_epoch,
        latest_round: round_clock
            .as_ref()
            .map(|round| round.latest_round)
            .unwrap_or(0),
        current_round_ends_at: round_clock
            .as_ref()
            .map(|round| round.current_round_ends_at),
        tick_seconds: round_clock
            .as_ref()
            .map(|round| round.tick_seconds)
            .unwrap_or_else(|| {
                game.tick_seconds
                    .map(i64::from)
                    .unwrap_or(TICK_SECONDS_DEFAULT)
            }),
        is_frozen_view: cutoff.is_some(),
        freeze: cutoff,
        challenges,
        detail_epoch_limit: TEAM_DETAIL_EPOCH_LIMIT,
        evidence: status,
        teams: team_rows,
        generated_at: now,
    })
}

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;
