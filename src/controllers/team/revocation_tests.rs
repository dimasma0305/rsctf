use super::*;

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

fn participation_row(status: i16) -> ParticipationRow {
    ParticipationRow {
        id: 1,
        status,
        token: "token".to_string(),
        writeup_id: None,
        game_id: 2,
        team_id: 3,
        division_id: None,
        suspicion_score: 0,
    }
}

#[test]
fn participation_rows_decode_only_known_statuses() {
    for expected in [
        crate::utils::enums::ParticipationStatus::Pending,
        crate::utils::enums::ParticipationStatus::Accepted,
        crate::utils::enums::ParticipationStatus::Rejected,
        crate::utils::enums::ParticipationStatus::Suspended,
        crate::utils::enums::ParticipationStatus::Unsubmitted,
    ] {
        let model = participation::Model::try_from(participation_row(expected as i16))
            .expect("known participation status");
        assert_eq!(model.status, expected);
    }
    assert!(participation::Model::try_from(participation_row(i16::MAX)).is_err());
}

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
        CREATE TABLE "UserParticipations" (
          team_id INTEGER NOT NULL,
          user_id UUID NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create roster fixture tables");
    let user_id = uuid::Uuid::new_v4();
    for table in ["TeamMembers", "UserParticipations"] {
        sqlx::query(&format!(
            r#"INSERT INTO "{table}" (team_id, user_id) VALUES (9, $1)"#
        ))
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    }

    // A teardown error drops the guard without touching roster rows, so the
    // original action remains authorized and retryable.
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
        CREATE TABLE "TeamMembers" (team_id INTEGER NOT NULL);
        CREATE TABLE "UserParticipations" (team_id INTEGER NOT NULL);
        INSERT INTO "Games" VALUES (11, NULL, NULL), (22, NULL, NULL);
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

    // Once teardown has durably fenced the team, a retry must remain possible
    // even if scoring starts before the external cleanup failure is retried.
    sqlx::query(r#"UPDATE "Teams" SET deletion_pending = TRUE WHERE id = 9"#)
        .execute(&pool)
        .await
        .unwrap();
    let mut control = crate::utils::single_flight::PgAdvisoryLock::acquire(&pool, key)
        .await
        .unwrap();
    mark_team_participations_revoked(&mut control, 9)
        .await
        .expect("already-fenced deletion must be retryable after scoring starts");
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
