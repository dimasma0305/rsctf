use chrono::{DateTime, Duration, Utc};
use sqlx::{Connection, PgConnection};

use super::{
    load_epoch_evidence, load_epoch_meta, load_latest_check_statuses, EvidenceRange,
    StableServiceRow,
};
use crate::services::ad_engine::AdCheckStatus;

const GAME_ID: i32 = 41;

fn range(cutoff: DateTime<Utc>, event_end_settlement: bool) -> EvidenceRange {
    EvidenceRange {
        official_start_round: 1,
        start_round: 1,
        end_round: None,
        epoch_ticks: 8,
        round_cutoff: Some(cutoff),
        checker_cutoff: Some(cutoff),
        attack_cutoff: Some(cutoff),
        event_end_settlement,
    }
}

fn services() -> Vec<StableServiceRow> {
    [(101, 11, 31), (102, 12, 32)]
        .into_iter()
        .map(
            |(team_service_id, participation_id, team_id)| StableServiceRow {
                team_service_id,
                participation_id,
                challenge_id: 21,
                team_id,
                team_name: format!("team-{team_id}"),
                division: None,
            },
        )
        .collect()
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn event_end_projects_late_checker_attack_and_round_boundaries() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "Teams" (id INTEGER PRIMARY KEY);
        CREATE TEMP TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "AdRounds" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, number INTEGER NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL, finalized BOOLEAN NOT NULL
        );
        CREATE TEMP TABLE "AdTeamServices" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "AdFlags" (
          id INTEGER PRIMARY KEY, round_id INTEGER NOT NULL,
          team_service_id INTEGER NOT NULL, checker_qualified BOOLEAN NOT NULL,
          service_weight DOUBLE PRECISION NOT NULL
        );
        CREATE TEMP TABLE "AdCheckResults" (
          round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
          status SMALLINT NOT NULL, message TEXT, checked_at TIMESTAMPTZ NOT NULL,
          sla_credit DOUBLE PRECISION, flag_verified BOOLEAN NOT NULL
        );
        CREATE TEMP TABLE "AdFlagDeliveryResults" (
          round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
          delivered BOOLEAN NOT NULL
        );
        CREATE TEMP TABLE "AdAttacks" (
          round_id INTEGER NOT NULL, attacker_participation_id INTEGER NOT NULL,
          victim_team_service_id INTEGER NOT NULL, flag_id INTEGER NOT NULL,
          submitted_at TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "AdEpochServiceRollups" (
          game_id INTEGER NOT NULL, participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, epoch INTEGER NOT NULL,
          closing_sla_status SMALLINT, closing_sla_credit DOUBLE PRECISION
        );
        INSERT INTO "Teams" VALUES (31), (32);
        INSERT INTO "Participations" VALUES (11, 41, 31), (12, 41, 32);
        INSERT INTO "GameChallenges" VALUES (21, 41);
        INSERT INTO "AdTeamServices" VALUES (101, 41, 11, 21), (102, 41, 12, 21);
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let end: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AdRounds" VALUES (1, 41, 1, $1, TRUE)"#)
        .bind(end - Duration::seconds(10))
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdFlags" VALUES
             (201, 1, 101, TRUE, 1.0), (202, 1, 102, TRUE, 1.0)"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdCheckResults" VALUES
             (1, 101, $1, 'genuine late', $2, 1.0, TRUE),
             (1, 102, $1, 'genuine exact-end', $3, 1.0, TRUE)"#,
    )
    .bind(AdCheckStatus::Ok as i16)
    .bind(end + Duration::milliseconds(1))
    .bind(end)
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AdAttacks" VALUES (1, 11, 102, 202, $1)"#)
        .bind(end)
        .execute(&mut connection)
        .await
        .unwrap();

    // A live freeze remains inclusive and does not synthesize a missing late
    // checker result. Exact-cutoff checks and captures retain their old view.
    let frozen = range(end, false);
    let frozen_meta = load_epoch_meta(&mut connection, GAME_ID, frozen)
        .await
        .unwrap();
    assert_eq!(frozen_meta[0].round_count, 1);
    assert!(!frozen_meta[0].all_checks_complete);
    let frozen_rows = load_epoch_evidence(&mut connection, GAME_ID, frozen)
        .await
        .unwrap();
    assert_eq!(frozen_rows[0].sla_credit_sum, 0.0);
    assert_eq!(frozen_rows[1].sla_credit_sum, 1.0);
    assert_eq!(frozen_rows[0].capture_count, 1);
    let frozen_latest = load_latest_check_statuses(
        &mut connection,
        GAME_ID,
        1,
        &services(),
        Some(end),
        Some(end),
        false,
    )
    .await
    .unwrap();
    assert_eq!(frozen_latest.len(), 1);
    assert_eq!(frozen_latest[0].participation_id, 12);
    assert_eq!(frozen_latest[0].status, AdCheckStatus::Ok as i16);

    // End settlement counts both required checker rows as complete, but the
    // strict fence turns exact/late observations into local zeroes and excludes
    // the exact-end capture.
    let ended = range(end, true);
    let ended_meta = load_epoch_meta(&mut connection, GAME_ID, ended)
        .await
        .unwrap();
    assert!(ended_meta[0].all_checks_complete);
    let ended_rows = load_epoch_evidence(&mut connection, GAME_ID, ended)
        .await
        .unwrap();
    assert!(ended_rows.iter().all(|row| {
        row.sla_tick_count == 1
            && row.sla_credit_sum == 0.0
            && row.capture_count == 0
            && row.eligible_flags_total == 0
            && row.closing_sla_status == Some(AdCheckStatus::InternalError as i16)
            && row.closing_sla_credit == Some(0.0)
    }));
    let ended_latest = load_latest_check_statuses(
        &mut connection,
        GAME_ID,
        1,
        &services(),
        Some(end),
        Some(end),
        true,
    )
    .await
    .unwrap();
    assert_eq!(ended_latest.len(), 2);
    assert!(ended_latest
        .iter()
        .all(|row| row.status == AdCheckStatus::InternalError as i16));

    // Moving the immutable deadline later exposes the untouched raw evidence.
    let extended = range(end + Duration::seconds(1), true);
    let extended_rows = load_epoch_evidence(&mut connection, GAME_ID, extended)
        .await
        .unwrap();
    assert!(extended_rows.iter().all(|row| {
        row.sla_tick_count == 1
            && row.sla_credit_sum == 1.0
            && row.closing_sla_status == Some(AdCheckStatus::Ok as i16)
            && row.closing_sla_credit == Some(1.0)
    }));
    assert_eq!(extended_rows[0].capture_count, 1);
    assert_eq!(extended_rows[0].eligible_flags_total, 2);

    // Both platform fallback identities are valid exact-boundary completions,
    // but their explicit zero cannot void the challenge tick or carry credit.
    sqlx::query(
        r#"UPDATE "AdCheckResults"
              SET status = $1,
                  message = CASE team_service_id
                    WHEN 101 THEN 'checker pass did not complete before event-close grace expired'
                    ELSE 'checker pass cancelled before completion' END,
                  checked_at = $2, sla_credit = 0.0, flag_verified = FALSE"#,
    )
    .bind(AdCheckStatus::InternalError as i16)
    .bind(end)
    .execute(&mut connection)
    .await
    .unwrap();
    let fallback_rows = load_epoch_evidence(&mut connection, GAME_ID, ended)
        .await
        .unwrap();
    assert!(fallback_rows.iter().all(|row| {
        row.sla_tick_count == 1
            && row.sla_credit_sum == 0.0
            && row.closing_sla_status == Some(AdCheckStatus::InternalError as i16)
            && row.closing_sla_credit == Some(0.0)
    }));

    // A round starting exactly at the deadline exists in the live inclusive
    // view, but never began within the ended event's [start, end) interval.
    sqlx::query(r#"INSERT INTO "AdRounds" VALUES (2, 41, 2, $1, TRUE)"#)
        .bind(end)
        .execute(&mut connection)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdFlags" VALUES
             (203, 2, 101, TRUE, 1.0), (204, 2, 102, TRUE, 1.0)"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdCheckResults" VALUES
             (2, 101, 0, 'exact-start round', $1, 1.0, TRUE),
             (2, 102, 0, 'exact-start round', $1, 1.0, TRUE)"#,
    )
    .bind(end)
    .execute(&mut connection)
    .await
    .unwrap();
    let strict_meta = load_epoch_meta(&mut connection, GAME_ID, ended)
        .await
        .unwrap();
    let inclusive_meta = load_epoch_meta(&mut connection, GAME_ID, frozen)
        .await
        .unwrap();
    assert_eq!(strict_meta[0].round_count, 1);
    assert_eq!(inclusive_meta[0].round_count, 2);
    let strict_latest = load_latest_check_statuses(
        &mut connection,
        GAME_ID,
        1,
        &services(),
        Some(end),
        Some(end),
        true,
    )
    .await
    .unwrap();
    let inclusive_latest = load_latest_check_statuses(
        &mut connection,
        GAME_ID,
        1,
        &services(),
        Some(end),
        Some(end),
        false,
    )
    .await
    .unwrap();
    assert!(strict_latest
        .iter()
        .all(|row| row.status == AdCheckStatus::InternalError as i16));
    assert!(inclusive_latest
        .iter()
        .all(|row| row.status == AdCheckStatus::Ok as i16));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn closing_sla_skips_platform_void_and_ineligible_later_history() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "Teams" (id INTEGER PRIMARY KEY);
        CREATE TEMP TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TEMP TABLE "AdRounds" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, number INTEGER NOT NULL,
          start_time_utc TIMESTAMPTZ NOT NULL, finalized BOOLEAN NOT NULL
        );
        CREATE TEMP TABLE "AdTeamServices" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "AdFlags" (
          id INTEGER PRIMARY KEY, round_id INTEGER NOT NULL,
          team_service_id INTEGER NOT NULL, checker_qualified BOOLEAN NOT NULL,
          service_weight DOUBLE PRECISION NOT NULL
        );
        CREATE TEMP TABLE "AdCheckResults" (
          round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
          status SMALLINT NOT NULL, message TEXT, checked_at TIMESTAMPTZ NOT NULL,
          sla_credit DOUBLE PRECISION, flag_verified BOOLEAN NOT NULL
        );
        CREATE TEMP TABLE "AdFlagDeliveryResults" (
          round_id INTEGER NOT NULL, team_service_id INTEGER NOT NULL,
          delivered BOOLEAN NOT NULL
        );
        CREATE TEMP TABLE "AdAttacks" (
          round_id INTEGER NOT NULL, attacker_participation_id INTEGER NOT NULL,
          victim_team_service_id INTEGER NOT NULL, flag_id INTEGER NOT NULL,
          submitted_at TIMESTAMPTZ NOT NULL
        );
        CREATE TEMP TABLE "AdEpochServiceRollups" (
          game_id INTEGER NOT NULL, participation_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL, epoch INTEGER NOT NULL,
          closing_sla_status SMALLINT, closing_sla_credit DOUBLE PRECISION
        );
        INSERT INTO "Teams" VALUES (31), (32);
        INSERT INTO "Participations" VALUES (11, 41, 31), (12, 41, 32);
        INSERT INTO "GameChallenges" VALUES (21, 41);
        INSERT INTO "AdTeamServices" VALUES (101, 41, 11, 21), (102, 41, 12, 21);
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let now = Utc::now();
    sqlx::query(
        r#"INSERT INTO "AdRounds" VALUES
             (1, 41, 1, $1, TRUE), (2, 41, 2, $2, TRUE),
             (3, 41, 3, $3, TRUE)"#,
    )
    .bind(now - Duration::seconds(20))
    .bind(now - Duration::seconds(10))
    .bind(now - Duration::seconds(1))
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdFlags" VALUES
             (201, 1, 101, TRUE, 1.0), (202, 1, 102, TRUE, 1.0),
             (203, 2, 101, TRUE, 1.0), (204, 2, 102, TRUE, 1.0),
             (205, 3, 101, TRUE, 1.0), (206, 3, 102, TRUE, 1.0)"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdCheckResults" VALUES
             (1, 101, 0, NULL, $1, 1.0, TRUE),
             (1, 102, 0, NULL, $1, 1.0, TRUE),
             (2, 101, 3, 'flag delivery failed', $2, 0.0, FALSE),
             (2, 102, 0, NULL, $2, 1.0, TRUE),
             (3, 101, 0, 'pending placeholder', $3, NULL, FALSE),
             (3, 102, 3, 'later infrastructure failure', $3, 0.0, FALSE)"#,
    )
    .bind(now - Duration::seconds(15))
    .bind(now - Duration::seconds(5))
    .bind(now - Duration::milliseconds(500))
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AdFlagDeliveryResults" VALUES
             (1, 101, TRUE), (1, 102, TRUE),
             (2, 101, FALSE), (2, 102, TRUE),
             (3, 101, TRUE), (3, 102, TRUE)"#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let rows = load_epoch_evidence(
        &mut connection,
        GAME_ID,
        EvidenceRange {
            official_start_round: 1,
            start_round: 1,
            end_round: None,
            epoch_ticks: 8,
            round_cutoff: Some(now),
            checker_cutoff: Some(now),
            attack_cutoff: Some(now),
            event_end_settlement: false,
        },
    )
    .await
    .unwrap();
    let failed_service = rows.iter().find(|row| row.participation_id == 11).unwrap();
    let healthy_service = rows.iter().find(|row| row.participation_id == 12).unwrap();
    // The failed-delivery verdict is a personal void. The later NULL-credit
    // placeholder is absent from check history, so neither can replace the
    // last eligible closing sample from round 1.
    assert_eq!(failed_service.sla_tick_count, 2);
    assert_eq!(failed_service.sla_credit_sum, 1.0);
    assert_eq!(
        failed_service.closing_sla_status,
        Some(AdCheckStatus::Ok as i16)
    );
    assert_eq!(failed_service.closing_sla_credit, Some(1.0));
    // A later infrastructure verdict carries credit in the SLA timeline but
    // has no non-infrastructure credit of its own. Closing state therefore
    // remains the latest eligible round-2 verdict.
    assert_eq!(healthy_service.sla_tick_count, 3);
    assert_eq!(healthy_service.sla_credit_sum, 3.0);
    assert_eq!(
        healthy_service.closing_sla_status,
        Some(AdCheckStatus::Ok as i16)
    );
    assert_eq!(healthy_service.closing_sla_credit, Some(1.0));
    assert_eq!(failed_service.eligible_flags_total, 3);
}
