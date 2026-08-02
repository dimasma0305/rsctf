//! Evidence loading from immutable crown-cycle snapshots.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgConnection, PgPool};

use super::{HillEpochMetaRow, TeamEvidenceRow};
use crate::utils::enums::AdCheckStatus;
use crate::utils::error::{AppError, AppResult};

#[derive(Clone, Debug, FromRow)]
pub(super) struct OfficialConfigRow {
    pub(super) scoring_start_round: i32,
    pub(super) epoch_ticks: i32,
}

pub(super) async fn load_official_config(
    pool: &PgPool,
    game_id: i32,
) -> AppResult<Option<OfficialConfigRow>> {
    sqlx::query_as(
        r#"SELECT scoring_start_round, epoch_ticks
             FROM "KothOfficialConfigs"
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Load one bounded metadata row per frozen hill/epoch and one dense evidence
/// row per frozen team/hill/epoch. Only exact cycle/container receipts written
/// while a cycle is Active or Completed can become scorable evidence. A legacy
/// or event-close `InternalError` void without a cycle id may satisfy round
/// completion, but it remains non-scorable and contributes to no team's
/// denominator.
#[allow(clippy::too_many_arguments)]
pub(super) async fn load_evidence(
    connection: &mut PgConnection,
    game_id: i32,
    official_start_round: i32,
    range_start_round: i32,
    epoch_ticks: i32,
    round_cutoff: Option<DateTime<Utc>>,
    checker_cutoff: Option<DateTime<Utc>>,
    event_ended: bool,
) -> AppResult<(Vec<HillEpochMetaRow>, Vec<TeamEvidenceRow>)> {
    let meta = sqlx::query_as::<_, HillEpochMetaRow>(
        r#"WITH config AS (
               SELECT game_id, scoring_start_round,
                      epoch_ticks, roster_snapshot, hills_snapshot
                 FROM "KothOfficialConfigs"
                WHERE game_id = $1
                  AND scoring_start_round = $2 AND epoch_ticks = $3
           ), hills AS (
               SELECT (item->>'challengeId')::integer AS challenge_id,
                      COALESCE(NULLIF(item->>'claimSource', ''), 'Marker')
                        AS claim_source,
                      LEAST(1.2, GREATEST(0.8,
                        (item->>'serviceWeight')::double precision)) AS service_weight
                 FROM config,
                      LATERAL jsonb_array_elements(config.hills_snapshot) item
           ), scoring_rounds AS (
               SELECT round.id, round.number,
                      ((round.number - $2) / $3) + 1 AS epoch,
                      round.finalized
                 FROM "AdRounds" round
                WHERE round.game_id = $1 AND round.number >= $2
                  AND round.number >= $4
                  AND ($5::timestamptz IS NULL
                       OR (NOT $7 AND round.start_time_utc <= $5)
                       OR ($7 AND round.start_time_utc < $5))
           ), observations AS (
               SELECT scoring_round.id AS round_id, scoring_round.number,
                      scoring_round.epoch, scoring_round.finalized,
                      hill.challenge_id, hill.claim_source, hill.service_weight,
                      result.cycle_id, result.token_window_attempt,
                      result.id AS result_id,
                      CASE WHEN result.id IS NULL THEN FALSE
                           WHEN $6::timestamptz IS NULL
                             OR (NOT $7 AND result.checked_at <= $6)
                             OR ($7 AND result.checked_at < $6)
                           THEN result.is_scorable ELSE FALSE END AS is_scorable,
                      CASE WHEN result.id IS NULL THEN NULL
                           WHEN $6::timestamptz IS NULL
                             OR (NOT $7 AND result.checked_at <= $6)
                             OR ($7 AND result.checked_at < $6)
                           THEN result.checked_at ELSE $6 END AS checked_at
                 FROM scoring_rounds scoring_round
                 CROSS JOIN hills hill
                 LEFT JOIN "KothCrownCycles" cycle
                   ON cycle.game_id = $1
                  AND cycle.challenge_id = hill.challenge_id
                  AND scoring_round.number BETWEEN cycle.planned_start_round
                                               AND cycle.planned_end_round
                 LEFT JOIN "KothControlResults" result
                   ON result.game_id = $1
                  AND result.challenge_id = hill.challenge_id
                  AND result.ad_round_id = scoring_round.id
                  AND (result.cycle_id = cycle.id
                       OR (result.cycle_id IS NULL AND result.is_scorable = FALSE
                           AND result.status = $8))
                  AND (result.is_scorable = FALSE
                       OR result.container_id = cycle.replacement_container_id
                       OR result.container_id = cycle.old_container_id
                       OR EXISTS (
                            SELECT 1 FROM "KothCycleAuditReceipts" receipt
                             WHERE receipt.cycle_id = cycle.id
                               AND result.container_id IN (
                                 receipt.receipt->>'replacementContainerId',
                                 receipt.receipt->>'containerId'
                               )
                       ))
           )
           SELECT challenge_id, MAX(claim_source) AS claim_source, epoch,
                  MIN(number)::integer AS start_round,
                  MAX(number)::integer AS end_round,
                  MAX(service_weight) AS service_weight,
                  COUNT(*)::bigint AS round_count,
                  COUNT(result_id)::bigint AS result_count,
                  COUNT(*) FILTER (WHERE result_id IS NOT NULL AND is_scorable)::bigint
                    AS scorable_ticks,
                  CASE WHEN MAX(claim_source) = 'Api'
                    THEN COUNT(*) FILTER (
                      WHERE result_id IS NOT NULL AND is_scorable
                    )
                    ELSE COUNT(DISTINCT (cycle_id, token_window_attempt)) FILTER (
                      WHERE result_id IS NOT NULL AND is_scorable
                    )
                  END::bigint AS eligible_windows,
                  BOOL_AND(finalized) AS all_finalized,
                  MAX(checked_at) AS max_checked_at
             FROM observations
            GROUP BY challenge_id, epoch
            ORDER BY epoch, challenge_id"#,
    )
    .bind(game_id)
    .bind(official_start_round)
    .bind(epoch_ticks)
    .bind(range_start_round)
    .bind(round_cutoff)
    .bind(checker_cutoff)
    .bind(event_ended)
    .bind(AdCheckStatus::InternalError as i16)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let team = sqlx::query_as::<_, TeamEvidenceRow>(
        r#"WITH config AS (
               SELECT game_id, scoring_start_round,
                      epoch_ticks, roster_snapshot, hills_snapshot
                 FROM "KothOfficialConfigs"
                WHERE game_id = $1
                  AND scoring_start_round = $2 AND epoch_ticks = $3
           ), roster AS (
               SELECT value::integer AS participation_id
                 FROM config,
                      LATERAL jsonb_array_elements_text(config.roster_snapshot) value
           ), hills AS (
               SELECT (item->>'challengeId')::integer AS challenge_id,
                      COALESCE(NULLIF(item->>'claimSource', ''), 'Marker')
                        AS claim_source
                 FROM config,
                      LATERAL jsonb_array_elements(config.hills_snapshot) item
           ), scoring_rounds AS (
               SELECT round.id, round.number,
                      ((round.number - $2) / $3) + 1 AS epoch
                 FROM "AdRounds" round
                WHERE round.game_id = $1 AND round.number >= $2
                  AND round.number >= $4
                  AND ($5::timestamptz IS NULL
                       OR (NOT $7 AND round.start_time_utc <= $5)
                       OR ($7 AND round.start_time_utc < $5))
           ), observations AS (
               SELECT scoring_round.id AS round_id, scoring_round.number,
                      scoring_round.epoch, hill.challenge_id, hill.claim_source,
                      result.cycle_id,
                      result.token_window_attempt,
                      result.id AS result_id,
                      CASE WHEN result.id IS NULL THEN FALSE
                           WHEN $6::timestamptz IS NULL
                             OR (NOT $7 AND result.checked_at <= $6)
                             OR ($7 AND result.checked_at < $6)
                           THEN result.is_scorable ELSE FALSE END AS is_scorable,
                      CASE WHEN $6::timestamptz IS NULL
                             OR (NOT $7 AND result.checked_at <= $6)
                             OR ($7 AND result.checked_at < $6)
                           THEN result.status ELSE $8 END AS status,
                      CASE WHEN $6::timestamptz IS NULL
                             OR (NOT $7 AND result.checked_at <= $6)
                             OR ($7 AND result.checked_at < $6)
                           THEN result.controlling_participation_id END
                        AS controlling_participation_id,
                      CASE WHEN $6::timestamptz IS NULL
                             OR (NOT $7 AND result.checked_at <= $6)
                             OR ($7 AND result.checked_at < $6)
                           THEN result.responsible_participation_id END
                        AS responsible_participation_id
                 FROM scoring_rounds scoring_round
                 CROSS JOIN hills hill
                 LEFT JOIN "KothCrownCycles" cycle
                   ON cycle.game_id = $1
                  AND cycle.challenge_id = hill.challenge_id
                  AND scoring_round.number BETWEEN cycle.planned_start_round
                                               AND cycle.planned_end_round
                 LEFT JOIN "KothControlResults" result
                   ON result.game_id = $1
                  AND result.challenge_id = hill.challenge_id
                  AND result.ad_round_id = scoring_round.id
                  AND (result.cycle_id = cycle.id
                       OR (result.cycle_id IS NULL AND result.is_scorable = FALSE
                           AND result.status = $8))
                  AND (result.is_scorable = FALSE
                       OR result.container_id = cycle.replacement_container_id
                       OR result.container_id = cycle.old_container_id
                       OR EXISTS (
                            SELECT 1 FROM "KothCycleAuditReceipts" receipt
                             WHERE receipt.cycle_id = cycle.id
                               AND result.container_id IN (
                                 receipt.receipt->>'replacementContainerId',
                                 receipt.receipt->>'containerId'
                               )
                       ))
           ), acquisitions AS (
               SELECT acquisition.game_id, acquisition.challenge_id,
                      acquisition.participation_id, acquisition.cycle_id,
                      token.reset_attempt AS token_window_attempt
                 FROM "KothAcquisitions" acquisition
                 JOIN "KothTokens" token
                   ON token.id = acquisition.token_id
                  AND token.cycle_id = acquisition.cycle_id
                WHERE acquisition.game_id = $1
                  AND ($6::timestamptz IS NULL
                       OR (NOT $7 AND acquisition.confirmed_at <= $6)
                       OR ($7 AND acquisition.confirmed_at < $6))
                GROUP BY acquisition.game_id, acquisition.challenge_id,
                         acquisition.participation_id, acquisition.cycle_id,
                         token.reset_attempt
           ), epoch_numbers AS (
               SELECT DISTINCT epoch FROM scoring_rounds
           ), api_ticks_raw AS (
               SELECT api_score.participation_id,
                      observation.challenge_id, observation.epoch,
                      observation.number,
                      api_score.activity_rate, api_score.objective_rate,
                      api_score.performance_rate, api_score.lead_credit
                 FROM observations observation
                 JOIN "KothApiScoreResults" api_score
                   ON api_score.game_id = $1
                  AND api_score.challenge_id = observation.challenge_id
                  AND api_score.ad_round_id = observation.round_id
                WHERE observation.claim_source = 'Api'
                  AND observation.result_id IS NOT NULL
                  AND observation.is_scorable
           ), api_ticks AS (
               SELECT tick.*,
                      LAG(tick.lead_credit) OVER (
                        PARTITION BY tick.participation_id,
                                     tick.challenge_id, tick.epoch
                        ORDER BY tick.number
                      ) AS previous_lead_credit
                 FROM api_ticks_raw tick
           ), api_epochs AS (
               SELECT participation_id, challenge_id, epoch,
                      AVG(activity_rate) AS activity_rate,
                      AVG(objective_rate) AS objective_rate,
                      AVG(performance_rate) AS performance_rate,
                      AVG(lead_credit) AS lead_rate,
                      CASE WHEN COUNT(*) < 2 THEN 0.0
                           ELSE COALESCE(SUM(LEAST(
                             lead_credit, previous_lead_credit
                           )) FILTER (WHERE previous_lead_credit IS NOT NULL), 0.0)
                             / (COUNT(*) - 1)::double precision
                      END AS sustained_lead_rate
                 FROM api_ticks
                GROUP BY participation_id, challenge_id, epoch
           )
           SELECT roster.participation_id, hill.challenge_id, epoch.epoch,
                  CASE WHEN hill.claim_source = 'Api'
                    THEN COUNT(*) FILTER (
                      WHERE observation.result_id IS NOT NULL
                        AND observation.is_scorable
                        AND api_score.activity_rate > 0
                    )
                    ELSE COUNT(DISTINCT (
                      acquisition.cycle_id, acquisition.token_window_attempt
                    )) FILTER (
                      WHERE acquisition.cycle_id IS NOT NULL
                    )
                  END::bigint AS acquisition_windows,
                  CASE WHEN hill.claim_source = 'Api'
                    THEN COUNT(*) FILTER (
                      WHERE observation.result_id IS NOT NULL
                        AND observation.is_scorable
                        AND api_score.objective_rate > 0
                    )
                    ELSE COUNT(*) FILTER (
                      WHERE observation.result_id IS NOT NULL
                        AND observation.is_scorable
                        AND observation.controlling_participation_id = roster.participation_id
                    )
                  END::bigint AS controlled_ticks,
                  CASE WHEN hill.claim_source = 'Api'
                    THEN COUNT(api_score.participation_id) FILTER (
                      WHERE observation.result_id IS NOT NULL
                        AND observation.is_scorable
                    )
                    ELSE COUNT(*) FILTER (
                      WHERE observation.result_id IS NOT NULL
                        AND observation.is_scorable
                        AND observation.responsible_participation_id = roster.participation_id
                    )
                  END::bigint AS responsible_ticks,
                  CASE WHEN hill.claim_source = 'Api'
                    THEN COUNT(*) FILTER (
                      WHERE observation.result_id IS NOT NULL
                        AND observation.is_scorable
                        AND api_score.lead_credit > 0.0
                    )
                    ELSE COUNT(*) FILTER (
                      WHERE observation.result_id IS NOT NULL
                        AND observation.is_scorable
                        AND observation.responsible_participation_id = roster.participation_id
                        AND observation.status = $9
                    )
                  END::bigint AS healthy_responsible_ticks,
                  COUNT(*) FILTER (
                    WHERE observation.result_id IS NOT NULL
                      AND observation.is_scorable
                      AND cooldown.participation_id IS NULL
                  )::bigint AS personal_scorable_ticks,
                  COUNT(DISTINCT (
                    observation.cycle_id, observation.token_window_attempt
                  )) FILTER (
                    WHERE observation.result_id IS NOT NULL
                      AND observation.is_scorable
                      AND cooldown.participation_id IS NULL
                  )::bigint AS personal_eligible_windows,
                  MAX(api_epoch.activity_rate) AS api_activity_rate,
                  MAX(api_epoch.objective_rate) AS api_objective_rate,
                  MAX(api_epoch.performance_rate) AS api_performance_rate,
                  MAX(api_epoch.lead_rate) AS api_lead_rate,
                  MAX(api_epoch.sustained_lead_rate) AS api_sustained_lead_rate
             FROM roster
             CROSS JOIN hills hill
             CROSS JOIN epoch_numbers epoch
             LEFT JOIN observations observation
               ON observation.challenge_id = hill.challenge_id
              AND observation.epoch = epoch.epoch
             LEFT JOIN "KothCycleCooldowns" cooldown
               ON cooldown.cycle_id = observation.cycle_id
              AND cooldown.participation_id = roster.participation_id
              AND observation.number BETWEEN cooldown.starts_round
                                         AND cooldown.expires_after_round
             LEFT JOIN acquisitions acquisition
               ON acquisition.game_id = $1
              AND acquisition.challenge_id = hill.challenge_id
              AND acquisition.participation_id = roster.participation_id
              AND acquisition.cycle_id = observation.cycle_id
              AND acquisition.token_window_attempt = observation.token_window_attempt
             LEFT JOIN "KothApiScoreResults" api_score
               ON api_score.game_id = $1
              AND api_score.challenge_id = hill.challenge_id
              AND api_score.ad_round_id = observation.round_id
              AND api_score.participation_id = roster.participation_id
             LEFT JOIN api_epochs api_epoch
               ON api_epoch.participation_id = roster.participation_id
              AND api_epoch.challenge_id = hill.challenge_id
              AND api_epoch.epoch = epoch.epoch
            GROUP BY roster.participation_id, hill.challenge_id,
                     hill.claim_source, epoch.epoch
            ORDER BY roster.participation_id, epoch.epoch, hill.challenge_id"#,
    )
    .bind(game_id)
    .bind(official_start_round)
    .bind(epoch_ticks)
    .bind(range_start_round)
    .bind(round_cutoff)
    .bind(checker_cutoff)
    .bind(event_ended)
    .bind(AdCheckStatus::InternalError as i16)
    .bind(AdCheckStatus::Ok as i16)
    .fetch_all(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((meta, team))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    async fn create_test_schema(connection: &mut PgConnection) {
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothOfficialConfigs" (
              game_id INTEGER, scoring_start_round INTEGER,
              epoch_ticks INTEGER, roster_snapshot JSONB, hills_snapshot JSONB
            );
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER, game_id INTEGER, number INTEGER, finalized BOOLEAN,
              start_time_utc TIMESTAMPTZ
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT, game_id INTEGER, challenge_id INTEGER,
              planned_start_round INTEGER,
              planned_end_round INTEGER, phase TEXT, old_container_id TEXT,
              replacement_container_id TEXT
            );
            CREATE TEMP TABLE "KothCycleAuditReceipts" (
              cycle_id BIGINT, receipt JSONB
            );
            CREATE TEMP TABLE "KothControlResults" (
              id INTEGER, game_id INTEGER, challenge_id INTEGER, ad_round_id INTEGER,
              cycle_id BIGINT, container_id TEXT, checked_at TIMESTAMPTZ,
              is_scorable BOOLEAN, status SMALLINT,
              controlling_participation_id INTEGER,
              responsible_participation_id INTEGER,
              token_window_attempt INTEGER NOT NULL
            );
            CREATE TEMP TABLE "KothApiScoreResults" (
              game_id INTEGER, challenge_id INTEGER, ad_round_id INTEGER,
              participation_id INTEGER, activity_rate DOUBLE PRECISION,
              objective_rate DOUBLE PRECISION, core_rate DOUBLE PRECISION,
              performance_rate DOUBLE PRECISION, lead_credit DOUBLE PRECISION
            );
            CREATE TEMP TABLE "KothCycleCooldowns" (
              cycle_id BIGINT, participation_id INTEGER,
              starts_round INTEGER, expires_after_round INTEGER
            );
            CREATE TEMP TABLE "KothTokens" (
              id INTEGER, cycle_id BIGINT, reset_attempt INTEGER
            );
            CREATE TEMP TABLE "KothAcquisitions" (
              id BIGINT, game_id INTEGER, challenge_id INTEGER,
              participation_id INTEGER, cycle_id BIGINT, token_id INTEGER,
              confirmed_at TIMESTAMPTZ
            );
            "#,
        )
        .execute(connection)
        .await
        .unwrap();
    }

    #[test]
    fn checker_status_identity_is_not_an_ambient_default() {
        assert_eq!(AdCheckStatus::InternalError as i16, 3);
        assert_eq!(AdCheckStatus::Ok as i16, 0);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn cooldown_is_personal_and_acquisition_requires_a_receipt() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        create_test_schema(&mut connection).await;

        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothOfficialConfigs" VALUES
                 (41, 1, 12, '[7,9]',
                  '[{"challengeId":5,"serviceWeight":1.0}]')"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdRounds" VALUES
                 (1,41,1,TRUE,$1),(2,41,2,TRUE,$1),(3,41,3,TRUE,$1),
                 (4,41,4,TRUE,$1),(5,41,5,TRUE,$1)"#,
        )
        .bind(now - chrono::Duration::seconds(1))
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothCrownCycles" VALUES
                 (11,41,5,1,5,'Completed','container-11','container-12')"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothControlResults" VALUES
                 (1,41,5,1,11,'container-11',$1,TRUE,0,9,9,0),
                 (2,41,5,2,11,'container-11',$1,TRUE,0,7,7,0),
                 (3,41,5,3,11,'container-11',$1,TRUE,0,7,7,1),
                 (4,41,5,4,11,'stale-container',$1,TRUE,0,7,7,1),
                 (5,41,5,5,11,NULL,$1,FALSE,3,NULL,NULL,1)"#,
        )
        .bind(now)
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "KothCycleCooldowns" VALUES (11,7,1,1)"#)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "KothTokens" VALUES (101,11,0),(102,11,1)"#)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothAcquisitions" VALUES
                 (1,41,5,7,11,101,$1),(2,41,5,7,11,102,$1)"#,
        )
        .bind(now)
        .execute(&mut connection)
        .await
        .unwrap();

        let (meta, teams) = load_evidence(&mut connection, 41, 1, 1, 12, None, None, false)
            .await
            .unwrap();
        assert_eq!(meta.len(), 1);
        assert_eq!(meta[0].scorable_ticks, 3);
        assert_eq!(meta[0].eligible_windows, 2);
        assert_eq!(meta[0].result_count, 4);
        assert_eq!(meta[0].round_count, 5);
        let cooled = teams.iter().find(|row| row.participation_id == 7).unwrap();
        assert_eq!(cooled.personal_scorable_ticks, 2);
        assert_eq!(cooled.personal_eligible_windows, 2);
        assert_eq!(cooled.controlled_ticks, 2);
        assert_eq!(cooled.acquisition_windows, 2);
        let challenger = teams.iter().find(|row| row.participation_id == 9).unwrap();
        assert_eq!(challenger.personal_scorable_ticks, 3);
        assert_eq!(challenger.acquisition_windows, 0);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn unscoped_platform_void_settles_an_epoch_without_becoming_control() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        create_test_schema(&mut connection).await;

        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothOfficialConfigs" VALUES
                 (52, 1, 3, '[7]',
                  '[{"challengeId":5,"serviceWeight":1.0}]')"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdRounds" VALUES
                 (1,52,1,TRUE,$1),(2,52,2,TRUE,$1),(3,52,3,TRUE,$1)"#,
        )
        .bind(now - chrono::Duration::seconds(2))
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothControlResults" VALUES
                 (1,52,5,1,NULL,NULL,$1,FALSE,3,NULL,NULL,0),
                 (2,52,5,2,NULL,NULL,$1,FALSE,3,NULL,NULL,0),
                 (3,52,5,3,NULL,NULL,$1,FALSE,3,NULL,NULL,0)"#,
        )
        .bind(now - chrono::Duration::seconds(1))
        .execute(&mut connection)
        .await
        .unwrap();

        let (meta, teams) = load_evidence(&mut connection, 52, 1, 1, 3, Some(now), Some(now), true)
            .await
            .unwrap();
        assert_eq!(meta[0].round_count, 3);
        assert_eq!(meta[0].result_count, 3);
        assert_eq!(meta[0].scorable_ticks, 0);
        assert_eq!(teams[0].personal_scorable_ticks, 0);
        let settled = super::super::score_evidence_rows(&meta, &teams, &[7], 3, true).unwrap();
        assert!(settled.fully_settled);
        assert!(settled.teams[&7].epochs[0].finalized);
        assert_eq!(settled.teams[&7].cells[&5].controlled_ticks, 0);

        sqlx::query(
            r#"UPDATE "KothControlResults"
                  SET is_scorable = TRUE, status = $1
                WHERE id = 3"#,
        )
        .bind(AdCheckStatus::Ok as i16)
        .execute(&mut connection)
        .await
        .unwrap();
        let (meta, teams) = load_evidence(&mut connection, 52, 1, 1, 3, Some(now), Some(now), true)
            .await
            .unwrap();
        assert_eq!(meta[0].result_count, 2);
        let unsettled = super::super::score_evidence_rows(&meta, &teams, &[7], 3, true).unwrap();
        assert!(!unsettled.fully_settled);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn leaderboard_epoch_averages_tick_performance_and_adjacent_lead_credit() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        create_test_schema(&mut connection).await;

        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothOfficialConfigs" VALUES
                 (61, 1, 2, '[7,9]',
                  '[{"challengeId":8,"serviceWeight":1.0,"claimSource":"Api"}]')"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdRounds" VALUES
                 (1,61,1,TRUE,$1),(2,61,2,TRUE,$1)"#,
        )
        .bind(now - chrono::Duration::seconds(2))
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothCrownCycles" VALUES
                 (11,61,8,1,2,'Completed',NULL,'arena-11')"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "KothControlResults" VALUES
                 (1,61,8,1,11,'arena-11',$1,TRUE,0,NULL,NULL,0),
                 (2,61,8,2,11,'arena-11',$1,TRUE,0,NULL,NULL,0)"#,
        )
        .bind(now - chrono::Duration::seconds(1))
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"INSERT INTO "KothApiScoreResults" VALUES
                 (61,8,1,7,1.0,1.0,1.0,1.0,1.0),
                 (61,8,2,7,0.4,0.4,0.4,0.4,0.0),
                 (61,8,1,9,0.5,0.5,0.5,0.5,0.0),
                 (61,8,2,9,0.5,0.5,0.5,0.5,1.0)"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let (meta, teams) = load_evidence(&mut connection, 61, 1, 1, 2, None, None, false)
            .await
            .unwrap();
        assert_eq!((meta[0].scorable_ticks, meta[0].eligible_windows), (2, 2));
        let mixed = teams.iter().find(|row| row.participation_id == 7).unwrap();
        assert_eq!(
            (
                mixed.api_activity_rate,
                mixed.api_objective_rate,
                mixed.api_performance_rate,
                mixed.api_lead_rate,
                mixed.api_sustained_lead_rate,
            ),
            (Some(0.7), Some(0.7), Some(0.7), Some(0.5), Some(0.0))
        );
        let steady = teams.iter().find(|row| row.participation_id == 9).unwrap();
        assert_eq!(steady.api_performance_rate, Some(0.5));
        assert_eq!(steady.api_sustained_lead_rate, Some(0.0));

        let scored = super::super::score_evidence_rows(&meta, &teams, &[7, 9], 2, false).unwrap();
        assert!((scored.teams[&7].projected_total - 71.3125).abs() < 1e-10);
        assert!((scored.teams[&9].projected_total - 51.5625).abs() < 1e-10);
    }
}
