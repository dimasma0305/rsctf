use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection, PgPool};

use super::evidence::{
    load_epoch_evidence, load_epoch_meta, EpochMetaRow, EvidenceAggregateRow, EvidenceRange,
};
use super::service_rollup::{insert_service_rows, load_service_rollups, ServiceRollupRow};
use super::{score_epoch_service, EpochServiceEvidence};
use crate::utils::error::{AppError, AppResult};

const FLAG_LIFETIME_TICKS_DEFAULT: i32 = 5;
const ROLLUP_LOCK_NAMESPACE: i32 = 0x4144_4550;

#[derive(Clone, Debug, FromRow)]
pub(super) struct RollupHeaderRow {
    pub epoch: i32,
    pub cumulative_eligible_flags: i64,
    pub cumulative_captured_flags: i64,
    pub cumulative_accepted_captures: i64,
    pub cumulative_defense_opportunities: i64,
    pub cumulative_protected_opportunities: i64,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct TeamRollupRow {
    pub participation_id: i32,
    pub cumulative_points_numerator: f64,
    pub cumulative_epoch_weight: f64,
    pub cumulative_offense_numerator: f64,
    pub cumulative_defense_numerator: f64,
    pub cumulative_sla_numerator: f64,
    pub cumulative_rate_weight: f64,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct RecentTeamEpochRow {
    pub participation_id: i32,
    pub epoch: i32,
    pub points: f64,
    pub epoch_weight: f64,
}

#[derive(Debug, FromRow)]
struct RollupGameRow {
    epoch_ticks: i32,
    scoring_start_round: Option<i32>,
    flag_lifetime_ticks: Option<i32>,
    end_time_utc: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Default)]
struct PreviousTeamTotals {
    points_numerator: f64,
    epoch_weight: f64,
    offense_numerator: f64,
    defense_numerator: f64,
    sla_numerator: f64,
    rate_weight: f64,
}

#[derive(Debug)]
struct ComputedTeamRollup {
    participation_id: i32,
    points: f64,
    epoch_weight: f64,
    totals: PreviousTeamTotals,
}

fn count(value: i64) -> i64 {
    value.max(0)
}

fn service_evidence(row: &EvidenceAggregateRow) -> EpochServiceEvidence {
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

/// Drop the cumulative suffix affected by an administrator changing raw check
/// evidence. Child service/team rows cascade from the epoch header.
pub(crate) async fn invalidate_rollups_from_round(
    connection: &mut PgConnection,
    game_id: i32,
    round_number: i32,
) -> AppResult<()> {
    lock_epoch_rollups(connection, game_id).await?;
    sqlx::query(
        r#"DELETE FROM "AdEpochRollups" rollup
              USING "Games" game
             WHERE rollup.game_id = $1
               AND game.id = rollup.game_id
               AND game.ad_scoring_start_round IS NOT NULL
               AND $2 >= game.ad_scoring_start_round
               AND rollup.epoch >=
                   (($2 - game.ad_scoring_start_round) /
                    GREATEST(game.ad_epoch_ticks, 1)) + 1"#,
    )
    .bind(game_id)
    .bind(round_number)
    .execute(connection)
    .await
    .map(|_| ())
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Reopen durable evidence affected by an event deadline edit. Extending
/// reopens any tail whose flag-lifetime horizon was cut short, including a full
/// epoch sealed exactly at event end. Shortening conservatively rebuilds all.
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
    if next_end < previous_end {
        sqlx::query(r#"DELETE FROM "AdEpochRollups" WHERE game_id = $1"#)
            .bind(game_id)
            .execute(connection)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    } else {
        sqlx::query(
            r#"DELETE FROM "AdEpochRollups" rollup
                USING "Games" game
                WHERE rollup.game_id = $1
                  AND game.id = rollup.game_id
                  AND rollup.epoch >= COALESCE(
                        (SELECT MIN(candidate.epoch)
                           FROM "AdEpochRollups" candidate
                          WHERE candidate.game_id = $1
                            AND (
                              candidate.epoch_weight < 1.0
                              OR candidate.finalized_round < candidate.end_round
                                 + GREATEST(COALESCE(game.ad_flag_lifetime_ticks, 5), 1)
                            )),
                        2147483647
                  )"#,
        )
        .bind(game_id)
        .execute(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

/// Persist every newly immutable epoch. The advisory transaction makes the
/// header row an exactly-once marker across replicas and concurrent cache misses.
pub(crate) async fn ensure_epoch_rollups(
    pool: &PgPool,
    game_id: i32,
    now: DateTime<Utc>,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    lock_epoch_rollups(&mut transaction, game_id).await?;

    let game = sqlx::query_as::<_, RollupGameRow>(
        r#"SELECT ad_epoch_ticks AS epoch_ticks,
                  ad_scoring_start_round AS scoring_start_round,
                  ad_flag_lifetime_ticks AS flag_lifetime_ticks, end_time_utc
             FROM "Games" WHERE id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(game) = game else {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(());
    };
    let Some(official_start_round) = game.scoring_start_round else {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(());
    };
    let epoch_ticks = game.epoch_ticks.clamp(1, 64);
    let lifetime_ticks = game
        .flag_lifetime_ticks
        .unwrap_or(FLAG_LIFETIME_TICKS_DEFAULT)
        .clamp(1, 50);
    let previous_epoch = sqlx::query_scalar::<_, Option<i32>>(
        r#"SELECT MAX(epoch) FROM "AdEpochRollups" WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(0);
    let range_start =
        official_start_round.saturating_add(previous_epoch.saturating_mul(epoch_ticks));
    let ended = now >= game.end_time_utc;
    let round_cutoff = ended.then_some(game.end_time_utc);
    let meta = load_epoch_meta(
        &mut transaction,
        game_id,
        EvidenceRange {
            official_start_round,
            start_round: range_start,
            end_round: None,
            epoch_ticks,
            round_cutoff,
            checker_cutoff: ended.then_some(game.end_time_utc),
            attack_cutoff: None,
            event_end_settlement: ended,
        },
    )
    .await?;

    for epoch in meta {
        let full = epoch.round_count == i64::from(epoch_ticks);
        let immutable = epoch.all_checks_complete
            && epoch.all_finalized
            && (ended
                || (full && epoch.current_round >= epoch.end_round.saturating_add(lifetime_ticks)));
        if !immutable {
            break;
        }
        let finalized_round = if ended {
            epoch.current_round.max(epoch.end_round)
        } else {
            epoch.end_round.saturating_add(lifetime_ticks)
        };
        materialize_epoch(
            &mut transaction,
            game_id,
            official_start_round,
            epoch_ticks,
            finalized_round,
            &epoch,
            ended.then_some(game.end_time_utc),
        )
        .await?;
    }

    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn materialize_epoch(
    connection: &mut PgConnection,
    game_id: i32,
    official_start_round: i32,
    epoch_ticks: i32,
    finalized_round: i32,
    meta: &EpochMetaRow,
    evidence_cutoff: Option<DateTime<Utc>>,
) -> AppResult<()> {
    let exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
              SELECT 1 FROM "AdEpochRollups" WHERE game_id = $1 AND epoch = $2
           )"#,
    )
    .bind(game_id)
    .bind(meta.epoch)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if exists {
        return Ok(());
    }

    let rows = load_epoch_evidence(
        connection,
        game_id,
        EvidenceRange {
            official_start_round,
            start_round: meta.start_round,
            end_round: Some(meta.end_round),
            epoch_ticks,
            round_cutoff: evidence_cutoff,
            checker_cutoff: evidence_cutoff,
            attack_cutoff: evidence_cutoff,
            event_end_settlement: evidence_cutoff.is_some(),
        },
    )
    .await?;
    if rows.is_empty() {
        return Ok(());
    }
    let epoch_weight = meta.round_count as f64 / f64::from(epoch_ticks);
    let status = &rows[0];
    let previous_header = sqlx::query_as::<_, RollupHeaderRow>(
        r#"SELECT epoch, cumulative_eligible_flags, cumulative_captured_flags,
                  cumulative_accepted_captures, cumulative_defense_opportunities,
                  cumulative_protected_opportunities
             FROM "AdEpochRollups"
            WHERE game_id = $1 AND epoch < $2
            ORDER BY epoch DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(meta.epoch)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let previous_team_rows = sqlx::query_as::<_, TeamRollupRow>(
        r#"SELECT participation_id, cumulative_points_numerator, cumulative_epoch_weight,
                  cumulative_offense_numerator, cumulative_defense_numerator,
                  cumulative_sla_numerator, cumulative_rate_weight
             FROM "AdEpochTeamRollups"
            WHERE game_id = $1 AND epoch = $2"#,
    )
    .bind(game_id)
    .bind(meta.epoch - 1)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let previous_teams: HashMap<i32, PreviousTeamTotals> = previous_team_rows
        .into_iter()
        .map(|row| {
            (
                row.participation_id,
                PreviousTeamTotals {
                    points_numerator: row.cumulative_points_numerator,
                    epoch_weight: row.cumulative_epoch_weight,
                    offense_numerator: row.cumulative_offense_numerator,
                    defense_numerator: row.cumulative_defense_numerator,
                    sla_numerator: row.cumulative_sla_numerator,
                    rate_weight: row.cumulative_rate_weight,
                },
            )
        })
        .collect();
    let computed = compute_team_rollups(&rows, epoch_weight, &previous_teams)?;

    let previous = previous_header.as_ref();
    sqlx::query(
        r#"INSERT INTO "AdEpochRollups"
             (game_id, epoch, start_round, end_round, round_count, epoch_weight,
              finalized_round, eligible_flags, captured_flags, accepted_captures,
              defense_opportunities, protected_opportunities,
              cumulative_eligible_flags, cumulative_captured_flags,
              cumulative_accepted_captures, cumulative_defense_opportunities,
              cumulative_protected_opportunities)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
           ON CONFLICT (game_id, epoch) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(meta.epoch)
    .bind(meta.start_round)
    .bind(meta.end_round)
    .bind(meta.round_count as i32)
    .bind(epoch_weight)
    .bind(finalized_round)
    .bind(count(status.eligible_flags_total))
    .bind(count(status.captured_flags_total))
    .bind(count(status.accepted_captures_total))
    .bind(count(status.defense_opportunities_total))
    .bind(count(status.protected_opportunities_total))
    .bind(
        previous.map_or(0, |row| row.cumulative_eligible_flags)
            + count(status.eligible_flags_total),
    )
    .bind(
        previous.map_or(0, |row| row.cumulative_captured_flags)
            + count(status.captured_flags_total),
    )
    .bind(
        previous.map_or(0, |row| row.cumulative_accepted_captures)
            + count(status.accepted_captures_total),
    )
    .bind(
        previous.map_or(0, |row| row.cumulative_defense_opportunities)
            + count(status.defense_opportunities_total),
    )
    .bind(
        previous.map_or(0, |row| row.cumulative_protected_opportunities)
            + count(status.protected_opportunities_total),
    )
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let previous_services = match previous_header {
        Some(ref header) => load_service_rollups(connection, game_id, header.epoch).await?,
        None => Vec::new(),
    };
    insert_service_rows(
        connection,
        game_id,
        meta.epoch,
        epoch_weight,
        &rows,
        &previous_services,
    )
    .await?;
    insert_team_rows(connection, game_id, meta.epoch, &computed).await
}

fn compute_team_rollups(
    rows: &[EvidenceAggregateRow],
    epoch_weight: f64,
    previous: &HashMap<i32, PreviousTeamTotals>,
) -> AppResult<Vec<ComputedTeamRollup>> {
    let mut grouped: HashMap<i32, Vec<_>> = HashMap::new();
    for row in rows {
        let score = score_epoch_service(&service_evidence(row))
            .map_err(|error| AppError::internal(error.to_string()))?;
        grouped.entry(row.participation_id).or_default().push(score);
    }
    let mut output = Vec::with_capacity(grouped.len());
    for (participation_id, services) in grouped {
        let service_weight = services
            .iter()
            .map(|score| score.service_weight)
            .sum::<f64>();
        if service_weight <= 0.0 {
            continue;
        }
        let points = services
            .iter()
            .map(|score| score.local_points * score.service_weight)
            .sum::<f64>()
            / service_weight;
        let rate_weight = service_weight * epoch_weight;
        let prior = previous.get(&participation_id).copied().unwrap_or_default();
        output.push(ComputedTeamRollup {
            participation_id,
            points,
            epoch_weight,
            totals: PreviousTeamTotals {
                points_numerator: prior.points_numerator + points * epoch_weight,
                epoch_weight: prior.epoch_weight + epoch_weight,
                offense_numerator: prior.offense_numerator
                    + services
                        .iter()
                        .map(|score| score.offense_rate * score.service_weight * epoch_weight)
                        .sum::<f64>(),
                defense_numerator: prior.defense_numerator
                    + services
                        .iter()
                        .map(|score| score.defense_rate * score.service_weight * epoch_weight)
                        .sum::<f64>(),
                sla_numerator: prior.sla_numerator
                    + services
                        .iter()
                        .map(|score| score.sla_rate * score.service_weight * epoch_weight)
                        .sum::<f64>(),
                rate_weight: prior.rate_weight + rate_weight,
            },
        });
    }
    output.sort_by_key(|row| row.participation_id);
    Ok(output)
}

async fn insert_team_rows(
    connection: &mut PgConnection,
    game_id: i32,
    epoch: i32,
    rows: &[ComputedTeamRollup],
) -> AppResult<()> {
    let participation_ids: Vec<_> = rows.iter().map(|row| row.participation_id).collect();
    let points: Vec<_> = rows.iter().map(|row| row.points).collect();
    let epoch_weights: Vec<_> = rows.iter().map(|row| row.epoch_weight).collect();
    let points_numerator: Vec<_> = rows.iter().map(|row| row.totals.points_numerator).collect();
    let total_epoch_weight: Vec<_> = rows.iter().map(|row| row.totals.epoch_weight).collect();
    let offense: Vec<_> = rows
        .iter()
        .map(|row| row.totals.offense_numerator)
        .collect();
    let defense: Vec<_> = rows
        .iter()
        .map(|row| row.totals.defense_numerator)
        .collect();
    let sla: Vec<_> = rows.iter().map(|row| row.totals.sla_numerator).collect();
    let rate_weight: Vec<_> = rows.iter().map(|row| row.totals.rate_weight).collect();
    sqlx::query(
        r#"INSERT INTO "AdEpochTeamRollups"
             (game_id, epoch, participation_id, points, epoch_weight,
              cumulative_points_numerator, cumulative_epoch_weight,
              cumulative_offense_numerator, cumulative_defense_numerator,
              cumulative_sla_numerator, cumulative_rate_weight)
           SELECT $1, $2, row.*
             FROM UNNEST($3::integer[], $4::float8[], $5::float8[], $6::float8[],
                         $7::float8[], $8::float8[], $9::float8[], $10::float8[],
                         $11::float8[])
                  AS row(participation_id, points, epoch_weight,
                         cumulative_points_numerator, cumulative_epoch_weight,
                         cumulative_offense_numerator, cumulative_defense_numerator,
                         cumulative_sla_numerator, cumulative_rate_weight)
           ON CONFLICT (game_id, epoch, participation_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(epoch)
    .bind(&participation_ids)
    .bind(&points)
    .bind(&epoch_weights)
    .bind(&points_numerator)
    .bind(&total_epoch_weight)
    .bind(&offense)
    .bind(&defense)
    .bind(&sla)
    .bind(&rate_weight)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn load_rollup_snapshot(
    connection: &mut PgConnection,
    game_id: i32,
    cutoff_round: Option<i32>,
) -> AppResult<(
    Option<RollupHeaderRow>,
    Vec<TeamRollupRow>,
    Vec<ServiceRollupRow>,
    Vec<RecentTeamEpochRow>,
)> {
    let header = sqlx::query_as::<_, RollupHeaderRow>(
        r#"SELECT epoch, cumulative_eligible_flags, cumulative_captured_flags,
                  cumulative_accepted_captures, cumulative_defense_opportunities,
                  cumulative_protected_opportunities
             FROM "AdEpochRollups"
            WHERE game_id = $1
              AND ($2::integer IS NULL OR finalized_round <= $2)
            ORDER BY epoch DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(cutoff_round)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(header) = header else {
        return Ok((None, Vec::new(), Vec::new(), Vec::new()));
    };
    let teams = sqlx::query_as::<_, TeamRollupRow>(
        r#"SELECT participation_id, cumulative_points_numerator, cumulative_epoch_weight,
                  cumulative_offense_numerator, cumulative_defense_numerator,
                  cumulative_sla_numerator, cumulative_rate_weight
             FROM "AdEpochTeamRollups"
            WHERE game_id = $1 AND epoch = $2
            ORDER BY participation_id"#,
    )
    .bind(game_id)
    .bind(header.epoch)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let services = load_service_rollups(connection, game_id, header.epoch).await?;
    let recent = sqlx::query_as::<_, RecentTeamEpochRow>(
        r#"WITH recent_epochs AS (
               SELECT epoch FROM "AdEpochRollups"
                WHERE game_id = $1 AND epoch <= $2
                ORDER BY epoch DESC LIMIT 3
           )
           SELECT team.participation_id, team.epoch, team.points, team.epoch_weight
             FROM "AdEpochTeamRollups" team
             JOIN recent_epochs recent ON recent.epoch = team.epoch
            WHERE team.game_id = $1
            ORDER BY team.participation_id, team.epoch"#,
    )
    .bind(game_id)
    .bind(header.epoch)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((Some(header), teams, services, recent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_rollup_keeps_prior_epoch_exact() {
        let row = EvidenceAggregateRow {
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
        };
        let previous = HashMap::from([(
            1,
            PreviousTeamTotals {
                points_numerator: 40.0,
                epoch_weight: 1.0,
                offense_numerator: 0.4,
                defense_numerator: 0.6,
                sla_numerator: 0.8,
                rate_weight: 1.0,
            },
        )]);
        let result = compute_team_rollups(&[row], 0.5, &previous).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].totals.epoch_weight, 1.5);
        assert_eq!(result[0].totals.rate_weight, 1.5);
        assert!(result[0].totals.points_numerator > 40.0);
    }

    /// Transactional SQL-contract test for the nullable carry arrays and
    /// exactly-once header marker. All fixture changes are rolled back.
    #[tokio::test]
    #[ignore = "requires a migrated Postgres with an A&D game"]
    async fn database_epoch_rollup_is_idempotent() {
        let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("test database URL");
        let game_id: i32 = std::env::var("RSCTF_TEST_GAME_ID")
            .expect("test game id")
            .parse()
            .expect("numeric game id");
        let start_round: i32 = std::env::var("RSCTF_TEST_START_ROUND")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let epoch_ticks: i32 = std::env::var("RSCTF_TEST_EPOCH_TICKS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect test database");
        let mut transaction = pool.begin().await.expect("begin transaction");
        sqlx::query(r#"DELETE FROM "AdEpochRollups" WHERE game_id = $1"#)
            .bind(game_id)
            .execute(&mut *transaction)
            .await
            .expect("clear transaction-local rollups");
        sqlx::query(
            r#"UPDATE "AdCheckResults" result SET sla_credit = 0.0
                 FROM "AdRounds" round
                WHERE result.round_id = round.id AND round.game_id = $1
                  AND round.number BETWEEN $2 AND $3
                  AND result.team_service_id IN (
                    SELECT flag.team_service_id
                      FROM "AdFlags" flag
                      JOIN "AdRounds" boundary ON boundary.id = flag.round_id
                     WHERE boundary.game_id = $1 AND boundary.number = $2
                  )"#,
        )
        .bind(game_id)
        .bind(start_round)
        .bind(start_round + epoch_ticks - 1)
        .execute(&mut *transaction)
        .await
        .expect("complete fixture checks");
        let range = EvidenceRange {
            official_start_round: start_round,
            start_round,
            end_round: Some(start_round + epoch_ticks - 1),
            epoch_ticks,
            round_cutoff: None,
            checker_cutoff: None,
            attack_cutoff: None,
            event_end_settlement: false,
        };
        let meta = load_epoch_meta(&mut transaction, game_id, range)
            .await
            .expect("load fixture epoch");
        let epoch = meta.first().expect("fixture epoch exists");
        assert!(epoch.all_checks_complete);
        materialize_epoch(
            &mut transaction,
            game_id,
            start_round,
            epoch_ticks,
            epoch.end_round + 5,
            epoch,
            None,
        )
        .await
        .expect("materialize epoch");
        materialize_epoch(
            &mut transaction,
            game_id,
            start_round,
            epoch_ticks,
            epoch.end_round + 5,
            epoch,
            None,
        )
        .await
        .expect("repeat materialization");
        let counts: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT COUNT(*) FROM "AdEpochRollups" WHERE game_id = $1),
                 (SELECT COUNT(*) FROM "AdEpochServiceRollups" WHERE game_id = $1),
                 (SELECT COUNT(*) FROM "AdEpochTeamRollups" WHERE game_id = $1)"#,
        )
        .bind(game_id)
        .fetch_one(&mut *transaction)
        .await
        .expect("read rollup counts");
        assert_eq!(counts.0, 1);
        assert!(counts.1 > 0);
        assert!(counts.2 > 0);
        let (header, teams, services, recent) =
            load_rollup_snapshot(&mut transaction, game_id, Some(epoch.end_round + 5))
                .await
                .expect("load persisted snapshot");
        assert_eq!(header.expect("snapshot header").epoch, epoch.epoch);
        assert!(!teams.is_empty());
        assert!(!services.is_empty());
        assert!(!recent.is_empty());
        let reconciliation: (i64, i64, bool) = sqlx::query_as(
            r#"SELECT COUNT(*)::bigint,
                      COUNT(service.participation_id)::bigint,
                      COALESCE(BOOL_AND(
                        service.participation_id IS NOT NULL
                        AND ABS(team.cumulative_points_numerator
                          - service.cumulative_points_numerator) <= 0.00000001
                      ), FALSE)
                 FROM "AdEpochTeamRollups" team
                 LEFT JOIN (
                   SELECT game_id, epoch, participation_id,
                          SUM(cumulative_points_numerator)
                            AS cumulative_points_numerator
                     FROM "AdEpochServiceRollups"
                    WHERE game_id = $1 AND epoch = $2
                    GROUP BY game_id, epoch, participation_id
                 ) service
                   ON service.game_id = team.game_id
                  AND service.epoch = team.epoch
                  AND service.participation_id = team.participation_id
                WHERE team.game_id = $1 AND team.epoch = $2"#,
        )
        .bind(game_id)
        .bind(epoch.epoch)
        .fetch_one(&mut *transaction)
        .await
        .expect("reconcile team and service point numerators");
        assert!(reconciliation.0 > 0);
        assert_eq!(reconciliation.1, reconciliation.0);
        assert!(reconciliation.2);
        let (before_finalization, _, _, _) =
            load_rollup_snapshot(&mut transaction, game_id, Some(epoch.end_round + 4))
                .await
                .expect("load pre-finalization cutoff");
        assert!(before_finalization.is_none());
        transaction.rollback().await.expect("rollback fixture");
    }

    #[tokio::test]
    #[ignore = "requires a migrated Postgres with an A&D game"]
    async fn database_rollup_invalidation_keeps_only_the_safe_prefix() {
        let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("test database URL");
        let game_id: i32 = std::env::var("RSCTF_TEST_GAME_ID")
            .expect("test game id")
            .parse()
            .expect("numeric game id");
        let start_round: i32 = std::env::var("RSCTF_TEST_START_ROUND")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect test database");
        let mut transaction = pool.begin().await.expect("begin transaction");

        sqlx::query(
            r#"UPDATE "Games"
                  SET ad_scoring_start_round = $2, ad_epoch_ticks = 8,
                      ad_flag_lifetime_ticks = 5
                WHERE id = $1"#,
        )
        .bind(game_id)
        .bind(start_round)
        .execute(&mut *transaction)
        .await
        .expect("configure fixture game");

        async fn seed(connection: &mut PgConnection, game_id: i32, start: i32) {
            sqlx::query(r#"DELETE FROM "AdEpochRollups" WHERE game_id = $1"#)
                .bind(game_id)
                .execute(&mut *connection)
                .await
                .expect("clear fixture rollups");
            sqlx::query(
                r#"INSERT INTO "AdEpochRollups"
                     (game_id, epoch, start_round, end_round, round_count,
                      epoch_weight, finalized_round, eligible_flags,
                      captured_flags, accepted_captures, defense_opportunities,
                      protected_opportunities, cumulative_eligible_flags,
                      cumulative_captured_flags, cumulative_accepted_captures,
                      cumulative_defense_opportunities,
                      cumulative_protected_opportunities)
                   VALUES
                     ($1, 1, $2,     $2 + 7,  8, 1.0, $2 + 12,
                      0,0,0,0,0, 0,0,0,0,0),
                     ($1, 2, $2 + 8, $2 + 15, 8, 1.0, $2 + 15,
                      0,0,0,0,0, 0,0,0,0,0),
                     ($1, 3, $2 + 16,$2 + 19, 4, 0.5, $2 + 19,
                      0,0,0,0,0, 0,0,0,0,0)"#,
            )
            .bind(game_id)
            .bind(start)
            .execute(connection)
            .await
            .expect("seed fixture rollups");
        }

        async fn epochs(connection: &mut PgConnection, game_id: i32) -> Vec<i32> {
            sqlx::query_scalar(
                r#"SELECT epoch FROM "AdEpochRollups"
                    WHERE game_id = $1 ORDER BY epoch"#,
            )
            .bind(game_id)
            .fetch_all(connection)
            .await
            .expect("read fixture epochs")
        }

        seed(&mut transaction, game_id, start_round).await;
        invalidate_rollups_from_round(&mut transaction, game_id, start_round + 8)
            .await
            .expect("invalidate override suffix");
        assert_eq!(epochs(&mut transaction, game_id).await, vec![1]);

        seed(&mut transaction, game_id, start_round).await;
        let previous_end = Utc::now();
        invalidate_rollups_for_end_change(
            &mut transaction,
            game_id,
            previous_end,
            previous_end + chrono::Duration::hours(1),
        )
        .await
        .expect("reopen full and partial lifetime tail");
        assert_eq!(epochs(&mut transaction, game_id).await, vec![1]);

        seed(&mut transaction, game_id, start_round).await;
        invalidate_rollups_for_end_change(
            &mut transaction,
            game_id,
            previous_end,
            previous_end - chrono::Duration::hours(1),
        )
        .await
        .expect("shorten event");
        assert!(epochs(&mut transaction, game_id).await.is_empty());

        transaction.rollback().await.expect("rollback fixture");
    }
}
