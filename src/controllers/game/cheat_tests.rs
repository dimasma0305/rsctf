use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

use super::*;

const USER_1: &str = "00000000-0000-0000-0000-000000000001";
const USER_2: &str = "00000000-0000-0000-0000-000000000002";
const USER_3: &str = "00000000-0000-0000-0000-000000000003";
const USER_4: &str = "00000000-0000-0000-0000-000000000004";

struct CheatReportFixture {
    database_url: String,
    admin_pool: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl CheatReportFixture {
    async fn create() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("rsctf_cheat_report_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin_pool)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .expect("connect isolated pool");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL, practice_mode BOOLEAN NOT NULL
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY, name TEXT NOT NULL, avatar_hash TEXT NULL
            );
            CREATE TABLE "Divisions" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, name TEXT NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL, division_id INTEGER NULL
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, title TEXT NOT NULL
            );
            CREATE TABLE "AspNetUsers" (
              id UUID PRIMARY KEY, user_name TEXT NULL
            );
            CREATE TABLE "Submissions" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, user_id UUID NULL, answer TEXT NOT NULL,
              status SMALLINT NOT NULL, submit_time_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "FirstSolves" (
              participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              submission_id INTEGER NOT NULL
            );
            CREATE TABLE "CheatInfo" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              submission_id INTEGER NOT NULL, submit_participation_id INTEGER NOT NULL,
              source_participation_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              evidence_key TEXT NOT NULL, observed_at_utc TIMESTAMPTZ NOT NULL,
              evidence_payload JSONB NOT NULL, evidence_version SMALLINT NOT NULL
            );
            CREATE TABLE "SuspicionEvents" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, challenge_id INTEGER NULL,
              kind SMALLINT NOT NULL, evidence_key TEXT NOT NULL,
              score_delta INTEGER NOT NULL, created_at TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL, game_id INTEGER NOT NULL, team_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL,
              PRIMARY KEY (user_id, game_id)
            );
            CREATE TABLE "IdentityObservations" (
              id BIGSERIAL PRIMARY KEY, user_id UUID NOT NULL, team_id INTEGER NULL,
              game_id INTEGER NULL, participation_id INTEGER NULL, kind TEXT NOT NULL,
              value_hash BYTEA NOT NULL, value_hint TEXT NOT NULL,
              observed_at_utc TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "AntiCheatExemptions" (
              user_a UUID NOT NULL, user_b UUID NOT NULL, kind TEXT NOT NULL,
              value_hash BYTEA NOT NULL, created_at_utc TIMESTAMPTZ NOT NULL,
              expires_at_utc TIMESTAMPTZ NOT NULL, revoked_at_utc TIMESTAMPTZ NULL
            );
            CREATE TABLE "SuspicionReconciliationState" (
              game_id INTEGER PRIMARY KEY,
              evidence_closed_at_utc TIMESTAMPTZ NULL,
              last_reconciled_at_utc TIMESTAMPTZ NULL,
              sealed_at_utc TIMESTAMPTZ NULL,
              attempts INTEGER NOT NULL DEFAULT 0,
              last_error TEXT NULL
            );
            CREATE TABLE "SuspicionEvaluationOutbox" (
              id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
              observed_at_utc TIMESTAMPTZ NOT NULL,
              completed_at_utc TIMESTAMPTZ NULL, last_error TEXT NULL
            );

            INSERT INTO "Games" VALUES
              (1, '2026-01-01T00:00:00Z', '2026-12-31T23:59:59Z', FALSE),
              (2, '2026-01-01T00:00:00Z', '2026-02-01T00:00:00Z', TRUE);
            INSERT INTO "Teams" VALUES
              (101, 'Owner current', NULL), (102, 'Submit current', NULL),
              (103, 'Other game A', NULL), (104, 'Other game B', NULL);
            INSERT INTO "Participations" VALUES
              (201, 1, 101, 1, NULL), (202, 1, 102, 1, NULL),
              (203, 2, 103, 1, NULL), (204, 2, 104, 1, NULL);
            INSERT INTO "GameChallenges" VALUES
              (11, 1, 'Current challenge'), (12, 2, 'Other challenge');
            INSERT INTO "AspNetUsers" VALUES
              ('00000000-0000-0000-0000-000000000001', 'current-user-1'),
              ('00000000-0000-0000-0000-000000000002', 'current-user-2'),
              ('00000000-0000-0000-0000-000000000003', 'current-user-3'),
              ('00000000-0000-0000-0000-000000000004', 'current-user-4');
            INSERT INTO "UserParticipations" VALUES
              ('00000000-0000-0000-0000-000000000001', 1, 101, 201),
              ('00000000-0000-0000-0000-000000000002', 1, 102, 202),
              ('00000000-0000-0000-0000-000000000003', 2, 103, 203),
              ('00000000-0000-0000-0000-000000000004', 2, 104, 204);
            "#,
        )
        .execute(&pool)
        .await
        .expect("create cheat-report fixture");
        Self {
            database_url,
            admin_pool,
            pool,
            schema,
        }
    }

    async fn seed_cheat_rows(&self) {
        sqlx::raw_sql(
            r#"
            INSERT INTO "Submissions" VALUES
              (301, 1, 202, 11, 102,
               '00000000-0000-0000-0000-000000000002', 'flag-one', 3,
               '2026-06-01T12:00:00Z'),
              (302, 1, 202, 11, 102,
               '00000000-0000-0000-0000-000000000002', 'flag-two', 3,
               '2026-06-01T12:01:00Z'),
              (309, 1, 202, 11, 102,
               '00000000-0000-0000-0000-000000000002', 'correct', 1,
               '2026-06-01T12:00:00Z'),
              (310, 1, 202, 11, 102,
               '00000000-0000-0000-0000-000000000002', 'correct replay', 1,
               '2026-06-01T12:01:00Z');
            INSERT INTO "FirstSolves" VALUES (202, 11, 309);
            INSERT INTO "CheatInfo" VALUES
              (401, 1, 301, 202, 201, 11, 'submission:301',
               '2026-06-01T12:00:00Z',
               '{"sourceTeamName":"Owner at detection","submitTeamName":"Submitter at detection","submitUserName":"user-at-detection","challengeTitle":"Challenge at detection"}', 1),
              (402, 1, 302, 202, 201, 11, 'submission:302',
               '2026-06-01T12:01:00Z',
               '{"sourceTeamName":"Owner second","submitTeamName":"Submitter second","submitUserName":"second-user","challengeTitle":"Second challenge snapshot"}', 1);
            UPDATE "Teams" SET name = 'Owner after rotation' WHERE id = 101;
            UPDATE "Teams" SET name = 'Submit after rotation' WHERE id = 102;
            UPDATE "GameChallenges" SET title = 'Challenge after rotation' WHERE id = 11;
            UPDATE "AspNetUsers" SET user_name = 'user-after-rotation'
             WHERE id = '00000000-0000-0000-0000-000000000002';
            "#,
        )
        .execute(&self.pool)
        .await
        .expect("seed immutable cheat evidence");
    }

    async fn read_only_pool(&self) -> sqlx::PgPool {
        let options = PgConnectOptions::from_str(&self.database_url)
            .expect("parse database URL")
            .options([
                ("search_path", self.schema.as_str()),
                ("default_transaction_read_only", "on"),
            ]);
        PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("connect read-only fixture pool")
    }

    async fn cleanup(self) {
        self.pool.close().await;
        assert!(self.schema.starts_with("rsctf_cheat_report_"));
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin_pool)
            .await
            .expect("drop isolated schema");
        self.admin_pool.close().await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn report_survives_rotation_and_paginates_by_stable_incident_id() {
    let fixture = CheatReportFixture::create().await;
    fixture.seed_cheat_rows().await;

    let rows = load_cheat_incident_rows(&fixture.pool, Some(1), None, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].answer, "flag-two");
    assert_eq!(rows[0].source_team_name, "Owner second");
    assert_eq!(rows[1].source_team_name, "Owner at detection");
    assert_eq!(rows[1].submit_team_name, "Submitter at detection");
    assert_eq!(rows[1].user_name.as_deref(), Some("user-at-detection"));
    assert_eq!(rows[1].challenge_title, "Challenge at detection");

    let page_two = load_cheat_incident_rows(&fixture.pool, Some(1), Some(1), 1)
        .await
        .unwrap();
    assert_eq!(page_two.len(), 1);
    assert_eq!(page_two[0].answer, "flag-one");

    let canonical = canonical_solves(&fixture.pool, 1, &[]).await.unwrap();
    assert_eq!(canonical.len(), 1, "accepted replay must not inflate RSI");
    assert_eq!(
        canonical[0].submit_time_utc.to_rfc3339(),
        "2026-06-01T12:00:00+00:00"
    );

    sqlx::raw_sql(
        r#"
        INSERT INTO "GameChallenges" VALUES
          (13, 2, 'Competitive solve'),
          (14, 2, 'Exact-end practice solve'),
          (15, 2, 'Post-end practice solve');
        INSERT INTO "Submissions" VALUES
          (303, 2, 203, 13, 103,
           '00000000-0000-0000-0000-000000000003', 'competitive', 1,
           '2026-01-31T23:59:59Z'),
          (304, 2, 203, 14, 103,
           '00000000-0000-0000-0000-000000000003', 'exact-end', 1,
           '2026-02-01T00:00:00Z'),
          (305, 2, 204, 15, 104,
           '00000000-0000-0000-0000-000000000004', 'post-end', 1,
           '2026-02-01T00:00:01Z');
        INSERT INTO "FirstSolves" VALUES
          (203, 13, 303), (203, 14, 304), (204, 15, 305);
        "#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let practice = canonical_solves(&fixture.pool, 2, &[]).await.unwrap();
    assert_eq!(
        practice.len(),
        1,
        "practice solves at/after end stay out of reports"
    );
    assert_eq!(practice[0].challenge_id, 13);
    let compared = canonical_solves(&fixture.pool, 2, &[203, 204])
        .await
        .unwrap();
    assert_eq!(
        compared.len(),
        1,
        "compare uses the same strict game window"
    );
    sqlx::raw_sql(
        r#"
        INSERT INTO "Submissions" VALUES
          (306, 2, 204, 12, 104,
           '00000000-0000-0000-0000-000000000004', 'stolen-at-start', 3,
           '2026-01-01T00:00:00Z'),
          (307, 2, 204, 12, 104,
           '00000000-0000-0000-0000-000000000004', 'stolen-at-end', 3,
           '2026-02-01T00:00:00Z'),
          (308, 2, 204, 12, 104,
           '00000000-0000-0000-0000-000000000004', 'stolen-after-end', 3,
           '2026-02-01T00:00:01Z');
        INSERT INTO "CheatInfo" VALUES
          (406, 2, 306, 204, 203, 12, 'submission:306',
           '2026-01-01T00:00:00Z', '{}', 1),
          (407, 2, 307, 204, 203, 12, 'submission:307',
           '2026-02-01T00:00:00Z', '{}', 1),
          (408, 2, 308, 204, 203, 12, 'submission:308',
           '2026-02-01T00:00:01Z', '{}', 1);
        "#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let practice_cheats = load_cheat_incident_rows(&fixture.pool, Some(2), None, 0)
        .await
        .unwrap();
    assert_eq!(practice_cheats.len(), 1);
    assert_eq!(practice_cheats[0].answer, "stolen-at-start");
    let global_cheats = load_cheat_incident_rows(&fixture.pool, None, Some(100), 0)
        .await
        .unwrap();
    assert_eq!(
        global_cheats.len(),
        3,
        "admin feed uses the same strict window"
    );
    sqlx::query(
        r#"INSERT INTO "SuspicionEvents"
             VALUES (2, 2, 204, 15, 12, 'challenge:15', 50,
                     '2026-01-31T23:59:59Z')"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (_, abnormal) = build_suspicion_sections(&fixture.pool, 2).await.unwrap();
    assert!(
        abnormal.is_empty(),
        "a post-end practice solve cannot become an abnormal competitive solve"
    );
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn ordinary_suspicion_events_are_not_stolen_flag_reports() {
    let fixture = CheatReportFixture::create().await;
    sqlx::raw_sql(
        r#"INSERT INTO "SuspicionEvents" VALUES
             (1, 1, 202, 11, 9, 'burst:202', 30, '2026-06-01T13:00:00Z'),
             (2, 1, 202, 11, 24, 'legacy-untrusted:2', 0,
              '2026-06-01T13:01:00Z')"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();

    assert!(load_cheat_incident_rows(&fixture.pool, None, Some(100), 0)
        .await
        .unwrap()
        .is_empty());
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn identity_overlap_excludes_observations_from_other_games() {
    let fixture = CheatReportFixture::create().await;
    let hash = vec![0xabu8; 32];
    for (user, game, team, participation) in [
        (USER_1, 1, 101, 201),
        (USER_3, 2, 103, 203),
        (USER_4, 2, 104, 204),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind, value_hash,
                  value_hint, observed_at_utc)
               VALUES ($1, $2, $3, $4, 'Ip', $5, '198.51.100.42',
                       '2026-06-01T12:00:00Z')"#,
        )
        .bind(Uuid::parse_str(user).unwrap())
        .bind(team)
        .bind(game)
        .bind(participation)
        .bind(&hash)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }

    // A non-practice game's end is exclusive. Even two matching observations
    // at exactly `end_time_utc` must not appear in that game's report.
    let boundary_hash = vec![0xcdu8; 32];
    for (user, team, participation) in [(USER_1, 101, 201), (USER_2, 102, 202)] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind, value_hash,
                  value_hint, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Ip', $4, '203.0.113.8',
                       '2026-12-31T23:59:59Z')"#,
        )
        .bind(Uuid::parse_str(user).unwrap())
        .bind(team)
        .bind(participation)
        .bind(&boundary_hash)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }

    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert!(ip_rows.is_empty());
    assert!(overlaps.is_empty());

    // Post-end practice observations are a negative control: reports retain the
    // configured competition window rather than expanding as practice continues.
    let (practice_ip_rows, practice_overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 2)
            .await
            .unwrap();
    assert!(practice_ip_rows.is_empty());
    assert!(practice_overlaps.is_empty());

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind, value_hash,
              value_hint, observed_at_utc)
           VALUES ($1, 102, 1, 202, 'Ip', $2, '198.51.100.42',
                   '2026-06-01T12:01:00Z')"#,
    )
    .bind(Uuid::parse_str(USER_2).unwrap())
    .bind(&hash)
    .execute(&fixture.pool)
    .await
    .unwrap();

    // The observation is an immutable membership snapshot. Leaving after the
    // login must not erase the historical relationship from the report.
    sqlx::query(
        r#"DELETE FROM "UserParticipations"
            WHERE user_id = $1 AND game_id = 1"#,
    )
    .bind(Uuid::parse_str(USER_2).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(ip_rows.len(), 2);
    assert_eq!(overlaps.len(), 1);
    assert!(ip_rows.iter().all(|row| row["type"] == "CrossTeamIP"));
    assert_ne!(overlaps[0]["value"], "198.51.100.42");
    assert_eq!(overlaps[0]["userNames"], serde_json::json!([]));

    // Large exact-IP groups are suppressed as likely shared networks, but a
    // browser fingerprint shared across any number of teams remains precise
    // identity evidence and must stay visible.
    sqlx::raw_sql(
        r#"
        INSERT INTO "Teams" VALUES
          (111, 'Fingerprint 1', NULL), (112, 'Fingerprint 2', NULL),
          (113, 'Fingerprint 3', NULL), (114, 'Fingerprint 4', NULL),
          (115, 'Fingerprint 5', NULL);
        INSERT INTO "Participations" VALUES
          (211, 1, 111, 1, NULL), (212, 1, 112, 1, NULL),
          (213, 1, 113, 1, NULL), (214, 1, 114, 1, NULL),
          (215, 1, 115, 1, NULL);
        INSERT INTO "IdentityObservations"
          (user_id, team_id, game_id, participation_id, kind, value_hash,
           value_hint, observed_at_utc)
        VALUES
          ('00000000-0000-0000-0000-000000000011', 111, 1, 211, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:00:00Z'),
          ('00000000-0000-0000-0000-000000000012', 112, 1, 212, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:01:00Z'),
          ('00000000-0000-0000-0000-000000000013', 113, 1, 213, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:02:00Z'),
          ('00000000-0000-0000-0000-000000000014', 114, 1, 214, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:03:00Z'),
          ('00000000-0000-0000-0000-000000000015', 115, 1, 215, 'Fingerprint',
           decode(repeat('ef', 32), 'hex'), 'abcdef012345', '2026-06-01T12:04:00Z');
        INSERT INTO "IdentityObservations"
          (user_id, team_id, game_id, participation_id, kind, value_hash,
           value_hint, observed_at_utc)
        SELECT user_id, team_id, game_id, participation_id, 'Ip',
               decode(repeat('ad', 32), 'hex'), '203.0.113.x', observed_at_utc
          FROM "IdentityObservations"
         WHERE kind = 'Fingerprint';
        "#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (identity_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(
        identity_rows
            .iter()
            .filter(|row| row["type"] == "SharedFingerprint")
            .count(),
        5
    );
    assert!(overlaps
        .iter()
        .any(|row| { row["kind"] == "fingerprint" && row["teamCount"] == 5 }));
    assert!(!overlaps
        .iter()
        .any(|row| { row["kind"] == "ip" && row["teamCount"] == 5 }));
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn identity_report_applies_exemptions_to_temporal_pairs_not_whole_hashes() {
    let fixture = CheatReportFixture::create().await;
    let user_a = Uuid::parse_str(USER_1).unwrap();
    let user_b = Uuid::parse_str(USER_2).unwrap();
    let user_c = Uuid::parse_str(USER_3).unwrap();
    let fingerprint_hash = vec![0x61_u8; 32];

    sqlx::raw_sql(
        r#"
        INSERT INTO "Teams" VALUES (105, 'Unexempt third', NULL);
        INSERT INTO "Participations" VALUES (205, 1, 105, 1, NULL);
        "#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    for (user_id, team_id, participation_id, observed_at) in [
        (user_a, 101, 201, "2026-06-01T12:01:00Z"),
        (user_b, 102, 202, "2026-06-01T12:02:00Z"),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind,
                  value_hash, value_hint, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Fingerprint', $4,
                       'abcdef012345', $5::timestamptz)"#,
        )
        .bind(user_id)
        .bind(team_id)
        .bind(participation_id)
        .bind(&fingerprint_hash)
        .bind(observed_at)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a, user_b, kind, value_hash, created_at_utc, expires_at_utc)
           VALUES ($1, $2, 'Fingerprint', $3,
                   '2026-06-01T12:00:00Z', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&fingerprint_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert!(ip_rows.is_empty());
    assert!(overlaps.is_empty());

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind,
              value_hash, value_hint, observed_at_utc)
           VALUES ($1, 105, 1, 205, 'Fingerprint', $2,
                   'abcdef012345', '2026-06-01T12:03:00Z')"#,
    )
    .bind(user_c)
    .bind(&fingerprint_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(ip_rows.len(), 3, "the unexempt A-C and B-C edges remain");
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0]["teamCount"], 3);
    let team_a_row = ip_rows.iter().find(|row| row["teamId"] == 101).unwrap();
    let team_b_row = ip_rows.iter().find(|row| row["teamId"] == 102).unwrap();
    let team_c_row = ip_rows.iter().find(|row| row["teamId"] == 105).unwrap();
    assert_eq!(
        team_a_row["relatedTeams"],
        serde_json::json!(["Unexempt third"]),
        "the exempt A-B edge must not leak back through the retained group"
    );
    assert_eq!(
        team_b_row["relatedTeams"],
        serde_json::json!(["Unexempt third"])
    );
    assert_eq!(
        team_c_row["relatedTeams"],
        serde_json::json!(["Owner current", "Submit current"])
    );

    sqlx::query(r#"DELETE FROM "IdentityObservations""#)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "AntiCheatExemptions""#)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let ip_hash = vec![0x62_u8; 32];
    for (user_id, team_id, participation_id, observed_at) in [
        (user_a, 101, 201, "2026-06-01T12:01:00Z"),
        (user_b, 102, 202, "2026-06-01T12:02:00Z"),
    ] {
        sqlx::query(
            r#"INSERT INTO "IdentityObservations"
                 (user_id, team_id, game_id, participation_id, kind,
                  value_hash, value_hint, observed_at_utc)
               VALUES ($1, $2, 1, $3, 'Ip', $4,
                       '198.51.100.x', $5::timestamptz)"#,
        )
        .bind(user_id)
        .bind(team_id)
        .bind(participation_id)
        .bind(&ip_hash)
        .bind(observed_at)
        .execute(&fixture.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        r#"INSERT INTO "AntiCheatExemptions"
             (user_a, user_b, kind, value_hash, created_at_utc, expires_at_utc)
           VALUES ($1, $2, 'Ip', $3,
                   '2026-06-01T12:00:00Z', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_a)
    .bind(user_b)
    .bind(&ip_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert!(
        ip_rows.is_empty(),
        "later reconciliation cannot revive the edge"
    );
    assert!(overlaps.is_empty());

    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, team_id, game_id, participation_id, kind,
              value_hash, value_hint, observed_at_utc)
           VALUES ($1, 102, 1, 202, 'Ip', $2,
                   '198.51.100.x', '2026-06-01T13:00:00Z')"#,
    )
    .bind(user_b)
    .bind(&ip_hash)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let (ip_rows, overlaps) =
        super::super::cheat_identity::build_identity_analysis(&fixture.pool, 1)
            .await
            .unwrap();
    assert_eq!(ip_rows.len(), 2);
    assert_eq!(overlaps.len(), 1);
    assert_eq!(
        ip_rows[0]["time"],
        "2026-06-01T13:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap()
            .timestamp_millis()
    );

    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn report_queries_succeed_on_a_database_enforced_read_only_connection() {
    let fixture = CheatReportFixture::create().await;
    fixture.seed_cheat_rows().await;
    sqlx::query(
        r#"INSERT INTO "SuspicionEvents"
             VALUES (1, 1, 202, 11, 9, 'burst:202', 30, '2026-06-01T13:00:00Z')"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "SuspicionReconciliationState"
          (game_id, evidence_closed_at_utc, last_reconciled_at_utc,
           sealed_at_utc, attempts, last_error)
        VALUES
          (1, '2026-12-31T23:59:59Z', '2026-06-01T13:05:00Z', NULL, 3,
           NULL);
        INSERT INTO "SuspicionEvaluationOutbox" VALUES
          (1, 1, '2026-06-01T13:01:00Z', NULL, 'job retry'),
          (2, 1, '2026-06-01T13:02:00Z', '2026-06-01T13:03:00Z', NULL),
          (3, 1, '2026-12-31T23:59:59Z', NULL, 'out of window');
        "#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let pool = fixture.read_only_pool().await;
    assert_eq!(
        load_cheat_incident_rows(&pool, Some(1), None, 0)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(canonical_solves(&pool, 1, &[]).await.unwrap().len(), 1);
    let _ = super::super::cheat_identity::build_identity_analysis(&pool, 1)
        .await
        .unwrap();
    let (suspicion, abnormal) = build_suspicion_sections(&pool, 1).await.unwrap();
    assert_eq!(suspicion.len(), 1);
    assert_eq!(suspicion[0]["events"].as_array().unwrap().len(), 1);
    assert!(
        abnormal.is_empty(),
        "quarantined rows stay out of projections"
    );
    let state = load_reconciliation_report_state(&pool, 1).await.unwrap();
    assert_eq!(state.pending_jobs, 1);
    assert_eq!(
        state.oldest_pending_at.unwrap(),
        "2026-06-01T13:01:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(
        state.last_reconciled_at.unwrap(),
        "2026-06-01T13:05:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(state.last_error.as_deref(), Some("job retry"));
    assert!(state.sealed_at.is_none());
    pool.close().await;
    fixture.cleanup().await;
}

#[test]
fn report_routes_keep_monitor_and_admin_role_extractors_and_no_sweeps() {
    let game_source = include_str!("cheat.rs");
    let report_start = game_source.find("pub async fn cheat_report(").unwrap();
    let report_end = game_source[report_start..]
        .find("pub async fn cheat_report_compare(")
        .map(|offset| report_start + offset)
        .unwrap();
    let report_handler = &game_source[report_start..report_end];
    assert!(report_handler.contains("_user: MonitorUser"));
    assert!(!report_handler.contains("run_abnormal_solve_checks"));
    assert!(!report_handler.contains("run_statistical_checks"));
    assert!(!report_handler.contains("run_correlation_checks"));
    assert!(!report_handler.contains("run_container_access_checks"));
    assert!(!report_handler.contains("run_honeypot_chain_checks"));

    let admin_source = include_str!("../admin/anti_cheat.rs");
    assert!(admin_source.contains("_admin: AdminUser"));
    assert!(admin_source.contains("AdminUser(admin): AdminUser"));
}
