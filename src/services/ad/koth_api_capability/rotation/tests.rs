use sqlx::{Connection, PgConnection};

use super::repair_missing_eligible_event_capabilities;
use crate::utils::enums::{ParticipationStatus, Role};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn missing_capability_repair_is_eligible_fenced_and_idempotent() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "KothOfficialConfigs" (
          game_id INTEGER PRIMARY KEY,
          roster_snapshot JSONB NOT NULL,
          hills_snapshot JSONB NOT NULL
        );
        CREATE TEMP TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          captain_id INTEGER NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TEMP TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id INTEGER NOT NULL
        );
        CREATE TEMP TABLE "AspNetUsers" (
          id INTEGER PRIMARY KEY,
          role SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "KothCrownCycles" (
          game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          activated_at TIMESTAMPTZ
        );
        CREATE TEMP TABLE "KothApiTeamTokens" (
          game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          token TEXT NOT NULL UNIQUE,
          generation INTEGER NOT NULL DEFAULT 1,
          rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
          last_used_at TIMESTAMPTZ,
          revocation_pending BOOLEAN NOT NULL DEFAULT FALSE,
          PRIMARY KEY (game_id, challenge_id, participation_id)
        );
        CREATE TEMP TABLE "KothApiSnapshots" (
          target_id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          snapshot_hash BYTEA NOT NULL
        );

        INSERT INTO "KothOfficialConfigs" VALUES
          (7, '[11,12,13,14]', '[{"challengeId":9,"claimSource":"Api"}]');
        INSERT INTO "Teams" (id, captain_id) VALUES
          (21, 101), (22, 102), (23, 103), (24, 104);
        INSERT INTO "KothCrownCycles" VALUES (7, 9, clock_timestamp());
        INSERT INTO "KothApiSnapshots" VALUES
          (31, 7, 9, decode(repeat('11', 32), 'hex'));
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, game_id, team_id, status) VALUES
             (11, 7, 21, $1),
             (12, 7, 22, $2),
             (13, 7, 23, $1),
             (14, 7, 24, $1)"#,
    )
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ParticipationStatus::Suspended as i16)
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, role) VALUES
             (101, $1), (102, $1), (103, $2)"#,
    )
    .bind(Role::User as i16)
    .bind(Role::Banned as i16)
    .execute(&mut connection)
    .await
    .unwrap();

    let before: Vec<u8> =
        sqlx::query_scalar(r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 31"#)
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let repaired = repair_missing_eligible_event_capabilities(&mut connection, 7, 9)
        .await
        .unwrap();
    assert_eq!(repaired.len(), 1);
    assert_eq!(repaired[0].challenge_id, 9);
    assert_eq!(repaired[0].participation_id, 11);

    let capabilities: Vec<(i32, String, i32, bool)> = sqlx::query_as(
        r#"SELECT participation_id, token, generation, revocation_pending
             FROM "KothApiTeamTokens"
            ORDER BY participation_id"#,
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].0, 11);
    assert!(capabilities[0].1.starts_with("koth_"));
    assert_eq!(capabilities[0].2, 1);
    assert!(!capabilities[0].3);

    let after: Vec<u8> =
        sqlx::query_scalar(r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 31"#)
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_ne!(after, before);

    assert!(
        repair_missing_eligible_event_capabilities(&mut connection, 7, 9)
            .await
            .unwrap()
            .is_empty()
    );
    let after_retry: Vec<u8> =
        sqlx::query_scalar(r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 31"#)
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(after_retry, after);
}
