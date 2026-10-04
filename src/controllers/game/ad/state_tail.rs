//! Fresh per-team half of the player A&D State response.
//!
//! The game-global configuration stays in the five-second cache. Round metadata,
//! owned services, current flags, and checker verdicts share one SQL snapshot so
//! synchronized polls cost one pool checkout and cannot mix two round revisions.

use chrono::{DateTime, Utc};
use sqlx::{Executor, Postgres};

use crate::utils::enums::{ChallengeReviewStatus, ChallengeType, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AdStateService {
    pub(super) id: i32,
    pub(super) challenge_id: i32,
    pub(super) host: String,
    pub(super) port: i32,
    pub(super) container_id: Option<String>,
    pub(super) snapshot_available: bool,
    pub(super) last_reset_at: Option<DateTime<Utc>>,
    pub(super) current_flag: Option<String>,
    pub(super) last_check_status: Option<i16>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct AdStateTail {
    pub(super) event_started: bool,
    pub(super) event_ended: bool,
    pub(super) current_round: i32,
    pub(super) round_started_at: Option<DateTime<Utc>>,
    pub(super) round_ends_at: Option<DateTime<Utc>>,
    pub(super) flags_ready: bool,
    pub(super) flag_delivery_failures: i32,
    pub(super) scoring_paused: bool,
    pub(super) scoring_paused_at: Option<DateTime<Utc>>,
    pub(super) services: Vec<AdStateService>,
}

/// Service columns are nullable because the query emits one sentinel row when
/// there are no services. That row retains current-round metadata without a
/// fallback query; required service columns are checked during reduction.
#[derive(Debug, sqlx::FromRow)]
struct AdStateTailRow {
    event_started: Option<bool>,
    event_ended: Option<bool>,
    round_number: Option<i32>,
    round_started_at: Option<DateTime<Utc>>,
    round_ends_at: Option<DateTime<Utc>>,
    flags_ready: Option<bool>,
    flag_delivery_failures: Option<i32>,
    scoring_paused: Option<bool>,
    scoring_paused_at: Option<DateTime<Utc>>,
    service_id: Option<i32>,
    challenge_id: Option<i32>,
    host: Option<String>,
    port: Option<i32>,
    container_id: Option<String>,
    snapshot_available: Option<bool>,
    last_reset_at: Option<DateTime<Utc>>,
    current_flag: Option<String>,
    last_check_status: Option<i16>,
}

const STATE_TAIL_SQL: &str = r#"
WITH current_round AS (
    SELECT id, number, start_time_utc, end_time_utc,
           flags_published_at IS NOT NULL AS flags_ready,
           flag_delivery_failures
      FROM "AdRounds"
     WHERE game_id = $1
     ORDER BY number DESC, id DESC
     LIMIT 1
), game_state AS (
    SELECT start_time_utc <= clock_timestamp() AS event_started,
           end_time_utc <= clock_timestamp() AS event_ended,
           ad_scoring_paused, ad_scoring_paused_at
      FROM "Games"
     WHERE id = $1
), team_services AS (
    SELECT service.id, service.challenge_id, service.host, service.port,
           service.container_id, service.last_reset_at
      FROM "AdTeamServices" service
      JOIN "Participations" participation
        ON participation.id = service.participation_id
       AND participation.game_id = service.game_id
      JOIN "GameChallenges" challenge
        ON challenge.id = service.challenge_id
       AND challenge.game_id = service.game_id
     WHERE service.game_id = $1
       AND service.participation_id = $2
       AND participation.status = $3
       AND challenge.is_enabled = TRUE
       AND challenge.review_status = $4
       AND challenge."Type" = $5
)
SELECT game.event_started, game.event_ended,
       round.number AS round_number,
       round.start_time_utc AS round_started_at,
       round.end_time_utc AS round_ends_at,
       round.flags_ready, round.flag_delivery_failures,
       game.ad_scoring_paused AS scoring_paused,
       game.ad_scoring_paused_at AS scoring_paused_at,
       service.id AS service_id, service.challenge_id, service.host, service.port,
       service.container_id,
       snapshot.id IS NOT NULL AND snapshot_file.reference_count > 0
           AS snapshot_available,
       service.last_reset_at,
       flag.flag AS current_flag, latest.status AS last_check_status
  FROM (VALUES (1)) singleton(dummy)
  LEFT JOIN current_round round ON TRUE
  LEFT JOIN game_state game ON TRUE
  LEFT JOIN team_services service ON TRUE
  LEFT JOIN "AdServiceSnapshots" snapshot
    ON snapshot.team_service_id = service.id
   AND (snapshot.expires_at_utc IS NULL
        OR snapshot.expires_at_utc > clock_timestamp())
  LEFT JOIN "Files" snapshot_file ON snapshot_file.id = snapshot.local_file_id
  LEFT JOIN "AdFlags" flag
    ON flag.round_id = round.id AND flag.team_service_id = service.id
  LEFT JOIN LATERAL (
      SELECT result.status
        FROM "AdCheckResults" result
       WHERE result.team_service_id = service.id
         AND result.sla_credit IS NOT NULL
       ORDER BY result.round_id DESC
       LIMIT 1
  ) latest ON service.id IS NOT NULL
 ORDER BY service.id
"#;

pub(super) async fn load<'e, E>(
    executor: E,
    game_id: i32,
    participation_id: i32,
) -> AppResult<AdStateTail>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as::<_, AdStateTailRow>(STATE_TAIL_SQL)
        .bind(game_id)
        .bind(participation_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .fetch_all(executor)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    reduce(rows)
}

fn malformed_service(column: &str) -> AppError {
    AppError::internal(format!("A&D State service row is missing {column}"))
}

fn reduce(rows: Vec<AdStateTailRow>) -> AppResult<AdStateTail> {
    let first = rows
        .first()
        .ok_or_else(|| AppError::internal("A&D State tail query returned no sentinel row"))?;
    let mut tail = AdStateTail {
        event_started: first.event_started.unwrap_or(false),
        event_ended: first.event_ended.unwrap_or(true),
        current_round: first.round_number.unwrap_or(0),
        round_started_at: first.round_started_at,
        round_ends_at: first.round_ends_at,
        flags_ready: first.flags_ready.unwrap_or(false),
        flag_delivery_failures: first.flag_delivery_failures.unwrap_or(0),
        scoring_paused: first.scoring_paused.unwrap_or(false),
        scoring_paused_at: first.scoring_paused_at,
        services: Vec::with_capacity(rows.len()),
    };
    for row in rows {
        let Some(id) = row.service_id else { continue };
        tail.services.push(AdStateService {
            id,
            challenge_id: row
                .challenge_id
                .ok_or_else(|| malformed_service("challenge_id"))?,
            host: row.host.ok_or_else(|| malformed_service("host"))?,
            port: row.port.ok_or_else(|| malformed_service("port"))?,
            container_id: row.container_id,
            snapshot_available: row.snapshot_available.unwrap_or(false),
            last_reset_at: row.last_reset_at,
            current_flag: row.current_flag,
            last_check_status: row.last_check_status,
        });
    }
    Ok(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use sqlx::{Connection, PgConnection};

    fn sentinel(round: Option<i32>) -> AdStateTailRow {
        AdStateTailRow {
            event_started: Some(true),
            event_ended: Some(false),
            round_number: round,
            round_started_at: None,
            round_ends_at: None,
            flags_ready: round.map(|_| true),
            flag_delivery_failures: round.map(|_| 2),
            scoring_paused: Some(false),
            scoring_paused_at: None,
            service_id: None,
            challenge_id: None,
            host: None,
            port: None,
            container_id: None,
            snapshot_available: Some(false),
            last_reset_at: None,
            current_flag: None,
            last_check_status: None,
        }
    }

    #[test]
    fn sentinel_preserves_round_and_empty_state_defaults() {
        let empty = reduce(vec![sentinel(None)]).unwrap();
        assert!(empty.event_started);
        assert!(!empty.event_ended);
        assert_eq!(empty.current_round, 0);
        assert!(!empty.flags_ready);
        assert_eq!(empty.flag_delivery_failures, 0);
        assert!(empty.services.is_empty());

        let round_only = reduce(vec![sentinel(Some(7))]).unwrap();
        assert_eq!(round_only.current_round, 7);
        assert!(round_only.flags_ready);
        assert_eq!(round_only.flag_delivery_failures, 2);
        assert!(round_only.services.is_empty());
    }

    #[test]
    fn partial_service_rows_fail_instead_of_disappearing() {
        let mut row = sentinel(Some(1));
        row.service_id = Some(9);
        row.challenge_id = Some(4);
        row.port = Some(80);
        assert!(reduce(vec![row]).is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn query_preserves_fresh_tail_and_filters_services() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "AdRounds" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, number INTEGER NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL,
              flags_published_at TIMESTAMPTZ, flag_delivery_failures INTEGER NOT NULL
            );
            CREATE TEMP TABLE "Games" (
              id INTEGER PRIMARY KEY, ad_scoring_paused BOOLEAN NOT NULL,
              ad_scoring_paused_at TIMESTAMPTZ,
              start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, is_enabled BOOLEAN NOT NULL,
              review_status SMALLINT NOT NULL, "Type" SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              host TEXT NOT NULL, port INTEGER NOT NULL, container_id TEXT,
              last_reset_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "AdFlags" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL, flag TEXT NOT NULL,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "AdCheckResults" (
              round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
              status SMALLINT NOT NULL, sla_credit DOUBLE PRECISION,
              PRIMARY KEY (round_id, team_service_id)
            );
            CREATE TEMP TABLE "Files" (
              id INTEGER PRIMARY KEY, reference_count BIGINT NOT NULL
            );
            CREATE TEMP TABLE "AdServiceSnapshots" (
              id BIGINT PRIMARY KEY, team_service_id INTEGER NOT NULL,
              local_file_id INTEGER NOT NULL, expires_at_utc TIMESTAMPTZ
            );
            INSERT INTO "Games" VALUES
              (1, TRUE, now(), now() - interval '1 hour', now() + interval '1 hour'),
              (2, FALSE, NULL, now() - interval '1 hour', now() + interval '1 hour');
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let empty = load(&mut connection, 1, 7).await.unwrap();
        assert!(empty.event_started);
        assert!(!empty.event_ended);
        assert_eq!(empty.current_round, 0);
        assert!(empty.services.is_empty());

        let now = Utc::now();
        sqlx::query(
            r#"INSERT INTO "AdRounds" VALUES
               (100, 1, 6, $1, $2, $1, 0), (101, 1, 7, $2, $3, $2, 2)"#,
        )
        .bind(now - Duration::minutes(2))
        .bind(now - Duration::minutes(1))
        .bind(now + Duration::minutes(1))
        .execute(&mut connection)
        .await
        .unwrap();
        let round_only = load(&mut connection, 1, 7).await.unwrap();
        assert_eq!(round_only.current_round, 7);
        assert_eq!(round_only.flag_delivery_failures, 2);
        assert!(round_only.services.is_empty());

        sqlx::query(r#"INSERT INTO "Participations" VALUES (7, 1, $1), (8, 2, $1)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges" VALUES
               (10, 1, TRUE, $1, $2), (11, 1, FALSE, $1, $2),
               (12, 1, TRUE, $1, $3), (20, 2, TRUE, $1, $2)"#,
        )
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeType::AttackDefense as i16)
        .bind(ChallengeType::KingOfTheHill as i16)
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdTeamServices" VALUES
               (21, 1, 7, 10, '10.0.0.21', 80, 'container-21', $1),
               (22, 1, 7, 10, '10.0.0.22', 81, NULL, NULL),
               (23, 1, 7, 11, '10.0.0.23', 82, NULL, NULL),
               (24, 1, 7, 12, '10.0.0.24', 83, NULL, NULL),
               (31, 2, 8, 20, '10.0.0.31', 80, NULL, NULL)"#,
        )
        .bind(now)
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdFlags" VALUES
               (100, 21, 'old-flag'), (101, 21, 'current-flag'),
               (101, 23, 'disabled-service-flag')"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AdCheckResults" VALUES
               (100, 21, 0, 1.0), (101, 21, 2, NULL),
               (101, 22, 1, 0.5), (999, 31, 1, 1.0)"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"INSERT INTO "Files" VALUES (501, 1);
               INSERT INTO "AdServiceSnapshots"
                    VALUES (601, 21, 501, now() + interval '1 day')"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let tail = load(&mut connection, 1, 7).await.unwrap();
        assert_eq!(tail.current_round, 7);
        assert!(tail.scoring_paused);
        assert!(tail.scoring_paused_at.is_some());
        assert_eq!(tail.services.len(), 2);
        assert!(tail.services[0].snapshot_available);
        assert!(!tail.services[1].snapshot_available);
        assert_eq!(
            tail.services[0].current_flag.as_deref(),
            Some("current-flag")
        );
        assert_eq!(tail.services[0].last_check_status, Some(0));
        assert_eq!(tail.services[1].current_flag, None);
        assert_eq!(tail.services[1].last_check_status, Some(1));

        let no_round = load(&mut connection, 2, 8).await.unwrap();
        assert_eq!(no_round.current_round, 0);
        assert_eq!(no_round.services.len(), 1);
        assert_eq!(no_round.services[0].last_check_status, Some(1));
        assert_eq!(no_round.services[0].current_flag, None);
    }
}
