use super::*;
use sea_orm::{ActiveModelTrait, Set, SqlxPostgresConnector};
use sea_orm_migration::MigratorTrait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{str::FromStr, sync::Arc};

use crate::app_state::AppState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::{ad_team_api_token, game, team, user};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;
use crate::utils::enums::Role;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn roster_removal_stays_invisible_until_teardown_lock_commits() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_roster_teardown_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    sqlx::raw_sql(
        r#"
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          status SMALLINT NOT NULL,
          game_id INTEGER NOT NULL
        );
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY, end_time_utc TIMESTAMPTZ NOT NULL);
        CREATE TABLE "UserParticipations" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL,
          participation_id INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create roster fixture tables");
    let user_id = uuid::Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES (9, $1)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    // Deliberately dangling: ordinary roster cleanup must still remove legacy
    // links that have no participation identity to preserve.
    sqlx::query(
        r#"INSERT INTO "UserParticipations" (team_id, user_id, participation_id)
           VALUES (9, $1, 99)"#,
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    // A failed teardown leaves roster rows untouched and retryable.
    let failed_attempt = acquire_roster_mutation(&pool, 9).await.unwrap();
    drop(failed_attempt);
    let visible_after_failure: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "TeamMembers" WHERE user_id = $1"#)
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(visible_after_failure, 1);

    let mut roster = acquire_roster_mutation(&pool, 9).await.unwrap();
    let mut issuer = tokio::spawn({
        let pool = pool.clone();
        async move { crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, "team-roster:9").await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut issuer)
            .await
            .is_err(),
        "credential issuer entered during retained teardown lock"
    );
    let visible_before: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "TeamMembers" WHERE user_id = $1"#)
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        visible_before, 1,
        "membership vanished before teardown succeeded"
    );

    remove_membership(roster.transaction_mut(), 9, user_id)
        .await
        .unwrap();
    let visible_uncommitted: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "TeamMembers" WHERE user_id = $1"#)
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        visible_uncommitted, 1,
        "membership deletion leaked before commit"
    );
    roster.release().await.unwrap();

    let acquired = tokio::time::timeout(std::time::Duration::from_secs(2), issuer)
        .await
        .expect("issuer remained blocked after roster commit")
        .expect("issuer task failed")
        .expect("issuer lock failed");
    acquired.release().await.unwrap();
    for table in ["TeamMembers", "UserParticipations"] {
        let remaining: i64 = sqlx::query_scalar(&format!(
            r#"SELECT COUNT(*) FROM "{table}" WHERE user_id = $1"#
        ))
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0, "{table} did not commit atomically");
    }

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn multi_game_revocation_is_atomic_and_deletion_has_one_owner() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_team_revoke_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          start_time_utc TIMESTAMPTZ NOT NULL,
          practice_mode BOOLEAN NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
          ad_scoring_start_round INTEGER,
          koth_scoring_start_round INTEGER
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
          invite_token TEXT NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL,
          token TEXT NOT NULL,
          writeup_id INTEGER,
          division_id INTEGER,
          suspicion_score INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL
        );
        CREATE TABLE "Submissions" (
          id INTEGER PRIMARY KEY,
          participation_id INTEGER NOT NULL
            REFERENCES "Participations"(id) ON DELETE CASCADE,
          challenge_id INTEGER NOT NULL,
          status SMALLINT NOT NULL
        );
        CREATE TABLE "FirstSolves" (
          participation_id INTEGER NOT NULL,
          submission_id INTEGER NOT NULL
            REFERENCES "Submissions"(id) ON DELETE CASCADE
        );
        CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL);
        CREATE TABLE "UserParticipations" (team_id INTEGER NOT NULL);
        INSERT INTO "Games" VALUES
          (11, clock_timestamp() + interval '1 hour', FALSE, FALSE, NULL, NULL),
          (22, clock_timestamp() + interval '1 hour', FALSE, FALSE, NULL, NULL);
        INSERT INTO "Teams" VALUES (9, FALSE, 'original-secret');
        INSERT INTO "Participations"
          (id, game_id, team_id, status, token, writeup_id, division_id, suspicion_score)
        VALUES
          (1, 22, 9, 1, 'part-one', NULL, 3, 7),
          (2, 11, 9, 1, 'part-two', 4, NULL, 8);
        INSERT INTO "TeamMembers" VALUES (9);
        INSERT INTO "UserParticipations" VALUES (9);
        "#,
    )
    .execute(&pool)
    .await
    .expect("create revocation fixtures");
    crate::services::participation_evidence::create_test_evidence_tables(&pool)
        .await
        .expect("create competition-evidence fixtures");

    let mut parts = team_participations(&pool, 9)
        .await
        .expect("load participations through raw SQL");
    parts.sort_by_key(|part| part.id);
    assert_eq!(parts.len(), 2);
    assert_eq!(
        parts[0].status,
        crate::utils::enums::ParticipationStatus::Accepted
    );
    assert_eq!(parts[0].token, "part-one");
    assert_eq!(parts[0].division_id, Some(3));
    assert_eq!(parts[0].suspicion_score, 7);
    assert_eq!(parts[1].writeup_id, Some(4));
    sqlx::query(r#"UPDATE "Participations" SET writeup_id = NULL WHERE id = 2"#)
        .execute(&pool)
        .await
        .unwrap();
    rotate_team_invite_secret(&pool, 9)
        .await
        .expect("rotate team invite secret through raw SQL");
    let rotated_secret: String =
        sqlx::query_scalar(r#"SELECT invite_token FROM "Teams" WHERE id = 9"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(rotated_secret, "original-secret");
    let key = "team-roster:9";
    sqlx::query(r#"INSERT INTO "SuspicionEvents" (participation_id) VALUES (1)"#)
        .execute(&pool)
        .await
        .unwrap();
    let mut audit_blocked = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    let error = mark_team_participations_revoked(&mut audit_blocked, 9)
        .await
        .expect_err("audit-only evidence did not preserve the team identity");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    drop(audit_blocked);
    sqlx::query(r#"DELETE FROM "SuspicionEvents""#)
        .execute(&pool)
        .await
        .unwrap();
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    require_team_mutable(control.transaction_mut(), 9)
        .await
        .expect("ordinary mutation rejected before deletion started");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        mark_team_participations_revoked(&mut control, 9),
    )
    .await
    .expect("revocation nested a second pool connection")
    .unwrap();
    control.release().await.unwrap();
    let statuses: Vec<i16> =
        sqlx::query_scalar(r#"SELECT status FROM "Participations" ORDER BY id"#)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![
            crate::utils::enums::ParticipationStatus::Suspended as i16,
            crate::utils::enums::ParticipationStatus::Suspended as i16,
        ]
    );
    assert!(
        sqlx::query_scalar::<_, bool>(r#"SELECT deletion_pending FROM "Teams" WHERE id = 9"#)
            .fetch_one(&pool)
            .await
            .unwrap()
    );

    sqlx::raw_sql(
        r#"UPDATE "Participations" SET status = 1;
           UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 9;"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO "GameChallenges" VALUES (101, 11);
           INSERT INTO "Submissions" VALUES (201, 2, 101, 1);
           INSERT INTO "FirstSolves" VALUES (2, 201);
           UPDATE "Participations" SET status = 2 WHERE id = 2;"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "Games"
              SET start_time_utc = clock_timestamp() - interval '1 hour'
            WHERE id = 11"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    let error = mark_team_participations_revoked(&mut control, 9)
        .await
        .expect_err("started Jeopardy game must preserve final team history");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    drop(control);
    let statuses: Vec<i16> =
        sqlx::query_scalar(r#"SELECT status FROM "Participations" ORDER BY id"#)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        statuses,
        vec![1, 2],
        "failed Jeopardy deletion changed roster state"
    );
    for table in ["Submissions", "FirstSolves"] {
        let retained: i64 = sqlx::query_scalar(&format!(r#"SELECT COUNT(*) FROM "{table}""#))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            retained, 1,
            "{table} history was removed by a rejected deletion"
        );
    }
    assert!(
        !sqlx::query_scalar::<_, bool>(r#"SELECT deletion_pending FROM "Teams" WHERE id = 9"#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "failed Jeopardy deletion persisted the deletion fence"
    );

    sqlx::raw_sql(
        r#"UPDATE "Games"
              SET start_time_utc = clock_timestamp() + interval '1 hour',
                  practice_mode = TRUE
            WHERE id = 11;
           UPDATE "Participations" SET status = 1 WHERE id = 2;
           DELETE FROM "FirstSolves";
           DELETE FROM "Submissions";"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    mark_team_participations_revoked(&mut control, 9)
        .await
        .expect("future practice game without evidence blocked team deletion");
    control.release().await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(r#"SELECT deletion_pending FROM "Teams" WHERE id = 9"#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "allowed future practice deletion did not commit its fence"
    );

    sqlx::raw_sql(
        r#"UPDATE "Games" SET practice_mode = FALSE WHERE id = 11;
           UPDATE "Participations" SET status = 1;
           UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 9;"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Games" SET ad_scoring_start_round = 1 WHERE id = 22"#)
        .execute(&pool)
        .await
        .unwrap();
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    let error = mark_team_participations_revoked(&mut control, 9)
        .await
        .expect_err("scored game must reject team deletion");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    drop(control);
    let statuses: Vec<i16> =
        sqlx::query_scalar(r#"SELECT status FROM "Participations" ORDER BY id"#)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(statuses, vec![1, 1], "failed revocation must roll back");
    assert!(
        !sqlx::query_scalar::<_, bool>(r#"SELECT deletion_pending FROM "Teams" WHERE id = 9"#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "failed revocation must roll back the deletion fence"
    );

    sqlx::query(r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 9"#)
        .execute(&pool)
        .await
        .unwrap();
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    let retry_error = mark_team_participations_revoked(&mut control, 9)
        .await
        .expect_err("a deletion retry ignored scoring that began during teardown");
    assert_eq!(retry_error.status(), axum::http::StatusCode::BAD_REQUEST);
    drop(control);

    let deletion_lease = TeamDeletionLease::acquire(&pool, key, 9)
        .await
        .unwrap()
        .expect("fenced team needs external deletion ownership");
    let late_marker = deletion_lease
        .finalize(9)
        .await
        .expect_err("final cascade ignored scoring that began during teardown");
    assert_eq!(late_marker.status(), axum::http::StatusCode::BAD_REQUEST);
    sqlx::raw_sql(
        r#"UPDATE "Games" SET ad_scoring_start_round = NULL WHERE id = 22;
           UPDATE "Teams" SET deletion_pending = FALSE WHERE id = 9;
           UPDATE "Participations" SET status = 1 WHERE team_id = 9;"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut audit_writer = pool.begin().await.unwrap();
    assert!(
        crate::services::participation_evidence::lock_audit_insert_scope(
            &mut audit_writer,
            22,
            None,
            &[1],
        )
        .await
        .unwrap()
    );
    sqlx::query(r#"INSERT INTO "HoneypotHits" (participation_id) VALUES (1)"#)
        .execute(&mut *audit_writer)
        .await
        .unwrap();
    let mut deletion = tokio::spawn({
        let pool = pool.clone();
        async move {
            let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
                .await
                .unwrap();
            mark_team_participations_revoked(&mut control, 9).await
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "team deletion crossed an in-flight audit identity lock"
    );
    audit_writer.commit().await.unwrap();
    let deletion_error = tokio::time::timeout(std::time::Duration::from_secs(2), deletion)
        .await
        .expect("team deletion remained blocked after audit commit")
        .expect("team deletion task panicked")
        .expect_err("team deletion ignored newly committed audit evidence");
    assert_eq!(deletion_error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Teams" WHERE id = 9"#)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    sqlx::query(r#"DELETE FROM "HoneypotHits""#)
        .execute(&pool)
        .await
        .unwrap();
    let mut deletion = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    mark_team_participations_revoked(&mut deletion, 9)
        .await
        .expect("clean retry did not restore its durable deletion fence");
    deletion.release().await.unwrap();
    let mut final_control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    let error = require_team_mutable(final_control.transaction_mut(), 9)
        .await
        .expect_err("ordinary mutation passed a durable deletion fence");
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    final_control.release().await.unwrap();

    let deletion_lease = TeamDeletionLease::acquire(&pool, key, 9)
        .await
        .unwrap()
        .expect("fenced team needs external deletion ownership");
    let mut duplicate = tokio::spawn({
        let pool = pool.clone();
        async move { TeamDeletionLease::acquire(&pool, key, 9).await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut duplicate)
            .await
            .is_err(),
        "duplicate deletion entered external teardown concurrently"
    );
    deletion_lease
        .finalize(9)
        .await
        .expect("fenced final cascade failed");
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), duplicate)
            .await
            .expect("duplicate deletion stayed blocked")
            .expect("duplicate task failed")
            .expect("duplicate lease check failed")
            .is_none(),
        "duplicate deletion repeated teardown after the first cascade"
    );
    for table in [
        "Teams",
        "Participations",
        "TeamMembers",
        "UserParticipations",
    ] {
        let remaining: i64 = sqlx::query_scalar(&format!(r#"SELECT COUNT(*) FROM "{table}""#))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0, "{table} survived final cascade");
    }
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn inflight_submit_commits_before_team_deletion_checks_evidence() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_team_submit_race_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          start_time_utc TIMESTAMPTZ NOT NULL,
          ad_scoring_start_round INTEGER,
          koth_scoring_start_round INTEGER
        );
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
        CREATE TABLE "Submissions" (
          id INTEGER PRIMARY KEY,
          participation_id INTEGER NOT NULL
            REFERENCES "Participations"(id) ON DELETE CASCADE
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create submit/deletion race fixtures");
    crate::services::participation_evidence::create_test_evidence_tables(&pool)
        .await
        .expect("create competition-evidence fixtures");

    // Advisory locks are database-global rather than schema-scoped. A random
    // game id prevents independently isolated tests from sharing this key.
    let game_id = (Uuid::new_v4().as_u128() % 1_000_000_000) as i32 + 1;
    sqlx::query(
        r#"INSERT INTO "Games"
             (id, start_time_utc, ad_scoring_start_round, koth_scoring_start_round)
           VALUES ($1, clock_timestamp() + interval '1 hour', NULL, NULL)"#,
    )
    .bind(game_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" (id) VALUES (9)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "Participations" (id, game_id, team_id, status)
           VALUES (13, $1, 9, $2)"#,
    )
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .execute(&pool)
    .await
    .unwrap();

    // This is the lock held by the Jeopardy submit path after its live status
    // check and until its Submission/FirstSolve transaction commits.
    let mut submit = pool.begin().await.unwrap();
    let submit_backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *submit)
        .await
        .unwrap();
    sqlx::query(r#"SELECT id FROM "Participations" WHERE id = 13 FOR SHARE"#)
        .execute(&mut *submit)
        .await
        .unwrap();

    let roster_key = format!("{schema}:team-roster:9");
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, &roster_key)
        .await
        .unwrap();
    let deletion_backend: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut **control.transaction_mut())
        .await
        .unwrap();
    let deletion = tokio::spawn(async move {
        match mark_team_participations_revoked(&mut control, 9).await {
            Ok(()) => control
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string())),
            Err(error) => {
                drop(control);
                Err(error)
            }
        }
    });

    // Wait until revocation is demonstrably behind the submit transaction's
    // participation lock. In the vulnerable ordering this was the later UPDATE,
    // after the no-evidence decision had already been made.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let submit_blocks_deletion: bool =
                sqlx::query_scalar("SELECT $1::integer = ANY(pg_blocking_pids($2::integer))")
                    .bind(submit_backend)
                    .bind(deletion_backend)
                    .fetch_one(&admin_pool)
                    .await
                    .unwrap();
            if submit_blocks_deletion {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("team deletion never waited for the in-flight submit");

    sqlx::query(r#"INSERT INTO "Submissions" (id, participation_id) VALUES (21, 13)"#)
        .execute(&mut *submit)
        .await
        .unwrap();
    submit.commit().await.unwrap();

    let deletion_result = tokio::time::timeout(std::time::Duration::from_secs(2), deletion)
        .await
        .expect("team deletion stayed blocked after submit commit")
        .expect("team deletion task panicked");
    let retained_submission: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Submissions" WHERE id = 21"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, deletion_pending): (i16, bool) = sqlx::query_as(
        r#"SELECT participation.status, team.deletion_pending
             FROM "Participations" participation
             JOIN "Teams" team ON team.id = participation.team_id
            WHERE participation.id = 13"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");

    let error = deletion_result
        .expect_err("team deletion ignored evidence committed by the in-flight submit");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(retained_submission, 1, "committed submission was cascaded");
    assert_eq!(status, ParticipationStatus::Accepted as i16);
    assert!(!deletion_pending, "failed deletion persisted its fence");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn locked_roster_revocation_reuses_the_existing_game_fence() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_locked_roster_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    crate::migrations::Migrator::up(&database, None)
        .await
        .expect("migrate isolated schema");

    let storage_root = std::env::temp_dir().join(format!(
        "rsctf-locked-roster-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut config = AppConfig::default();
    config.storage_root = storage_root.to_string_lossy().into_owned();
    config.jwt_secret = "0123456789abcdef0123456789abcdef".to_string();
    let state = AppState::new(
        database,
        Arc::new(config),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(storage_root.join("blobs"))),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    );

    let now = chrono::Utc::now();
    let user_id = uuid::Uuid::new_v4();
    user::ActiveModel {
        id: Set(user_id),
        user_name: Set(Some("member".to_string())),
        normalized_user_name: Set(Some("MEMBER".to_string())),
        email: Set(Some("member@example.test".to_string())),
        normalized_email: Set(Some("MEMBER@EXAMPLE.TEST".to_string())),
        email_confirmed: Set(true),
        password_hash: Set(None),
        security_stamp: Set(Some("stamp".to_string())),
        concurrency_stamp: Set(None),
        phone_number: Set(None),
        phone_number_confirmed: Set(false),
        two_factor_enabled: Set(false),
        lockout_end: Set(None),
        lockout_enabled: Set(false),
        access_failed_count: Set(0),
        role: Set(Role::User),
        ip: Set(String::new()),
        browser_fingerprint: Set(None),
        last_signed_in_utc: Set(now),
        last_visited_utc: Set(now),
        register_time_utc: Set(now),
        bio: Set(String::new()),
        real_name: Set(String::new()),
        std_number: Set(String::new()),
        exercise_visible: Set(true),
        avatar_hash: Set(None),
    }
    .insert(&state.db)
    .await
    .expect("insert member");
    let team = team::ActiveModel {
        name: Set("fenced".to_string()),
        bio: Set(None),
        avatar_hash: Set(None),
        locked: Set(false),
        deletion_pending: Set(false),
        invite_token: Set("original-secret".to_string()),
        captain_id: Set(user_id),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert team");
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let game = game::ActiveModel {
        title: Set("Warmup".to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        summary: Set(String::new()),
        content: Set(String::new()),
        hidden: Set(false),
        practice_mode: Set(false),
        accept_without_review: Set(false),
        allow_user_submissions: Set(false),
        writeup_required: Set(false),
        invite_code: Set(None),
        team_member_count_limit: Set(0),
        container_count_limit: Set(3),
        start_time_utc: Set(now - chrono::Duration::hours(1)),
        end_time_utc: Set(now + chrono::Duration::hours(1)),
        writeup_deadline: Set(now + chrono::Duration::hours(1)),
        writeup_note: Set(String::new()),
        blood_bonus_value: Set(0),
        ad_allow_snapshot_download: Set(true),
        ad_scoring_paused: Set(false),
        ad_epoch_ticks: Set(8),
        koth_epoch_ticks: Set(12),
        koth_cycle_ticks: Set(3),
        koth_champion_cooldown_ticks: Set(1),
        koth_claim_confirmation_ticks: Set(2),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert game");
    let participation = participation::ActiveModel {
        status: Set(ParticipationStatus::Accepted),
        token: Set("participant-token".to_string()),
        writeup_id: Set(None),
        game_id: Set(game.id),
        team_id: Set(team.id),
        division_id: Set(None),
        suspicion_score: Set(0),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert participation");
    let api_token = ad_team_api_token::ActiveModel {
        participation_id: Set(participation.id),
        token_hash: Set("live-capability".to_string()),
        hint: Set("test".to_string()),
        created_at_utc: Set(now),
        last_rotated_at_utc: Set(None),
        last_used_at_utc: Set(None),
        ..Default::default()
    }
    .insert(&state.db)
    .await
    .expect("insert team API capability");
    sqlx::query(r#"INSERT INTO "TeamMembers" (team_id, user_id) VALUES ($1, $2)"#)
        .bind(team.id)
        .bind(user_id)
        .execute(state.pg())
        .await
        .expect("insert team member");

    let leave_error = crate::controllers::team::leave(
        axum::extract::State(state.clone()),
        CurrentUser {
            id: user_id,
            role: Role::User,
            name: "member".to_string(),
        },
        axum::extract::Path(team.id),
    )
    .await
    .expect_err("captain leave reported success without transferring captaincy");
    assert_eq!(leave_error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        leave_error.to_string(),
        "Team captain must transfer captaincy before leaving"
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                 SELECT 1 FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2
               )"#,
        )
        .bind(team.id)
        .bind(user_id)
        .fetch_one(state.pg())
        .await
        .unwrap(),
        "captain rejection changed the membership row"
    );
    let untouched: (String, bool) = sqlx::query_as(
        r#"SELECT team.invite_token,
                  EXISTS(SELECT 1 FROM "AdTeamApiTokens" token WHERE token.id = $2)
             FROM "Teams" team WHERE team.id = $1"#,
    )
    .bind(team.id)
    .bind(api_token.id)
    .fetch_one(state.pg())
    .await
    .unwrap();
    assert_eq!(untouched, ("original-secret".to_string(), true));

    let mut roster = acquire_roster_mutation(state.pg(), team.id).await.unwrap();
    crate::controllers::team::ensure_roster_change_allowed(roster.transaction_mut(), team.id)
        .await
        .expect("unscored warmup roster should remain mutable");
    let (parts, cache_invalidation) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        revoke_team_shared_capabilities_locked(&state, roster.transaction_mut(), team.id),
    )
    .await
    .expect("locked capability revocation reacquired its own game fence")
    .expect("locked capability revocation failed");
    assert_eq!(
        parts.iter().map(|part| part.id).collect::<Vec<_>>(),
        [participation.id]
    );
    remove_membership(roster.transaction_mut(), team.id, user_id)
        .await
        .unwrap();
    roster.release().await.unwrap();
    cache_invalidation.apply(state.cache.as_ref()).await;

    let member_exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM "TeamMembers" WHERE team_id = $1 AND user_id = $2
           )"#,
    )
    .bind(team.id)
    .bind(user_id)
    .fetch_one(state.pg())
    .await
    .unwrap();
    assert!(!member_exists);
    let invite: String = sqlx::query_scalar(r#"SELECT invite_token FROM "Teams" WHERE id = $1"#)
        .bind(team.id)
        .fetch_one(state.pg())
        .await
        .unwrap();
    assert_ne!(invite, "original-secret");

    drop(state);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
    let _ = tokio::fs::remove_dir_all(storage_root).await;
}
