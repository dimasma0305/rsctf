use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection};

use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Debug, FromRow)]
pub(super) struct StableServiceRow {
    pub team_service_id: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub team_id: i32,
    pub team_name: String,
    pub division: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct LatestCheckStatusRow {
    pub participation_id: i32,
    pub challenge_id: i32,
    pub status: i16,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct EpochMetaRow {
    pub epoch: i32,
    pub start_round: i32,
    pub round_count: i64,
    pub all_finalized: bool,
    pub all_checks_complete: bool,
    pub end_round: i32,
    pub current_round: i32,
}

#[derive(Clone, Debug, FromRow)]
pub(super) struct EvidenceAggregateRow {
    pub participation_id: i32,
    pub challenge_id: i32,
    pub epoch: i32,
    pub service_weight: f64,
    pub opportunity_count: i64,
    pub capture_count: i64,
    pub rarity_sum: f64,
    pub defense_opportunity_count: i64,
    pub protected_opportunity_count: i64,
    pub sla_credit_sum: f64,
    pub sla_tick_count: i64,
    pub eligible_flags_total: i64,
    pub captured_flags_total: i64,
    pub accepted_captures_total: i64,
    pub defense_opportunities_total: i64,
    pub protected_opportunities_total: i64,
    pub closing_sla_status: Option<i16>,
    pub closing_sla_credit: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct EvidenceRange {
    pub official_start_round: i32,
    pub start_round: i32,
    pub end_round: Option<i32>,
    pub epoch_ticks: i32,
    pub round_cutoff: Option<DateTime<Utc>>,
    pub checker_cutoff: Option<DateTime<Utc>>,
    pub attack_cutoff: Option<DateTime<Utc>>,
    /// Set only for immutable event-end settlement. Rounds and captures use a
    /// strict end fence; completed checker rows at or beyond `checker_cutoff`
    /// remain present but become a local zero rather than scoring or carrying.
    pub event_end_settlement: bool,
}

/// Freeze the ranked team/service set from flags minted in the declared start
/// round. A later participation or service waits for the next event rather than
/// receiving a full epoch score from one good tick.
pub(super) async fn load_stable_services(
    connection: &mut PgConnection,
    game_id: i32,
    start_round: i32,
    cutoff: Option<DateTime<Utc>>,
    event_end_settlement: bool,
) -> AppResult<Vec<StableServiceRow>> {
    sqlx::query_as::<_, StableServiceRow>(
        r#"SELECT service.id AS team_service_id,
                  service.participation_id, service.challenge_id,
                  team.id AS team_id, team.name AS team_name,
                  division.name AS division
             FROM "AdRounds" round
             JOIN "AdFlags" flag ON flag.round_id = round.id
             JOIN "AdTeamServices" service ON service.id = flag.team_service_id
             JOIN "Participations" participation
               ON participation.id = service.participation_id
              AND participation.game_id = service.game_id
             JOIN "Teams" team ON team.id = participation.team_id
             LEFT JOIN "Divisions" division
               ON division.id = participation.division_id
              AND division.game_id = participation.game_id
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE round.game_id = $1 AND round.number = $2
              AND ($3::timestamptz IS NULL OR CASE WHEN $4::boolean
                     THEN round.start_time_utc < $3
                     ELSE round.start_time_utc <= $3 END)
            ORDER BY participation.id, challenge.id"#,
    )
    .bind(game_id)
    .bind(start_round)
    .bind(cutoff)
    .bind(event_end_settlement)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Load one completed verdict per frozen service with an index-backed lateral
/// seek. `checker_cutoff` freezes a public in-progress view. Event-end
/// settlement additionally includes late completed rows so they can be shown
/// as deterministic `InternalError` zeros without mutating the raw verdict.
pub(super) async fn load_latest_check_statuses(
    connection: &mut PgConnection,
    game_id: i32,
    start_round: i32,
    services: &[StableServiceRow],
    round_cutoff: Option<DateTime<Utc>>,
    checker_cutoff: Option<DateTime<Utc>>,
    event_end_settlement: bool,
) -> AppResult<Vec<LatestCheckStatusRow>> {
    if services.is_empty() {
        return Ok(Vec::new());
    }
    let service_ids: Vec<_> = services.iter().map(|row| row.team_service_id).collect();
    let participation_ids: Vec<_> = services.iter().map(|row| row.participation_id).collect();
    let challenge_ids: Vec<_> = services.iter().map(|row| row.challenge_id).collect();
    sqlx::query_as::<_, LatestCheckStatusRow>(
        r#"WITH round_bounds AS (
               SELECT
                 (SELECT id FROM "AdRounds"
                   WHERE game_id = $4 AND number = $5
                     AND ($6::timestamptz IS NULL OR CASE WHEN $8::boolean
                            THEN start_time_utc < $6
                            ELSE start_time_utc <= $6 END)) AS first_id,
                 (SELECT id FROM "AdRounds"
                   WHERE game_id = $4 AND number >= $5
                     AND ($6::timestamptz IS NULL OR CASE WHEN $8::boolean
                            THEN start_time_utc < $6
                            ELSE start_time_utc <= $6 END)
                   ORDER BY number DESC LIMIT 1) AS last_id
           )
           SELECT stable.participation_id, stable.challenge_id,
                  CASE
                    WHEN $8::boolean AND $7::timestamptz IS NOT NULL
                     AND latest.checked_at >= $7
                      THEN 3::smallint
                    ELSE latest.status
                  END AS status
             FROM UNNEST($1::integer[], $2::integer[], $3::integer[])
                  AS stable(team_service_id, participation_id, challenge_id)
             JOIN round_bounds ON TRUE
             JOIN LATERAL (
               SELECT result.status, result.checked_at
                 FROM "AdCheckResults" result
                WHERE result.team_service_id = stable.team_service_id
                  AND result.sla_credit IS NOT NULL
                  -- Round creation is serialized per game, so its sequence ID
                  -- increases with number. This bound lets the existing
                  -- (team_service_id, round_id) index skip the frozen suffix.
                  AND result.round_id BETWEEN round_bounds.first_id AND round_bounds.last_id
                  AND ($8::boolean OR $7::timestamptz IS NULL OR result.checked_at <= $7)
                ORDER BY result.round_id DESC
                LIMIT 1
             ) latest ON TRUE
            ORDER BY stable.participation_id, stable.challenge_id"#,
    )
    .bind(&service_ids)
    .bind(&participation_ids)
    .bind(&challenge_ids)
    .bind(game_id)
    .bind(start_round)
    .bind(round_cutoff)
    .bind(checker_cutoff)
    .bind(event_end_settlement)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub(super) async fn load_epoch_meta(
    connection: &mut PgConnection,
    game_id: i32,
    range: EvidenceRange,
) -> AppResult<Vec<EpochMetaRow>> {
    sqlx::query_as::<_, EpochMetaRow>(
        r#"WITH stable_services AS (
               SELECT service.id
                 FROM "AdRounds" boundary
                 JOIN "AdFlags" flag ON flag.round_id = boundary.id
                 JOIN "AdTeamServices" service ON service.id = flag.team_service_id
                WHERE boundary.game_id = $1 AND boundary.number = $2
           ), epoch_rounds AS (
               SELECT id, number, finalized,
                      ((number - $2) / $3) + 1 AS epoch
                 FROM "AdRounds"
                WHERE game_id = $1 AND number >= $4
                  AND ($5::integer IS NULL OR number <= $5)
                  AND ($6::timestamptz IS NULL OR CASE WHEN $8::boolean
                         THEN start_time_utc < $6
                         ELSE start_time_utc <= $6 END)
           ), completed_checks AS (
               SELECT result.round_id, COUNT(*)::bigint AS completed
                 FROM "AdCheckResults" result
                 JOIN stable_services stable ON stable.id = result.team_service_id
                 JOIN epoch_rounds round ON round.id = result.round_id
                WHERE result.sla_credit IS NOT NULL
                  AND ($8::boolean OR $7::timestamptz IS NULL OR result.checked_at <= $7)
                GROUP BY result.round_id
           ), round_completion AS (
               SELECT round.*,
                      COALESCE(checks.completed, 0) =
                        (SELECT COUNT(*) FROM stable_services) AS checks_complete
                 FROM epoch_rounds round
                 LEFT JOIN completed_checks checks ON checks.round_id = round.id
           ), current_round AS (
               SELECT COALESCE(MAX(number), 0)::integer AS number
                 FROM "AdRounds"
                WHERE game_id = $1
                  AND ($6::timestamptz IS NULL OR CASE WHEN $8::boolean
                         THEN start_time_utc < $6
                         ELSE start_time_utc <= $6 END)
           )
           SELECT epoch, MIN(number)::integer AS start_round,
                  COUNT(*)::bigint AS round_count,
                  BOOL_AND(finalized) AS all_finalized,
                  BOOL_AND(checks_complete) AS all_checks_complete,
                  MAX(number)::integer AS end_round,
                  (SELECT number FROM current_round) AS current_round
             FROM round_completion
            GROUP BY epoch
            ORDER BY epoch"#,
    )
    .bind(game_id)
    .bind(range.official_start_round)
    .bind(range.epoch_ticks)
    .bind(range.start_round)
    .bind(range.end_round)
    .bind(range.round_cutoff)
    .bind(range.checker_cutoff)
    .bind(range.event_end_settlement)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Aggregate all raw flag/check/capture history in PostgreSQL. The application
/// receives one bounded row per frozen service and epoch, never the growing raw
/// event tables.
///
/// SLA uses a common frozen-roster denominator. A missing expected row is a
/// zero. A later infrastructure error carries the service's last adjudicated
/// status/credit. A challenge-round is void for the frozen field only when
/// every service reports an infrastructure error without a prior adjudicated
/// sample; an isolated first error is local zero credit, preventing one team
/// from vetoing everyone else's SLA denominator.
pub(super) async fn load_epoch_evidence(
    connection: &mut PgConnection,
    game_id: i32,
    range: EvidenceRange,
) -> AppResult<Vec<EvidenceAggregateRow>> {
    sqlx::query_as::<_, EvidenceAggregateRow>(
        r#"WITH scoring_rounds AS (
               SELECT id, number, ((number - $2) / $3) + 1 AS epoch
                 FROM "AdRounds"
                WHERE game_id = $1 AND number >= $4
                  AND ($5::integer IS NULL OR number <= $5)
                  AND ($6::timestamptz IS NULL OR CASE WHEN $9::boolean
                         THEN start_time_utc < $6
                         ELSE start_time_utc <= $6 END)
           ), official_start_round AS (
               SELECT id
                 FROM "AdRounds"
                WHERE game_id = $1 AND number = $2
                  AND ($6::timestamptz IS NULL OR CASE WHEN $9::boolean
                         THEN start_time_utc < $6
                         ELSE start_time_utc <= $6 END)
           ), stable_services AS (
               SELECT service.id, service.participation_id, service.challenge_id,
                      flag.service_weight
                 FROM official_start_round
                 JOIN "AdFlags" flag ON flag.round_id = official_start_round.id
                 JOIN "AdTeamServices" service ON service.id = flag.team_service_id
                 JOIN "Participations" participation
                   ON participation.id = service.participation_id
                  AND participation.game_id = service.game_id
                 JOIN "Teams" team ON team.id = participation.team_id
                 JOIN "GameChallenges" challenge
                   ON challenge.id = service.challenge_id
                  AND challenge.game_id = service.game_id
           ), stable_participants AS (
               SELECT DISTINCT challenge_id, participation_id FROM stable_services
           ), roster_sizes AS (
               SELECT challenge_id, COUNT(*)::bigint AS team_count
                 FROM stable_services
                GROUP BY challenge_id
           ), seed_state AS (
               SELECT stable.id AS team_service_id,
                      seed.closing_sla_status, seed.closing_sla_credit
                 FROM stable_services stable
                 LEFT JOIN LATERAL (
                   SELECT rollup.closing_sla_status, rollup.closing_sla_credit
                     FROM "AdEpochServiceRollups" rollup
                    WHERE rollup.game_id = $1
                      AND rollup.participation_id = stable.participation_id
                      AND rollup.challenge_id = stable.challenge_id
                      AND rollup.epoch < (($4 - $2) / $3) + 1
                    ORDER BY rollup.epoch DESC
                    LIMIT 1
                 ) seed ON TRUE
           ),
           -- Round preparation inserts a NULL-credit placeholder before the
           -- checker runs. It is not evidence: excluding it keeps a fixed freeze
           -- cutoff stable when the later UPSERT records its completion time.
           raw_check_history AS (
               SELECT check_result.team_service_id, round.number AS round_number,
                      check_result.status, check_result.flag_verified,
                      check_result.checked_at,
                      COALESCE(NOT delivery.delivered, FALSE) AS platform_void,
                      ($9::boolean AND $7::timestamptz IS NOT NULL
                        AND check_result.checked_at >= $7) AS settlement_zero,
                      ($9::boolean AND $7::timestamptz IS NOT NULL
                        AND check_result.checked_at = $7
                        AND check_result.status = 3
                        AND check_result.sla_credit = 0.0
                        AND check_result.flag_verified = FALSE
                        AND check_result.message IN (
                          'checker pass did not complete before event-close grace expired',
                          'checker pass cancelled before completion'
                        )) IS TRUE AS recognized_boundary_zero
                FROM "AdCheckResults" check_result
                 JOIN stable_services stable ON stable.id = check_result.team_service_id
                 JOIN scoring_rounds round ON round.id = check_result.round_id
                 LEFT JOIN "AdFlagDeliveryResults" delivery
                   ON delivery.round_id = check_result.round_id
                  AND delivery.team_service_id = check_result.team_service_id
                WHERE check_result.sla_credit IS NOT NULL
                  AND ($9::boolean OR $7::timestamptz IS NULL
                       OR check_result.checked_at <= $7)
           ), check_history AS (
               SELECT team_service_id, round_number,
                      CASE
                        WHEN settlement_zero AND NOT recognized_boundary_zero
                          THEN 3::smallint
                        ELSE status
                      END AS status,
                      CASE
                        WHEN settlement_zero AND NOT recognized_boundary_zero THEN FALSE
                        ELSE flag_verified
                      END AS flag_verified,
                      platform_void,
                      settlement_zero AS forced_zero
                 FROM raw_check_history
           ), check_links AS (
               SELECT check_history.*,
                      MAX(round_number) FILTER (WHERE status BETWEEN 0 AND 2) OVER (
                          PARTITION BY team_service_id ORDER BY round_number
                          ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
                      ) AS previous_noninfra_round
                 FROM check_history
           ), noninfra_credit AS (
               SELECT current.team_service_id, current.round_number,
                      CASE
                        WHEN current.status = 0
                         AND COALESCE(previous.status, seed.closing_sla_status) IN (1, 2)
                          THEN 0.5::float8
                        WHEN current.status = 0 THEN 1.0::float8
                        ELSE 0.0::float8
                      END AS credit,
                      current.status
                 FROM check_links current
                 LEFT JOIN check_history previous
                  ON previous.team_service_id = current.team_service_id
                  AND previous.round_number = current.previous_noninfra_round
                 JOIN seed_state seed ON seed.team_service_id = current.team_service_id
                WHERE current.status BETWEEN 0 AND 2
                  AND NOT current.platform_void
           ), timeline_points AS (
               SELECT stable_services.id AS team_service_id,
                      scoring_rounds.number AS round_number
                 FROM stable_services CROSS JOIN scoring_rounds
           ), credit_timeline AS (
               SELECT point.team_service_id, point.round_number,
                      history.status, history.flag_verified,
                      COALESCE(history.platform_void, FALSE) AS platform_void,
                      COALESCE(history.forced_zero, FALSE) AS forced_zero,
                      own.credit AS own_credit
                 FROM timeline_points point
                 LEFT JOIN check_history history
                   ON history.team_service_id = point.team_service_id
                  AND history.round_number = point.round_number
                 LEFT JOIN noninfra_credit own
                   ON own.team_service_id = point.team_service_id
                  AND own.round_number = point.round_number
           ), linked_timeline AS (
               SELECT credit_timeline.*,
                      MAX(round_number) FILTER (WHERE own_credit IS NOT NULL) OVER (
                          PARTITION BY team_service_id ORDER BY round_number
                          ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING
                      ) AS previous_credit_round
                 FROM credit_timeline
           ), sla_grid AS (
               SELECT stable.id AS team_service_id, stable.participation_id,
                      stable.challenge_id, round.id AS round_id,
                      round.number AS round_number, round.epoch,
                      CASE
                        WHEN timeline.platform_void THEN 0.0::float8
                        WHEN timeline.status IS NULL THEN 0.0::float8
                        WHEN timeline.forced_zero THEN 0.0::float8
                        WHEN timeline.status BETWEEN 0 AND 2 THEN timeline.own_credit
                        ELSE COALESCE(previous.credit, seed.closing_sla_credit)
                      END AS effective_credit,
                      timeline.platform_void AS personal_void,
                      timeline.status IS NOT NULL
                        AND NOT timeline.platform_void
                        AND NOT timeline.forced_zero
                        AND NOT (timeline.status BETWEEN 0 AND 2)
                        AND previous.credit IS NULL
                        AND seed.closing_sla_credit IS NULL AS infra_without_prior
                 FROM stable_services stable
                 CROSS JOIN scoring_rounds round
                 JOIN linked_timeline timeline
                   ON timeline.team_service_id = stable.id
                  AND timeline.round_number = round.number
                 LEFT JOIN noninfra_credit previous
                  ON previous.team_service_id = stable.id
                  AND previous.round_number = timeline.previous_credit_round
                 JOIN seed_state seed ON seed.team_service_id = stable.id
           ), field_sla_ticks AS (
               SELECT challenge_id, round_id, epoch,
                      COALESCE(
                        BOOL_AND(infra_without_prior) FILTER (WHERE NOT personal_void),
                        FALSE
                      ) AS void_tick
                 FROM sla_grid
                GROUP BY challenge_id, round_id, epoch
           ), sla_evidence AS (
               SELECT grid.participation_id, grid.challenge_id, grid.epoch,
                      COALESCE(SUM(grid.effective_credit)
                          FILTER (WHERE NOT field.void_tick AND NOT grid.personal_void),
                          0.0)::float8 AS credit_sum,
                      COUNT(*) FILTER (
                        WHERE NOT field.void_tick AND NOT grid.personal_void
                      )::bigint AS tick_count
                 FROM sla_grid grid
                 JOIN field_sla_ticks field
                   ON field.challenge_id = grid.challenge_id
                  AND field.round_id = grid.round_id
                  AND field.epoch = grid.epoch
                GROUP BY grid.participation_id, grid.challenge_id, grid.epoch
           ),
           -- Select every service's latest eligible closing sample in one
           -- ordered pass. A platform void is never eligible; a forced event-
           -- end zero remains eligible even though it has no noninfra credit.
           latest_closing_sla AS (
               SELECT DISTINCT ON (history.team_service_id)
                      history.team_service_id, history.status,
                      CASE WHEN history.forced_zero THEN 0.0::float8
                           ELSE credit.credit END AS credit
                 FROM check_history history
                 LEFT JOIN noninfra_credit credit
                   ON credit.team_service_id = history.team_service_id
                  AND credit.round_number = history.round_number
                WHERE NOT history.platform_void
                  AND (history.forced_zero OR credit.credit IS NOT NULL)
                ORDER BY history.team_service_id, history.round_number DESC
           ), closing_sla AS (
               SELECT stable.participation_id, stable.challenge_id,
                      COALESCE(latest.status, seed.closing_sla_status) AS closing_sla_status,
                      COALESCE(latest.credit, seed.closing_sla_credit) AS closing_sla_credit
                 FROM stable_services stable
                 JOIN seed_state seed ON seed.team_service_id = stable.id
                 LEFT JOIN latest_closing_sla latest
                   ON latest.team_service_id = stable.id
           ), stable_flags AS (
               SELECT flag.id, flag.round_id, flag.team_service_id,
                      stable.participation_id AS victim_id,
                      stable.challenge_id, round.number AS round_number, round.epoch,
                      flag.checker_qualified
                 FROM scoring_rounds round
                 JOIN "AdFlags" flag ON flag.round_id = round.id
                 JOIN stable_services stable ON stable.id = flag.team_service_id
           ), stable_captures AS (
               SELECT DISTINCT attack.flag_id, attack.attacker_participation_id
                 FROM "AdAttacks" attack
                 JOIN stable_flags flag ON flag.id = attack.flag_id
                 JOIN stable_participants attacker
                   ON attacker.challenge_id = flag.challenge_id
                  AND attacker.participation_id = attack.attacker_participation_id
                WHERE attack.attacker_participation_id <> flag.victim_id
                  AND ($8::timestamptz IS NULL OR CASE WHEN $9::boolean
                         THEN attack.submitted_at < $8
                         ELSE attack.submitted_at <= $8 END)
           ), capture_stats AS (
               SELECT flag_id, COUNT(*)::bigint AS capture_count
                 FROM stable_captures
                GROUP BY flag_id
           ), flag_outcomes AS (
               SELECT flag.*, roster.team_count - 1 AS opponents,
                      LEAST(COALESCE(captures.capture_count, 0), roster.team_count - 1)
                        AS capture_count,
                      COALESCE(delivery.delivered, TRUE) AS flag_delivered,
                      flag.checker_qualified
                        AND history.status = 0
                        AND history.flag_verified AS exact_checked
                 FROM stable_flags flag
                 JOIN roster_sizes roster ON roster.challenge_id = flag.challenge_id
                 LEFT JOIN capture_stats captures ON captures.flag_id = flag.id
                 LEFT JOIN "AdFlagDeliveryResults" delivery
                   ON delivery.round_id = flag.round_id
                  AND delivery.team_service_id = flag.team_service_id
                 LEFT JOIN check_history history
                   ON history.team_service_id = flag.team_service_id
                  AND history.round_number = flag.round_number
                WHERE roster.team_count > 1
           ), qualified_flags AS (
               SELECT *, opponents - capture_count AS protected_count,
                      CASE WHEN opponents >= 4
                           THEN (opponents - capture_count)::float8 / opponents::float8
                           ELSE 0.0::float8 END AS rarity_fraction
                 FROM flag_outcomes
                WHERE flag_delivered AND (exact_checked OR capture_count > 0)
           ), eligible_flag_totals AS (
               SELECT challenge_id, epoch, COUNT(*)::bigint AS eligible_flags
                 FROM qualified_flags
                GROUP BY challenge_id, epoch
           ), own_eligible_flags AS (
               SELECT victim_id AS participation_id, challenge_id, epoch,
                      COUNT(*)::bigint AS own_flags
                 FROM qualified_flags
                GROUP BY victim_id, challenge_id, epoch
           ), capture_evidence AS (
               SELECT capture.attacker_participation_id AS participation_id,
                      flag.challenge_id, flag.epoch, COUNT(*)::bigint AS captures,
                      SUM(flag.rarity_fraction)::float8 AS rarity_sum
                 FROM stable_captures capture
                 JOIN qualified_flags flag ON flag.id = capture.flag_id
                GROUP BY capture.attacker_participation_id, flag.challenge_id, flag.epoch
           ), defense_evidence AS (
               SELECT victim_id AS participation_id, challenge_id, epoch,
                      SUM(opponents)::bigint AS opportunities,
                      SUM(protected_count)::bigint AS protected
                 FROM qualified_flags
                WHERE exact_checked
                GROUP BY victim_id, challenge_id, epoch
           ), epoch_ids AS (
               SELECT DISTINCT epoch FROM scoring_rounds
           ), service_epoch_grid AS (
               SELECT stable.participation_id, stable.challenge_id, stable.service_weight,
                      epoch_ids.epoch
                 FROM stable_services stable CROSS JOIN epoch_ids
           ), evidence_rows AS (
               SELECT grid.participation_id, grid.challenge_id, grid.epoch,
                      grid.service_weight,
                      GREATEST(COALESCE(total.eligible_flags, 0)
                          - COALESCE(own.own_flags, 0), 0)::bigint AS opportunity_count,
                      COALESCE(capture.captures, 0)::bigint AS capture_count,
                      COALESCE(capture.rarity_sum, 0.0)::float8 AS rarity_sum,
                      COALESCE(defense.opportunities, 0)::bigint AS defense_opportunity_count,
                      COALESCE(defense.protected, 0)::bigint AS protected_opportunity_count,
                      COALESCE(sla.credit_sum, 0.0)::float8 AS sla_credit_sum,
                      COALESCE(sla.tick_count, 0)::bigint AS sla_tick_count,
                      closing.closing_sla_status,
                      closing.closing_sla_credit
                 FROM service_epoch_grid grid
                 LEFT JOIN eligible_flag_totals total
                   ON total.challenge_id = grid.challenge_id AND total.epoch = grid.epoch
                 LEFT JOIN own_eligible_flags own
                   ON own.participation_id = grid.participation_id
                  AND own.challenge_id = grid.challenge_id AND own.epoch = grid.epoch
                 LEFT JOIN capture_evidence capture
                   ON capture.participation_id = grid.participation_id
                  AND capture.challenge_id = grid.challenge_id AND capture.epoch = grid.epoch
                 LEFT JOIN defense_evidence defense
                   ON defense.participation_id = grid.participation_id
                  AND defense.challenge_id = grid.challenge_id AND defense.epoch = grid.epoch
                 LEFT JOIN sla_evidence sla
                   ON sla.participation_id = grid.participation_id
                  AND sla.challenge_id = grid.challenge_id AND sla.epoch = grid.epoch
                 JOIN closing_sla closing
                   ON closing.participation_id = grid.participation_id
                  AND closing.challenge_id = grid.challenge_id
           ), evidence_status AS (
               SELECT COUNT(*)::bigint AS eligible_flags_total,
                      COUNT(*) FILTER (WHERE capture_count > 0)::bigint AS captured_flags_total,
                      COALESCE(SUM(capture_count), 0)::bigint AS accepted_captures_total,
                      COALESCE(SUM(opponents) FILTER (WHERE exact_checked), 0)::bigint
                        AS defense_opportunities_total,
                      COALESCE(SUM(protected_count) FILTER (WHERE exact_checked), 0)::bigint
                        AS protected_opportunities_total
                 FROM qualified_flags
           )
           SELECT evidence_rows.*, evidence_status.*
             FROM evidence_rows CROSS JOIN evidence_status
            ORDER BY participation_id, epoch, challenge_id"#,
    )
    .bind(game_id)
    .bind(range.official_start_round)
    .bind(range.epoch_ticks)
    .bind(range.start_round)
    .bind(range.end_round)
    .bind(range.round_cutoff)
    .bind(range.checker_cutoff)
    .bind(range.attack_cutoff)
    .bind(range.event_end_settlement)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::services::ad_engine::AdCheckStatus;

    use super::{load_epoch_evidence, load_epoch_meta, load_stable_services, EvidenceRange};

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Adjudicated {
        credit: Option<f64>,
        effective: Option<AdCheckStatus>,
    }

    // Golden policy mirrored by the check-history CTE above and by the simulator.
    fn adjudicate(previous: Option<Adjudicated>, current: Option<AdCheckStatus>) -> Adjudicated {
        match current {
            None => Adjudicated {
                credit: Some(0.0),
                effective: None,
            },
            Some(AdCheckStatus::InternalError) => previous.unwrap_or(Adjudicated {
                credit: None,
                effective: None,
            }),
            Some(status) => Adjudicated {
                credit: Some(match status {
                    AdCheckStatus::Ok
                        if previous.is_some_and(|sample| {
                            matches!(
                                sample.effective,
                                Some(AdCheckStatus::Mumble | AdCheckStatus::Offline)
                            )
                        }) =>
                    {
                        0.5
                    }
                    AdCheckStatus::Ok => 1.0,
                    _ => 0.0,
                }),
                effective: Some(status),
            },
        }
    }

    #[test]
    fn internal_error_carries_prior_credit_and_effective_status() {
        let offline = adjudicate(None, Some(AdCheckStatus::Offline));
        let fault = adjudicate(Some(offline), Some(AdCheckStatus::InternalError));
        assert_eq!(fault, offline);
        assert_eq!(
            adjudicate(Some(fault), Some(AdCheckStatus::Ok)).credit,
            Some(0.5)
        );
    }

    #[test]
    fn first_internal_error_voids_but_missing_expected_sample_is_zero() {
        assert_eq!(
            adjudicate(None, Some(AdCheckStatus::InternalError)).credit,
            None
        );
        assert_eq!(adjudicate(None, None).credit, Some(0.0));
    }

    #[test]
    fn one_team_cannot_void_a_field_tick() {
        let first_internal = adjudicate(None, Some(AdCheckStatus::InternalError));
        let healthy = adjudicate(None, Some(AdCheckStatus::Ok));

        // The SQL uses BOOL_AND across the frozen challenge roster. An isolated
        // first InternalError therefore earns zero locally instead of vetoing
        // every other team's SLA denominator.
        assert!(![first_internal, healthy]
            .into_iter()
            .all(|sample| sample.credit.is_none()));
        assert!([first_internal, first_internal]
            .into_iter()
            .all(|sample| sample.credit.is_none()));
    }

    #[test]
    fn official_boundary_does_not_inherit_preboundary_verdict() {
        let preboundary = adjudicate(None, Some(AdCheckStatus::Offline));
        assert_eq!(
            adjudicate(Some(preboundary), Some(AdCheckStatus::Ok)).credit,
            Some(0.5)
        );

        // The SQL excludes pre-boundary history, so the same first official
        // sample is adjudicated from an empty state.
        assert_eq!(adjudicate(None, Some(AdCheckStatus::Ok)).credit, Some(1.0));
        assert_eq!(
            adjudicate(None, Some(AdCheckStatus::InternalError)).credit,
            None
        );
    }

    /// Read-only query-contract smoke test for a real migrated Postgres. Run with:
    /// `RSCTF_TEST_DATABASE_URL=... RSCTF_TEST_GAME_ID=... cargo test
    /// database_aggregate_is_bounded_by_stable_roster -- --ignored`.
    #[tokio::test]
    #[ignore = "requires a migrated Postgres with an A&D game"]
    async fn database_aggregate_is_bounded_by_stable_roster() {
        let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("test database URL");
        let game_id: i32 = std::env::var("RSCTF_TEST_GAME_ID")
            .expect("test game id")
            .parse()
            .expect("numeric game id");
        let start_round = std::env::var("RSCTF_TEST_START_ROUND")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1);
        let epoch_ticks = std::env::var("RSCTF_TEST_EPOCH_TICKS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect(&url)
            .await
            .expect("connect test database");

        let mut transaction = pool.begin().await.expect("begin read transaction");
        let services = load_stable_services(&mut transaction, game_id, start_round, None, false)
            .await
            .expect("stable roster query");
        let range = EvidenceRange {
            official_start_round: start_round,
            start_round,
            end_round: None,
            epoch_ticks,
            round_cutoff: None,
            checker_cutoff: None,
            attack_cutoff: None,
            event_end_settlement: false,
        };
        let epochs = load_epoch_meta(&mut transaction, game_id, range)
            .await
            .expect("epoch metadata query");
        let evidence = load_epoch_evidence(&mut transaction, game_id, range)
            .await
            .expect("aggregated evidence query");
        let stable: HashSet<_> = services
            .iter()
            .map(|service| (service.participation_id, service.challenge_id))
            .collect();

        let unexpected: Vec<_> = evidence
            .iter()
            .map(|row| (row.participation_id, row.challenge_id))
            .filter(|key| !stable.contains(key))
            .take(10)
            .collect();
        assert!(
            unexpected.is_empty(),
            "aggregate rows outside stable roster: {unexpected:?}; stable sample: {:?}",
            stable.iter().take(10).collect::<Vec<_>>()
        );
        assert!(evidence.len() <= stable.len().saturating_mul(epochs.len()));
        assert!(evidence.iter().all(|row| {
            row.opportunity_count >= 0
                && row.capture_count >= 0
                && row.capture_count <= row.opportunity_count
                && row.defense_opportunity_count >= row.protected_opportunity_count
                && row.sla_tick_count >= 0
                && row.sla_credit_sum >= 0.0
                && row.sla_credit_sum <= row.sla_tick_count as f64
        }));
    }
}

#[cfg(test)]
#[path = "evidence_cutoff_tests.rs"]
mod cutoff_tests;
