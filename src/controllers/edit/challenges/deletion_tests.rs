use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::deletion::{acquire_definition_lock, fence_challenge_deletion};
use crate::utils::enums::ChallengeType;

struct Harness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl Harness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_challenge_delete_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              practice_mode BOOLEAN NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              ad_scoring_start_round INTEGER,
              koth_scoring_start_round INTEGER
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              "Type" SMALLINT NOT NULL,
              is_enabled BOOLEAN NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              accepted_count INTEGER NOT NULL DEFAULT 0,
              submission_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE "Submissions" (
              id INTEGER PRIMARY KEY,
              challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "FirstSolves" (
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              submission_id INTEGER NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL,
              status SMALLINT NOT NULL,
              writeup_id INTEGER
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE "SuspicionEvents" (challenge_id INTEGER);
            CREATE TABLE "ContainerAccessEvents" (challenge_id INTEGER NOT NULL);
            CREATE TABLE "FlagEgressEvents" (challenge_id INTEGER NOT NULL);
            CREATE TABLE "TrafficCaptureFailures" (challenge_id INTEGER NOT NULL);
            CREATE TABLE "AdTeamServices" (
              id INTEGER PRIMARY KEY,
              challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "AdFlags" (team_service_id INTEGER NOT NULL);
            CREATE TABLE "AdCheckResults" (team_service_id INTEGER NOT NULL);
            CREATE TABLE "AdAttacks" (victim_team_service_id INTEGER NOT NULL);
            CREATE TABLE "AdFlagDeliveryResults" (team_service_id INTEGER NOT NULL);
            CREATE TABLE "AdEpochServiceRollups" (challenge_id INTEGER NOT NULL);
            CREATE TABLE "KothTokens" (challenge_id INTEGER NOT NULL);
            CREATE TABLE "KothControlResults" (challenge_id INTEGER NOT NULL);
            CREATE TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY,
              challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "KothCycleCooldowns" (cycle_id BIGINT NOT NULL);
            CREATE TABLE "KothCycleAuditReceipts" (cycle_id BIGINT NOT NULL);
            CREATE TABLE "KothTargets" (
              id INTEGER PRIMARY KEY,
              challenge_id INTEGER NOT NULL
            );
            CREATE TABLE "KothClaimStates" (target_id INTEGER NOT NULL);
            CREATE TABLE "KothAcquisitions" (challenge_id INTEGER NOT NULL);
            CREATE TABLE "KothEpochHillRollups" (challenge_id INTEGER NOT NULL);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn add_game(&self, id: i32, started: bool, practice: bool) {
        let interval = if started { "-1 hour" } else { "1 hour" };
        sqlx::query(
            r#"INSERT INTO "Games"
                 (id, practice_mode, start_time_utc,
                  ad_scoring_start_round, koth_scoring_start_round)
               VALUES ($1, $2, clock_timestamp() + $3::interval, NULL, NULL)"#,
        )
        .bind(id)
        .bind(practice)
        .bind(interval)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn add_challenge(&self, id: i32, game_id: i32, accepted: i32, submitted: i32) {
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (id, game_id, "Type", is_enabled, accepted_count, submission_count)
               VALUES ($1, $2, $3, TRUE, $4, $5)"#,
        )
        .bind(id)
        .bind(game_id)
        .bind(ChallengeType::StaticAttachment as i16)
        .bind(accepted)
        .bind(submitted)
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
async fn jeopardy_delete_preserves_started_and_scored_history_but_allows_fresh_practice() {
    let harness = Harness::new().await;
    harness.add_game(1, false, false).await;
    harness.add_game(2, true, false).await;
    harness.add_game(3, false, true).await;
    harness.add_game(4, false, false).await;
    harness.add_game(5, false, false).await;
    harness.add_challenge(11, 1, 0, 0).await;
    harness.add_challenge(12, 2, 0, 0).await;
    harness.add_challenge(13, 3, 0, 0).await;
    harness.add_challenge(14, 4, 1, 1).await;
    harness.add_challenge(15, 5, 0, 0).await;
    sqlx::query(r#"INSERT INTO "Submissions" VALUES (91, 15)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "FirstSolves" VALUES (81, 15, 91)"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    for (game_id, challenge_id) in [(1, 11), (3, 13)] {
        let mut allowed = harness.pool.begin().await.unwrap();
        fence_challenge_deletion(&mut allowed, game_id, challenge_id)
            .await
            .unwrap();
        allowed.commit().await.unwrap();
    }

    for (game_id, challenge_id) in [(2, 12), (4, 14), (5, 15)] {
        let mut transaction = harness.pool.begin().await.unwrap();
        assert!(
            fence_challenge_deletion(&mut transaction, game_id, challenge_id)
                .await
                .is_err(),
            "protected challenge {challenge_id} was deletable"
        );
        transaction.rollback().await.unwrap();
    }

    let states: Vec<(i32, bool, bool)> = sqlx::query_as(
        r#"SELECT id, is_enabled, deletion_pending FROM "GameChallenges" ORDER BY id"#,
    )
    .fetch_all(&harness.pool)
    .await
    .unwrap();
    assert_eq!(
        states,
        vec![
            (11, false, true),
            (12, true, false),
            (13, false, true),
            (14, true, false),
            (15, true, false)
        ]
    );
    let retained: (i64, i64) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*) FROM "Submissions" WHERE challenge_id = 15),
                  (SELECT COUNT(*) FROM "FirstSolves" WHERE challenge_id = 15)"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(retained, (1, 1));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn challenge_delete_rejects_a_committed_parent_game_fence() {
    let harness = Harness::new().await;
    harness.add_game(1, false, false).await;
    harness.add_challenge(11, 1, 0, 0).await;
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let mut deletion = harness.pool.begin().await.unwrap();
    let error = fence_challenge_deletion(&mut deletion, 1, 11)
        .await
        .expect_err("child deletion crossed the parent game's durable fence");
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    deletion.rollback().await.unwrap();

    let state: (bool, bool) = sqlx::query_as(
        r#"SELECT is_enabled, deletion_pending FROM "GameChallenges" WHERE id = 11"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(state, (true, false));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn authorized_challenge_delete_survives_start_but_rejects_late_evidence() {
    let harness = Harness::new().await;
    for game_id in 1..=4 {
        harness.add_game(game_id, false, false).await;
        harness.add_challenge(game_id * 10 + 1, game_id, 0, 0).await;
        let mut authorization = harness.pool.begin().await.unwrap();
        fence_challenge_deletion(&mut authorization, game_id, game_id * 10 + 1)
            .await
            .unwrap();
        authorization.commit().await.unwrap();
    }
    sqlx::query(
        r#"UPDATE "Games"
              SET start_time_utc = clock_timestamp() - interval '1 microsecond'"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" VALUES (91, 21)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "FlagEgressEvents" (challenge_id) VALUES (31)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "TrafficCaptureFailures" (challenge_id) VALUES (41)"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let mut clean_retry = harness.pool.begin().await.unwrap();
    fence_challenge_deletion(&mut clean_retry, 1, 11)
        .await
        .expect("scheduled start invalidated a durable challenge deletion fence");
    clean_retry.commit().await.unwrap();
    for (game_id, challenge_id, label) in [
        (2, 21, "submission"),
        (3, 31, "audit"),
        (4, 41, "runtime failure"),
    ] {
        let mut retry = harness.pool.begin().await.unwrap();
        let error = fence_challenge_deletion(&mut retry, game_id, challenge_id)
            .await
            .expect_err("late evidence was ignored");
        assert_eq!(
            error.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "{label} did not preserve the challenge"
        );
        retry.rollback().await.unwrap();
    }
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn deletion_marker_blocks_a_delayed_challenge_audit_writer() {
    let harness = Harness::new().await;
    harness.add_game(1, false, false).await;
    harness.add_challenge(11, 1, 0, 0).await;
    sqlx::query(r#"INSERT INTO "Teams" (id) VALUES (1001)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, game_id, team_id, status)
           VALUES (101, 1, 1001, 1)"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    let mut deletion = harness.pool.begin().await.unwrap();
    fence_challenge_deletion(&mut deletion, 1, 11)
        .await
        .unwrap();
    let mut late_writer = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            let eligible = crate::services::participation_evidence::lock_audit_insert_scope(
                &mut transaction,
                1,
                Some(11),
                &[101],
            )
            .await
            .unwrap();
            if eligible {
                sqlx::query(r#"INSERT INTO "FlagEgressEvents" (challenge_id) VALUES (11)"#)
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
        "late writer crossed the uncommitted deletion marker"
    );
    deletion.commit().await.unwrap();
    assert!(
        !tokio::time::timeout(Duration::from_secs(2), late_writer)
            .await
            .unwrap()
            .unwrap(),
        "late writer attributed evidence after challenge deletion was fenced"
    );
    let mut retry = harness.pool.begin().await.unwrap();
    fence_challenge_deletion(&mut retry, 1, 11)
        .await
        .expect("blocked audit writer poisoned the authorized retry");
    retry.rollback().await.unwrap();
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn deletion_waits_for_inflight_submit_and_then_preserves_it() {
    let harness = Harness::new().await;
    harness.add_game(1, false, false).await;
    harness.add_challenge(11, 1, 0, 0).await;

    let mut submit = harness.pool.begin().await.unwrap();
    crate::utils::scoring::lock_jeopardy_flags_shared(&mut submit, 11)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" VALUES (91, 11)"#)
        .execute(&mut *submit)
        .await
        .unwrap();

    let pool = harness.pool.clone();
    let mut deletion = tokio::spawn(async move {
        let mut transaction = pool.begin().await.unwrap();
        let result = fence_challenge_deletion(&mut transaction, 1, 11).await;
        transaction.rollback().await.unwrap();
        result
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "deletion bypassed the in-flight submission fence"
    );
    submit.commit().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), deletion)
            .await
            .unwrap()
            .unwrap()
            .is_err(),
        "deletion ignored the newly committed submission"
    );
    let retained: (bool, i64) = sqlx::query_as(
        r#"SELECT challenge.is_enabled,
                  (SELECT COUNT(*) FROM "Submissions" WHERE challenge_id = 11)
             FROM "GameChallenges" challenge WHERE challenge.id = 11"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(retained, (true, 1));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn challenge_deletion_preserves_audit_only_evidence() {
    let harness = Harness::new().await;
    harness.add_game(1, false, false).await;
    harness.add_challenge(11, 1, 0, 0).await;
    sqlx::query(r#"INSERT INTO "FlagEgressEvents" (challenge_id) VALUES (11)"#)
        .execute(&harness.pool)
        .await
        .unwrap();

    let mut deletion = harness.pool.begin().await.unwrap();
    let error = fence_challenge_deletion(&mut deletion, 1, 11)
        .await
        .expect_err("audit-only challenge history was deletable");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    deletion.rollback().await.unwrap();
    assert!(sqlx::query_scalar::<_, bool>(
        r#"SELECT is_enabled FROM "GameChallenges" WHERE id = 11"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap());
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn challenge_deletion_serializes_both_sides_of_an_audit_insert_race() {
    let harness = Harness::new().await;
    harness.add_game(1, false, false).await;
    harness.add_challenge(11, 1, 0, 0).await;
    sqlx::query(r#"INSERT INTO "Teams" (id) VALUES (1001)"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, game_id, team_id, status)
           VALUES (101, 1, 1001, 1)"#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    // Writer-first: deletion waits at the challenge/participation fence, then
    // sees the committed event in a fresh READ COMMITTED statement snapshot.
    let mut writer = harness.pool.begin().await.unwrap();
    assert!(
        crate::services::participation_evidence::lock_audit_insert_scope(
            &mut writer,
            1,
            Some(11),
            &[101],
        )
        .await
        .unwrap()
    );
    sqlx::query(r#"INSERT INTO "ContainerAccessEvents" (challenge_id) VALUES (11)"#)
        .execute(&mut *writer)
        .await
        .unwrap();
    let mut deletion = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            let result = fence_challenge_deletion(&mut transaction, 1, 11).await;
            transaction.rollback().await.unwrap();
            result
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "challenge deletion crossed an in-flight audit writer"
    );
    writer.commit().await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), deletion)
            .await
            .unwrap()
            .unwrap()
            .is_err(),
        "challenge deletion ignored newly committed audit evidence"
    );

    // Delete-first: a late writer waits for the challenge row and then returns
    // false instead of publishing an event with a dangling challenge id.
    sqlx::query(r#"DELETE FROM "ContainerAccessEvents""#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let mut delete_first = harness.pool.begin().await.unwrap();
    fence_challenge_deletion(&mut delete_first, 1, 11)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id = 11"#)
        .execute(&mut *delete_first)
        .await
        .unwrap();
    let mut late_writer = tokio::spawn({
        let pool = harness.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            let scope_exists = crate::services::participation_evidence::lock_audit_insert_scope(
                &mut transaction,
                1,
                Some(11),
                &[101],
            )
            .await
            .unwrap();
            if scope_exists {
                sqlx::query(r#"INSERT INTO "ContainerAccessEvents" (challenge_id) VALUES (11)"#)
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
        "late audit writer crossed the challenge deletion lock"
    );
    delete_first.commit().await.unwrap();
    assert!(
        !tokio::time::timeout(Duration::from_secs(2), late_writer)
            .await
            .unwrap()
            .unwrap(),
        "late audit writer did not observe the deleted challenge"
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
async fn deletion_uses_the_shared_definition_fence() {
    let harness = Harness::new().await;
    let first = acquire_definition_lock(&harness.pool, 7, 11).await.unwrap();
    let pool = harness.pool.clone();
    let mut second = tokio::spawn(async move { acquire_definition_lock(&pool, 7, 11).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second)
            .await
            .is_err(),
        "delete definition fence did not contend with another definition mutation"
    );
    first.release().await.unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    second.release().await.unwrap();
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_game_and_challenge_delete_lock_stacks_do_not_deadlock() {
    let harness = Harness::new().await;
    let game_id = (uuid::Uuid::new_v4().as_u128() % 1_000_000_000) as i32 + 1;
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL").unwrap();
    let single_options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", harness.schema.as_str())]);
    let single_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(single_options)
        .await
        .unwrap();
    let single_database = SqlxPostgresConnector::from_sqlx_postgres_pool(single_pool.clone());
    tokio::time::timeout(Duration::from_secs(2), async {
        let admission = super::super::deletion_locks::acquire_hard_deletion_admission()
            .await
            .unwrap();
        let locks = super::super::deletion_locks::acquire_game_test_deletion_locks(
            &single_database,
            game_id,
            admission,
        )
        .await
        .unwrap();
        locks.release().await.unwrap();
    })
    .await
    .expect("game/test deletion locks requested a second pooled connection");
    single_pool.close().await;

    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(harness.pool.clone());
    let start = Arc::new(tokio::sync::Barrier::new(3));

    let game_delete = tokio::spawn({
        let database = database.clone();
        let start = start.clone();
        async move {
            start.wait().await;
            let admission = super::super::deletion_locks::acquire_hard_deletion_admission().await?;
            let locks = super::super::deletion_locks::acquire_game_test_deletion_locks(
                &database, game_id, admission,
            )
            .await?;
            tokio::task::yield_now().await;
            locks.release().await
        }
    });
    let challenge_delete = tokio::spawn({
        let database = database.clone();
        let pool = harness.pool.clone();
        let start = start.clone();
        async move {
            start.wait().await;
            let admission = super::super::deletion_locks::acquire_hard_deletion_admission().await?;
            let locks = super::super::deletion_locks::acquire_game_test_deletion_locks(
                &database, game_id, admission,
            )
            .await?;
            let definition = acquire_definition_lock(&pool, game_id, 11).await?;
            definition.release().await?;
            locks.release().await
        }
    });

    start.wait().await;
    tokio::time::timeout(Duration::from_secs(3), async {
        game_delete.await.unwrap().unwrap();
        challenge_delete.await.unwrap().unwrap();
    })
    .await
    .expect("concurrent game/challenge deletion lock stacks deadlocked");
    harness.cleanup().await;
}
