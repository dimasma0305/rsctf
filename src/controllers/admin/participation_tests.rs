use super::*;

use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[test]
fn epoch_boundary_freezes_only_real_division_changes() {
    assert!(ensure_scored_division_unchanged(true, Some(3), Some(4)).is_err());
    assert!(ensure_scored_division_unchanged(true, Some(3), None).is_err());
    assert!(ensure_scored_division_unchanged(true, Some(3), Some(3)).is_ok());
    assert!(ensure_scored_division_unchanged(false, Some(3), Some(4)).is_ok());
}

#[test]
fn participation_edit_distinguishes_omitted_null_and_value_divisions() {
    let omitted: ParticipationEditModel = serde_json::from_str(r#"{}"#).unwrap();
    let cleared: ParticipationEditModel = serde_json::from_str(r#"{"divisionId":null}"#).unwrap();
    let selected: ParticipationEditModel = serde_json::from_str(r#"{"divisionId":7}"#).unwrap();

    assert_eq!(omitted.division_id, None);
    assert_eq!(cleared.division_id, Some(None));
    assert_eq!(selected.division_id, Some(Some(7)));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn active_suspension_is_reversible_and_rejection_preserves_jeopardy_evidence() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!(
        "rsctf_participation_sanction_{}",
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          ad_scoring_start_round INTEGER,
          koth_scoring_start_round INTEGER,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          locked BOOLEAN NOT NULL DEFAULT FALSE,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Divisions" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL,
          division_id INTEGER,
          writeup_id INTEGER
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    crate::services::participation_evidence::create_test_evidence_tables(&pool)
        .await
        .unwrap();

    // Advisory keys are database-wide. Random identities keep this regression
    // independent when the ignored PostgreSQL suite runs concurrently.
    let seed = (uuid::Uuid::new_v4().as_u128() % 100_000_000) as i32 + 1_000;
    let identity = ParticipationIdentity {
        id: seed + 2,
        game_id: seed,
        team_id: seed + 1,
    };
    let division_id = seed + 3;
    sqlx::query(
        r#"INSERT INTO "Games" (id, ad_scoring_start_round, koth_scoring_start_round)
           VALUES ($1, NULL, NULL)"#,
    )
    .bind(identity.game_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" VALUES ($1, TRUE, FALSE)"#)
        .bind(identity.team_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Divisions" VALUES ($1, $2)"#)
        .bind(division_id)
        .bind(identity.game_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Participations" VALUES ($1, $2, $3, $4, $5)"#)
        .bind(identity.id)
        .bind(identity.game_id)
        .bind(identity.team_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(division_id)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = $1"#)
        .bind(identity.game_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut pending_game_review = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    for error in [
        persist_participation_status(
            &mut pending_game_review,
            identity,
            ParticipationStatus::Suspended,
            None,
        )
        .await
        .expect_err("status review crossed the game deletion fence"),
        update_division_only(&mut pending_game_review, identity, Some(None))
            .await
            .expect_err("division review crossed the game deletion fence"),
    ] {
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    }
    pending_game_review.release().await.unwrap();
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = FALSE WHERE id = $1"#)
        .bind(identity.game_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Submissions" (participation_id) VALUES ($1)"#)
        .bind(identity.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "FirstSolves" (participation_id) VALUES ($1)"#)
        .bind(identity.id)
        .execute(&pool)
        .await
        .unwrap();

    let mut lease = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    for status in [
        ParticipationStatus::Pending,
        ParticipationStatus::Unsubmitted,
    ] {
        let error = persist_participation_status(&mut lease, identity, status, None)
            .await
            .expect_err("Jeopardy evidence was hidden behind a non-scoring status");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(error.to_string().contains("competition evidence"));
    }
    assert_eq!(
        sqlx::query_scalar::<_, i16>(r#"SELECT status FROM "Participations" WHERE id = $1"#)
            .bind(identity.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        ParticipationStatus::Accepted as i16
    );
    sqlx::query(
        r#"UPDATE "Games"
              SET ad_scoring_start_round = 1, koth_scoring_start_round = 1
            WHERE id = $1"#,
    )
    .bind(identity.game_id)
    .execute(&pool)
    .await
    .unwrap();
    persist_participation_status(&mut lease, identity, ParticipationStatus::Suspended, None)
        .await
        .expect("active roster could not be suspended");
    persist_participation_status(&mut lease, identity, ParticipationStatus::Accepted, None)
        .await
        .expect("active suspended roster could not be reinstated");
    persist_participation_status(&mut lease, identity, ParticipationStatus::Suspended, None)
        .await
        .expect("reinstated roster could not be suspended again");
    let error =
        persist_participation_status(&mut lease, identity, ParticipationStatus::Rejected, None)
            .await
            .expect_err("suspended solver was rejected and lost its scoring identity");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert!(error.to_string().contains("competition evidence"));
    lease.release().await.unwrap();

    let row: (i32, i32, i32, i16, Option<i32>) = sqlx::query_as(
        r#"SELECT id, game_id, team_id, status, division_id
             FROM "Participations" WHERE id = $1"#,
    )
    .bind(identity.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            identity.id,
            identity.game_id,
            identity.team_id,
            ParticipationStatus::Suspended as i16,
            Some(division_id),
        ),
        "sanction transitions changed the immutable roster identity"
    );
    let evidence: (i64, i64) = sqlx::query_as(
        r#"SELECT
              (SELECT COUNT(*) FROM "Submissions" WHERE participation_id = $1),
              (SELECT COUNT(*) FROM "FirstSolves" WHERE participation_id = $1)"#,
    )
    .bind(identity.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(evidence, (1, 1));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .unwrap();
}

/// Use an independent process-local key while retaining the real distributed
/// roster key, modeling an opposing request served by another replica.
async fn acquire_from_other_replica(pool: &sqlx::PgPool, team_id: i32) -> ParticipationReviewLease {
    let local_key = format!("test-review-replica:{}", uuid::Uuid::new_v4());
    let local = crate::utils::single_flight::coalesce(&local_key).await;
    let session = crate::utils::single_flight::PgSessionAdvisoryLock::acquire_roster(
        pool,
        &format!("team-roster:{team_id}"),
    )
    .await
    .unwrap();
    ParticipationReviewLease { session, local }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn opposing_reviews_serialize_status_and_external_effects() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!(
        "rsctf_participation_review_{}",
        uuid::Uuid::new_v4().simple()
    );
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (
          id INTEGER PRIMARY KEY,
          ad_scoring_start_round INTEGER,
          koth_scoring_start_round INTEGER,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY,
          locked BOOLEAN NOT NULL DEFAULT FALSE,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "Divisions" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL,
          status SMALLINT NOT NULL,
          division_id INTEGER,
          writeup_id INTEGER
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    crate::services::participation_evidence::create_test_evidence_tables(&pool)
        .await
        .unwrap();

    // Advisory keys are database-wide rather than schema-scoped. Random IDs
    // keep this test independent when all ignored PostgreSQL tests run together.
    let seed = (uuid::Uuid::new_v4().as_u128() % 100_000_000) as i32 + 1_000;
    let identity = ParticipationIdentity {
        id: seed + 2,
        game_id: seed,
        team_id: seed + 1,
    };
    let division_id = seed + 3;
    let other_division_id = seed + 4;
    sqlx::query(
        r#"INSERT INTO "Games" (id, ad_scoring_start_round, koth_scoring_start_round)
           VALUES ($1, NULL, NULL)"#,
    )
    .bind(identity.game_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" VALUES ($1, FALSE, FALSE)"#)
        .bind(identity.team_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Divisions" VALUES ($1, $2)"#)
        .bind(division_id)
        .bind(identity.game_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Divisions" VALUES ($1, $2)"#)
        .bind(other_division_id)
        .bind(identity.game_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Participations" VALUES ($1, $2, $3, 0, $4)"#)
        .bind(identity.id)
        .bind(identity.game_id)
        .bind(identity.team_id)
        .bind(division_id)
        .execute(&pool)
        .await
        .unwrap();
    let effects = Arc::new(Mutex::new(Vec::new()));
    let mut accepted = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    persist_participation_status(
        &mut accepted,
        identity,
        ParticipationStatus::Accepted,
        Some(Some(division_id)),
    )
    .await
    .unwrap();

    let (attempting_tx, attempting_rx) = tokio::sync::oneshot::channel();
    let second_pool = pool.clone();
    let second_effects = Arc::clone(&effects);
    let mut rejected = tokio::spawn(async move {
        attempting_tx.send(()).unwrap();
        let mut lease = acquire_from_other_replica(&second_pool, identity.team_id).await;
        persist_participation_status(
            &mut lease,
            identity,
            ParticipationStatus::Rejected,
            Some(Some(division_id)),
        )
        .await
        .unwrap();
        let effect_pool = second_pool.clone();
        run_terminal_effect(
            &mut lease,
            identity,
            ParticipationStatus::Rejected,
            || async move {
                let visible: i16 =
                    sqlx::query_scalar(r#"SELECT status FROM "Participations" WHERE id = $1"#)
                        .bind(identity.id)
                        .fetch_one(&effect_pool)
                        .await
                        .unwrap();
                assert_eq!(visible, ParticipationStatus::Rejected as i16);
                second_effects.lock().unwrap().push("revoke");
                Ok(())
            },
        )
        .await
        .unwrap();
        lease.release().await.unwrap();
    });
    attempting_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut rejected)
            .await
            .is_err(),
        "opposing rejection crossed the accepted review's external lease"
    );

    let first_effects = Arc::clone(&effects);
    let first_effect_pool = pool.clone();
    run_terminal_effect(
        &mut accepted,
        identity,
        ParticipationStatus::Accepted,
        || async move {
            let visible: i16 =
                sqlx::query_scalar(r#"SELECT status FROM "Participations" WHERE id = $1"#)
                    .bind(identity.id)
                    .fetch_one(&first_effect_pool)
                    .await
                    .unwrap();
            assert_eq!(visible, ParticipationStatus::Accepted as i16);
            first_effects.lock().unwrap().push("provision");
            Ok(())
        },
    )
    .await
    .unwrap();
    accepted.release().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), rejected)
        .await
        .expect("rejection remained blocked after accepted effect completed")
        .expect("rejection task failed");

    assert_eq!(*effects.lock().unwrap(), vec!["provision", "revoke"]);
    let final_row: (i16, Option<i32>, bool) = sqlx::query_as(
        r#"SELECT participation.status, participation.division_id, team.locked
             FROM "Participations" participation
             JOIN "Teams" team ON team.id = participation.team_id
            WHERE participation.id = $1"#,
    )
    .bind(identity.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        final_row,
        (ParticipationStatus::Rejected as i16, None, true),
        "the final rejection must win without undoing the durable roster freeze"
    );

    // Even an out-of-band stale caller cannot reach its effect: terminal status
    // is checked on the lock-owning session immediately before the closure.
    let stale_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut stale = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    let stale_flag = Arc::clone(&stale_called);
    let error = run_terminal_effect(
        &mut stale,
        identity,
        ParticipationStatus::Accepted,
        || async {
            stale_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
    )
    .await
    .expect_err("a stale accepted review reached provisioning");
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    assert!(!stale_called.load(std::sync::atomic::Ordering::SeqCst));
    stale.release().await.unwrap();

    // A status-bearing review cannot smuggle a division change through by
    // repeating the already-live Accepted status after either scoring engine has
    // declared its immutable boundary.
    sqlx::query(r#"UPDATE "Participations" SET status = $1, division_id = $2 WHERE id = $3"#)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(division_id)
        .bind(identity.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET ad_scoring_start_round = 1 WHERE id = $1"#)
        .bind(identity.game_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut status_review = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    let error = persist_participation_status(
        &mut status_review,
        identity,
        ParticipationStatus::Accepted,
        Some(Some(other_division_id)),
    )
    .await
    .expect_err("status-bearing review changed a scored division");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    status_review.release().await.unwrap();

    // A division-only request must wait behind the same distributed team lease,
    // then take the game advisory fence and observe the already-started boundary.
    let held = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    let (attempting_tx, attempting_rx) = tokio::sync::oneshot::channel();
    let second_pool = pool.clone();
    let mut division_only = tokio::spawn(async move {
        attempting_tx.send(()).unwrap();
        let mut lease = acquire_from_other_replica(&second_pool, identity.team_id).await;
        let result =
            update_division_only(&mut lease, identity, Some(Some(other_division_id))).await;
        lease.release().await.unwrap();
        result
    });
    attempting_rx.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut division_only)
            .await
            .is_err(),
        "division-only review crossed the team roster lease"
    );
    held.release().await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), division_only)
        .await
        .expect("division-only review remained blocked")
        .expect("division-only review task failed")
        .expect_err("division-only review changed a scored division");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);

    assert_eq!(
        sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT division_id FROM "Participations" WHERE id = $1"#,
        )
        .bind(identity.id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        Some(division_id),
        "a rejected scored-division update changed persistent state"
    );

    // A KotH-only boundary has identical semantics. Exact no-op assignments are
    // retry-safe, while clearing a live division remains a real mutation.
    sqlx::query(
        r#"UPDATE "Games"
              SET ad_scoring_start_round = NULL, koth_scoring_start_round = 1
            WHERE id = $1"#,
    )
    .bind(identity.game_id)
    .execute(&pool)
    .await
    .unwrap();
    let mut koth_review = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    update_division_only(&mut koth_review, identity, Some(Some(division_id)))
        .await
        .expect("same-division retry should remain idempotent after scoring");
    let error = update_division_only(&mut koth_review, identity, Some(None))
        .await
        .expect_err("KotH boundary allowed a division clear");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    koth_review.release().await.unwrap();

    // Before either boundary, retain the existing repository semantics.
    sqlx::query(
        r#"UPDATE "Games"
              SET ad_scoring_start_round = NULL, koth_scoring_start_round = NULL
            WHERE id = $1"#,
    )
    .bind(identity.game_id)
    .execute(&pool)
    .await
    .unwrap();
    let mut pre_scoring = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    update_division_only(&mut pre_scoring, identity, Some(Some(other_division_id)))
        .await
        .expect("pre-scoring division update regressed");
    pre_scoring.release().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Option<i32>>(
            r#"SELECT division_id FROM "Participations" WHERE id = $1"#,
        )
        .bind(identity.id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        Some(other_division_id)
    );

    // A submission that began before an opposing rejection holds FOR SHARE on
    // the scoring identity. The review must wait, then see the committed row on
    // its fresh snapshot and preserve both the Accepted status and its division.
    let mut submission = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Participations" WHERE id = $1 FOR SHARE"#)
        .bind(identity.id)
        .fetch_one(&mut *submission)
        .await
        .unwrap();
    let second_pool = pool.clone();
    let mut rejection = tokio::spawn(async move {
        let mut lease = acquire_from_other_replica(&second_pool, identity.team_id).await;
        let result =
            persist_participation_status(&mut lease, identity, ParticipationStatus::Rejected, None)
                .await;
        lease.release().await.unwrap();
        result
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut rejection)
            .await
            .is_err(),
        "rejection did not wait for the in-flight submission"
    );
    sqlx::query(r#"INSERT INTO "Submissions" (participation_id) VALUES ($1)"#)
        .bind(identity.id)
        .execute(&mut *submission)
        .await
        .unwrap();
    submission.commit().await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(2), rejection)
        .await
        .expect("rejection remained blocked after submission commit")
        .expect("rejection task failed")
        .expect_err("accepted solver was rejected after evidence committed");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    assert!(error.to_string().contains("suspend it instead"));
    assert_eq!(
        sqlx::query_as::<_, (i16, Option<i32>)>(
            r#"SELECT status, division_id FROM "Participations" WHERE id = $1"#,
        )
        .bind(identity.id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        (
            ParticipationStatus::Accepted as i16,
            Some(other_division_id)
        )
    );

    // Suspension must remain available even after an official engine boundary;
    // unlike rejection, it is reversible and retains the scoring identity.
    sqlx::query(r#"UPDATE "Games" SET ad_scoring_start_round = 1 WHERE id = $1"#)
        .bind(identity.game_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut sanction = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    persist_participation_status(
        &mut sanction,
        identity,
        ParticipationStatus::Suspended,
        None,
    )
    .await
    .expect("scoring boundary blocked the administrative suspension");
    sanction.release().await.unwrap();

    // Each engine family independently freezes division interpretation, even
    // when its official-start marker is absent (legacy/imported evidence).
    sqlx::query(
        r#"UPDATE "Games"
              SET ad_scoring_start_round = NULL, koth_scoring_start_round = NULL
            WHERE id = $1"#,
    )
    .bind(identity.game_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Participations" SET status = $1 WHERE id = $2"#)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(identity.id)
        .execute(&pool)
        .await
        .unwrap();
    let mut jeopardy_division = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    let error = update_division_only(&mut jeopardy_division, identity, Some(Some(division_id)))
        .await
        .expect_err("Jeopardy evidence allowed a division move");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    jeopardy_division.release().await.unwrap();

    sqlx::query(r#"DELETE FROM "Submissions" WHERE participation_id = $1"#)
        .bind(identity.id)
        .execute(&pool)
        .await
        .unwrap();
    let service_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "AdTeamServices" (participation_id) VALUES ($1) RETURNING id"#,
    )
    .bind(identity.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "AdFlags" (team_service_id) VALUES ($1)"#)
        .bind(service_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut ad_division = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    let error = update_division_only(&mut ad_division, identity, Some(Some(division_id)))
        .await
        .expect_err("A&D evidence allowed a division move");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    ad_division.release().await.unwrap();
    sqlx::query(r#"DELETE FROM "AdFlags""#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"DELETE FROM "AdTeamServices""#)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query(r#"INSERT INTO "KothTokens" (participation_id) VALUES ($1)"#)
        .bind(identity.id)
        .execute(&pool)
        .await
        .unwrap();
    let mut koth_division = ParticipationReviewLease::acquire(&pool, identity.team_id)
        .await
        .unwrap();
    let error = update_division_only(&mut koth_division, identity, Some(Some(division_id)))
        .await
        .expect_err("KotH evidence allowed a division move");
    assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    koth_division.release().await.unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .unwrap();
}
