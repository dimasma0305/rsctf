use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{delete_ad_game_data, fence_game_for_deletion};

struct DeletionFenceHarness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl DeletionFenceHarness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_game_delete_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              practice_mode BOOLEAN NOT NULL,
              hidden BOOLEAN NOT NULL DEFAULT FALSE,
              start_time_utc TIMESTAMPTZ NOT NULL,
              end_time_utc TIMESTAMPTZ NOT NULL,
              freeze_time_utc TIMESTAMPTZ,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              ad_scoring_start_round INTEGER,
              koth_scoring_start_round INTEGER,
              CONSTRAINT ck_games_event_window CHECK (end_time_utc > start_time_utc),
              CONSTRAINT ck_games_freeze_window CHECK (
                freeze_time_utc IS NULL OR
                (freeze_time_utc > start_time_utc AND freeze_time_utc < end_time_utc)
              )
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              accepted_count INTEGER NOT NULL DEFAULT 0,
              submission_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE "Submissions" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "FirstSolves" (
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              submission_id INTEGER NOT NULL
            );
            CREATE TABLE "AdRounds" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
            CREATE TABLE "AdEpochRollups" (game_id INTEGER NOT NULL);
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL,
              writeup_id INTEGER
            );
            CREATE TABLE "KothCrownCycles" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              champion_participation_id INTEGER,
              provisional_participation_id INTEGER,
              confirmed_participation_id INTEGER
            );
            CREATE TABLE "KothEpochRollups" (game_id INTEGER NOT NULL);
            CREATE TABLE "KothAcquisitions" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "KothControlResults" (
              game_id INTEGER NOT NULL,
              controlling_participation_id INTEGER,
              responsible_participation_id INTEGER,
              provisional_participation_id INTEGER,
              confirmed_participation_id INTEGER
            );
            CREATE TABLE "SuspicionEvents" (
              game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "HoneypotHits" (
              game_id INTEGER,
              participation_id INTEGER
            );
            CREATE TABLE "ContainerAccessEvents" (
              game_id INTEGER NOT NULL,
              container_owner_participation_id INTEGER NOT NULL,
              accessing_participation_id INTEGER
            );
            CREATE TABLE "FlagEgressEvents" (
              game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            CREATE TABLE "TrafficCaptureFailures" (
              challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::services::participation_evidence::create_test_evidence_tables(&pool)
            .await
            .unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn add_game(&self, id: i32, started: bool, marker: bool) {
        let start_interval = if started { "-1 hour" } else { "1 hour" };
        sqlx::query(
            r#"INSERT INTO "Games"
                 (id, practice_mode, start_time_utc, end_time_utc, freeze_time_utc,
                  ad_scoring_start_round, koth_scoring_start_round)
               VALUES ($1, TRUE,
                       clock_timestamp() + $2::interval,
                       clock_timestamp() + interval '2 hours',
                       clock_timestamp() + interval '90 minutes', $3, NULL)"#,
        )
        .bind(id)
        .bind(start_interval)
        .bind(marker.then_some(1))
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" (id) VALUES ($1)"#)
            .bind(id * 1_000)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "Participations" (id, game_id, team_id, status)
               VALUES ($1, $2, $3, 1)"#,
        )
        .bind(id * 100)
        .bind(id)
        .bind(id * 1_000)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (id, game_id, accepted_count, submission_count)
               VALUES ($1, $2, 0, 0)"#,
        )
        .bind(id * 10)
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn game_deletion_fence_allows_only_future_games_without_evidence() {
    let harness = DeletionFenceHarness::new().await;
    for (id, started, marker) in [
        (1, false, false),
        (2, true, false),
        (3, false, false),
        (4, false, false),
        (5, false, true),
        (6, false, false),
    ] {
        harness.add_game(id, started, marker).await;
    }
    sqlx::query(r#"INSERT INTO "Submissions" VALUES (31, 3, 30, 300)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "FirstSolves" VALUES (301, 30, 31)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AdRounds" VALUES (41, 4)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "KothCrownCycles" VALUES (61, 6, NULL, NULL, NULL)"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let mut allowed = harness.pool.begin().await.unwrap();
    fence_game_for_deletion(&mut allowed, 1).await.unwrap();
    allowed.commit().await.unwrap();

    for game_id in [2, 3, 4, 5, 6] {
        let mut rejected = harness.pool.begin().await.unwrap();
        assert!(
            fence_game_for_deletion(&mut rejected, game_id)
                .await
                .is_err(),
            "protected game {game_id} was deletable"
        );
        rejected.rollback().await.unwrap();
    }

    let states: Vec<(i32, bool, bool, bool, bool, bool)> = sqlx::query_as(
        r#"SELECT id, practice_mode,
                  end_time_utc = start_time_utc + interval '1 microsecond',
                  deletion_pending, hidden, freeze_time_utc IS NULL
             FROM "Games" ORDER BY id"#,
    )
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    assert_eq!(states[0], (1, false, true, true, true, true));
    for state in &states[1..] {
        assert_eq!(
            (state.1, state.2, state.3, state.4, state.5),
            (true, false, false, false, false)
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Submissions""#)
            .fetch_one(&harness.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "FirstSolves""#)
            .fetch_one(&harness.pool)
            .await
            .unwrap(),
        1
    );
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn authorized_game_deletion_survives_time_passage_but_not_late_evidence() {
    let harness = DeletionFenceHarness::new().await;
    for game_id in 10..=15 {
        harness.add_game(game_id, false, false).await;
        let mut authorization = harness.pool.begin().await.unwrap();
        fence_game_for_deletion(&mut authorization, game_id)
            .await
            .unwrap();
        authorization.commit().await.unwrap();
    }

    // The committed fence is already an authoritative ended marker. Let its
    // timestamp age before retrying, as slow backend teardown would.
    tokio::time::sleep(Duration::from_millis(2)).await;
    sqlx::query(r#"INSERT INTO "Submissions" VALUES (111, 11, 110, 1100)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AdRounds" VALUES (121, 12)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "FlagEgressEvents" (game_id, participation_id)
           VALUES (13, 1300)"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "TrafficCaptureFailures" (challenge_id, participation_id)
           VALUES (140, 1400)"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Participations" SET writeup_id = 42 WHERE game_id = 15"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let mut no_evidence = harness.pool.begin().await.unwrap();
    fence_game_for_deletion(&mut no_evidence, 10)
        .await
        .expect("time passage invalidated a durable deletion authorization");
    no_evidence.commit().await.unwrap();
    let retried: (bool, bool, bool, bool, bool) = sqlx::query_as(
        r#"SELECT deletion_pending, hidden, NOT practice_mode,
                  end_time_utc > start_time_utc,
                  end_time_utc <= clock_timestamp()
             FROM "Games" WHERE id = 10"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(
        retried,
        (true, true, true, true, true),
        "authorized retry weakened the durable deletion fence"
    );

    for (game_id, label) in [
        (11, "submission"),
        (12, "scoring"),
        (13, "audit"),
        (14, "runtime failure"),
        (15, "writeup"),
    ] {
        let mut retry = harness.pool.begin().await.unwrap();
        let error = fence_game_for_deletion(&mut retry, game_id)
            .await
            .expect_err("late evidence was ignored");
        assert_eq!(
            error.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "{label} did not preserve the game"
        );
        retry.rollback().await.unwrap();
    }
    let retained_fences: Vec<(bool, bool, bool, bool)> = sqlx::query_as(
        r#"SELECT deletion_pending, hidden, NOT practice_mode,
                  end_time_utc <= clock_timestamp()
             FROM "Games" WHERE id BETWEEN 11 AND 15 ORDER BY id"#,
    )
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    assert!(
        retained_fences
            .iter()
            .all(|state| *state == (true, true, true, true)),
        "late-evidence rollback erased an already committed deletion fence"
    );
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn deletion_marker_blocks_a_delayed_game_audit_writer() {
    let harness = DeletionFenceHarness::new().await;
    harness.add_game(15, false, false).await;

    let mut deletion = harness.pool.begin().await.unwrap();
    fence_game_for_deletion(&mut deletion, 15).await.unwrap();
    let mut late_writer = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            let eligible = crate::services::participation_evidence::lock_audit_insert_scope(
                &mut transaction,
                15,
                Some(150),
                &[1500],
            )
            .await
            .unwrap();
            if eligible {
                sqlx::query(
                    r#"INSERT INTO "FlagEgressEvents" (game_id, participation_id)
                       VALUES (15, 1500)"#,
                )
                .execute(&mut *transaction)
                .await
                .unwrap();
            }
            transaction.commit().await.unwrap();
            eligible
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut late_writer)
            .await
            .is_err(),
        "late writer crossed the uncommitted game deletion marker"
    );
    deletion.commit().await.unwrap();
    assert!(
        !tokio::time::timeout(Duration::from_secs(2), late_writer)
            .await
            .unwrap()
            .unwrap(),
        "late writer attributed evidence after game deletion was fenced"
    );
    let mut retry = harness.pool.begin().await.unwrap();
    fence_game_for_deletion(&mut retry, 15)
        .await
        .expect("blocked audit writer poisoned the authorized retry");
    retry.rollback().await.unwrap();
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_submission_commits_before_game_deletion_decision() {
    let harness = DeletionFenceHarness::new().await;
    harness.add_game(7, false, false).await;

    let mut submission = harness.pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 7 FOR SHARE"#)
        .execute(&mut *submission)
        .await
        .unwrap();
    crate::utils::scoring::lock_jeopardy_flags_shared(&mut submission, 70)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" VALUES (71, 7, 70, 700)"#)
        .execute(&mut *submission)
        .await
        .unwrap();

    let pool = harness.pool.clone();
    let mut deletion = tokio::spawn(async move {
        let mut transaction = pool.begin().await.unwrap();
        let result = fence_game_for_deletion(&mut transaction, 7).await;
        transaction.rollback().await.unwrap();
        result
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "game deletion bypassed the in-flight submission row lock"
    );
    submission.commit().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), deletion)
            .await
            .unwrap()
            .unwrap()
            .is_err(),
        "game deletion ignored a submission that won the race"
    );
    let retained: (bool, bool, i64) = sqlx::query_as(
        r#"SELECT practice_mode,
                  end_time_utc > clock_timestamp(),
                  (SELECT COUNT(*) FROM "Submissions" WHERE game_id = 7)
             FROM "Games" WHERE id = 7"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(retained, (true, true, 1));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn game_deletion_waits_for_an_in_flight_challenge_definition() {
    let harness = DeletionFenceHarness::new().await;
    harness.add_game(16, false, false).await;
    let definition =
        crate::services::challenge_workloads::acquire_definition_lock(&harness.pool, 16, 160)
            .await
            .unwrap();
    let mut deletion = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            fence_game_for_deletion(&mut transaction, 16).await.unwrap();
            transaction.commit().await.unwrap();
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "whole-game deletion did not acquire the challenge definition fence"
    );
    definition.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), deletion)
        .await
        .expect("game deletion did not resume after definition commit")
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(r#"SELECT deletion_pending FROM "Games" WHERE id = 16"#)
            .fetch_one(&harness.pool)
            .await
            .unwrap()
    );
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn game_deletion_preserves_audit_only_evidence() {
    let harness = DeletionFenceHarness::new().await;
    harness.add_game(8, false, false).await;
    sqlx::query(r#"INSERT INTO "FlagEgressEvents" (game_id, participation_id) VALUES (8, 800)"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let mut deletion = harness.pool.begin().await.unwrap();
    let error = fence_game_for_deletion(&mut deletion, 8)
        .await
        .expect_err("audit-only game history was deletable");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    deletion.rollback().await.unwrap();
    let state: (bool, bool) = sqlx::query_as(
        r#"SELECT practice_mode, end_time_utc > clock_timestamp()
             FROM "Games" WHERE id = 8"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(state, (true, true));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn game_deletion_serializes_both_sides_of_an_audit_insert_race() {
    let harness = DeletionFenceHarness::new().await;
    harness.add_game(9, false, false).await;

    // Writer-first: the game update waits for the writer's shared scope lock,
    // then rejects deletion using a fresh evidence snapshot.
    let mut writer = harness.pool.begin().await.unwrap();
    assert!(
        crate::services::participation_evidence::lock_audit_insert_scope(
            &mut writer,
            9,
            Some(90),
            &[900],
        )
        .await
        .unwrap()
    );
    sqlx::query(
        r#"INSERT INTO "ContainerAccessEvents"
             (game_id, container_owner_participation_id, accessing_participation_id)
           VALUES (9, 900, NULL)"#,
    )
    .execute(&mut *writer)
    .await
    .unwrap();
    let mut deletion = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            let result = fence_game_for_deletion(&mut transaction, 9).await;
            transaction.rollback().await.unwrap();
            result
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "game deletion crossed an in-flight audit writer"
    );
    writer.commit().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), deletion)
            .await
            .unwrap()
            .unwrap()
            .is_err(),
        "game deletion ignored newly committed audit evidence"
    );

    // Delete-first: the late writer blocks at the game row and returns false,
    // so it cannot emit an orphan game/participation identity.
    sqlx::query(r#"DELETE FROM "ContainerAccessEvents""#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let mut delete_first = harness.pool.begin().await.unwrap();
    fence_game_for_deletion(&mut delete_first, 9).await.unwrap();
    sqlx::query(r#"DELETE FROM "Games" WHERE id = 9"#)
        .execute(&mut *delete_first)
        .await
        .unwrap();
    let mut late_writer = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            let scope_exists = crate::services::participation_evidence::lock_audit_insert_scope(
                &mut transaction,
                9,
                Some(90),
                &[900],
            )
            .await
            .unwrap();
            if scope_exists {
                sqlx::query(
                    r#"INSERT INTO "ContainerAccessEvents"
                         (game_id, container_owner_participation_id,
                          accessing_participation_id)
                       VALUES (9, 900, NULL)"#,
                )
                .execute(&mut *transaction)
                .await
                .unwrap();
            }
            transaction.commit().await.unwrap();
            scope_exists
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut late_writer)
            .await
            .is_err(),
        "late audit writer crossed the game deletion lock"
    );
    delete_first.commit().await.unwrap();
    assert!(
        !tokio::time::timeout(Duration::from_secs(2), late_writer)
            .await
            .unwrap()
            .unwrap(),
        "late audit writer did not observe the deleted game"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "ContainerAccessEvents""#)
            .fetch_one(&harness.pool)
            .await
            .unwrap(),
        0
    );
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn game_cleanup_is_complete_scoped_and_idempotent() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "Participations" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TEMP TABLE "AdRounds" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TEMP TABLE "AdTeamServices" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TEMP TABLE "AdFlagDeliveryResults" (round_id INTEGER, team_service_id INTEGER);
        CREATE TEMP TABLE "AdAttacks" (
          id INTEGER PRIMARY KEY, round_id INTEGER, attacker_participation_id INTEGER,
          victim_team_service_id INTEGER, flag_id INTEGER
        );
        CREATE TEMP TABLE "AdCheckResults" (id INTEGER PRIMARY KEY, round_id INTEGER, team_service_id INTEGER);
        CREATE TEMP TABLE "AdFlags" (id INTEGER PRIMARY KEY, round_id INTEGER, team_service_id INTEGER);
        CREATE TEMP TABLE "AdEpochRollups" (game_id INTEGER, epoch INTEGER);
        CREATE TEMP TABLE "AdEpochServiceRollups" (game_id INTEGER, epoch INTEGER);
        CREATE TEMP TABLE "AdEpochTeamRollups" (game_id INTEGER, epoch INTEGER);
        CREATE TEMP TABLE "AdTeamApiTokens" (id INTEGER PRIMARY KEY, participation_id INTEGER);
        CREATE TEMP TABLE "AdSshKeys" (id INTEGER PRIMARY KEY, participation_id INTEGER);
        CREATE TEMP TABLE "AdVpnPeers" (id INTEGER PRIMARY KEY, game_id INTEGER);
        CREATE TEMP TABLE "KothAcquisitions" (id INTEGER PRIMARY KEY, game_id INTEGER);
        CREATE TEMP TABLE "KothControlResults" (id INTEGER PRIMARY KEY, game_id INTEGER);
        CREATE TEMP TABLE "KothTokens" (
          id INTEGER PRIMARY KEY, ad_round_id INTEGER, participation_id INTEGER
        );

        INSERT INTO "Participations" VALUES (11, 1), (22, 2);
        INSERT INTO "AdRounds" VALUES (101, 1), (202, 2);
        INSERT INTO "AdTeamServices" VALUES (111, 1), (222, 2);
        INSERT INTO "AdFlags" VALUES (1001, 101, 111), (2002, 202, 222);
        INSERT INTO "AdFlagDeliveryResults" VALUES (101, 111), (202, 222);
        INSERT INTO "AdCheckResults" VALUES (10001, 101, 111), (20002, 202, 222);
        INSERT INTO "AdAttacks" VALUES (1, 101, 11, 111, 1001), (2, 202, 22, 222, 2002);
        INSERT INTO "AdEpochRollups" VALUES (1, 1), (2, 1);
        INSERT INTO "AdEpochServiceRollups" VALUES (1, 1), (2, 1);
        INSERT INTO "AdEpochTeamRollups" VALUES (1, 1), (2, 1);
        INSERT INTO "AdTeamApiTokens" VALUES (1, 11), (2, 22);
        INSERT INTO "AdSshKeys" VALUES (1, 11), (2, 22);
        INSERT INTO "AdVpnPeers" VALUES (1, 1), (2, 2);
        INSERT INTO "KothAcquisitions" VALUES (1, 1), (2, 2);
        INSERT INTO "KothControlResults" VALUES (1, 1), (2, 2);
        INSERT INTO "KothTokens" VALUES (1, 101, 11), (2, 202, 22);
        "#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    delete_ad_game_data(&mut tx, 1).await.unwrap();
    delete_ad_game_data(&mut tx, 1).await.unwrap();

    for table in [
        "AdFlagDeliveryResults",
        "AdAttacks",
        "AdCheckResults",
        "AdFlags",
        "AdEpochServiceRollups",
        "AdEpochTeamRollups",
        "AdEpochRollups",
        "AdTeamApiTokens",
        "AdSshKeys",
        "AdVpnPeers",
        "KothAcquisitions",
        "KothControlResults",
        "KothTokens",
        "AdTeamServices",
        "AdRounds",
    ] {
        let count: i64 = sqlx::query_scalar(&format!(r#"SELECT COUNT(*) FROM "{table}""#))
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(count, 1, "{table} should retain only game 2 data");
    }
    tx.rollback().await.unwrap();
}
