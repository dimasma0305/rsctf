use std::str::FromStr;

use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::*;

#[test]
fn retry_delay_is_bounded_for_poison_jobs() {
    for attempts in 1_i32..10_000 {
        let exponent = u32::try_from(attempts.clamp(1, 11)).unwrap_or(11);
        let seconds = 1_i64.checked_shl(exponent).unwrap_or(3600).min(3600);
        assert!((2..=3600).contains(&seconds));
    }
}

#[test]
fn direct_payload_must_be_an_object() {
    assert!(payload_is_object(&serde_json::json!({"bait": "/.env"})).is_ok());
    assert!(payload_is_object(&serde_json::json!(["/.env"])).is_err());
}

#[test]
fn reconciliation_interval_is_validated() {
    assert_eq!(parse_reconcile_seconds(None).unwrap(), 30);
    assert_eq!(parse_reconcile_seconds(Some("1")).unwrap(), 1);
    assert_eq!(parse_reconcile_seconds(Some("3600")).unwrap(), 3600);
    assert!(parse_reconcile_seconds(Some("0")).is_err());
    assert!(parse_reconcile_seconds(Some("3601")).is_err());
    assert!(parse_reconcile_seconds(Some("not-a-number")).is_err());
    assert_eq!(parse_finalize_grace_seconds(None).unwrap(), 360);
    assert_eq!(parse_finalize_grace_seconds(Some("1")).unwrap(), 1);
    assert_eq!(parse_finalize_grace_seconds(Some("3600")).unwrap(), 3600);
    assert!(parse_finalize_grace_seconds(Some("0")).is_err());
    assert!(parse_finalize_grace_seconds(Some("3601")).is_err());
    assert!(parse_finalize_grace_seconds(Some("not-a-number")).is_err());
}

#[test]
fn games_share_the_configured_competitive_end_and_delayed_final_seal() {
    assert!(RECONCILE_GAMES_SQL.contains("WITH observed_clock AS MATERIALIZED"));
    assert!(RECONCILE_GAMES_SQL.contains("game.end_time_utc > observed_clock.db_now"));
    assert!(RECONCILE_GAMES_SQL.contains("<= observed_clock.db_now"));
    assert!(RECONCILE_GAMES_SQL.contains("reconciliation.sealed_at_utc IS NULL"));
    assert!(!RECONCILE_GAMES_SQL.contains("practice_mode"));
    assert!(!RECONCILE_GAMES_SQL.contains("Utc::now"));
    assert!((2..32).contains(&RECONCILE_GAME_CONCURRENCY));
    assert!(RECONCILE_PASS_DEADLINE > GAME_RECONCILE_DEADLINE);
}

#[test]
fn live_reconciliation_uses_incremental_submission_jobs() {
    let source = include_str!("outbox.rs");
    let watermarks = include_str!("outbox/watermarks.rs");
    assert!(source.contains("ReconciliationSnapshot::BarrierBackedFinal"));
    assert!(source.contains("durable per-submission outbox"));
    assert!(source.contains("buffer_unordered(RECONCILE_GAME_CONCURRENCY)"));
    assert!(watermarks.contains("SuspicionReconciliationWatermarks"));
    assert!(watermarks.contains("ORDER BY id LIMIT $7"));
    assert!(watermarks.contains("RECONCILE_SOURCE_BATCH"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn source_watermarks_bound_large_ledgers_and_multi_replica_claims() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_suspicion_cursor_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
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
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
        CREATE TABLE "SuspicionReconciliationState" (
          game_id INTEGER PRIMARY KEY REFERENCES "Games"(id),
          dirty_generation BIGINT NOT NULL DEFAULT 1,
          completed_generation BIGINT NOT NULL DEFAULT 0,
          dirty_mask BIGINT NOT NULL DEFAULT 63,
          lease_token UUID,
          lease_expires_at_utc TIMESTAMPTZ
        );
        CREATE TABLE "SuspicionReconciliationWatermarks" (
          game_id INTEGER PRIMARY KEY REFERENCES "Games"(id),
          identity_observation_id BIGINT NOT NULL DEFAULT 0,
          dns_revision BIGINT NOT NULL DEFAULT 0,
          network_revision BIGINT NOT NULL DEFAULT 0,
          flag_transport_id BIGINT NOT NULL DEFAULT 0,
          cheat_info_id BIGINT NOT NULL DEFAULT 0,
          updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
        );
        CREATE TABLE "IdentityObservations" (id BIGINT PRIMARY KEY, game_id INTEGER);
        CREATE TABLE "VpnDnsProviderBuckets" (
          id BIGINT PRIMARY KEY, game_id INTEGER, reconcile_revision BIGINT
        );
        CREATE TABLE "VpnPeerNetworkObservations" (
          id BIGINT PRIMARY KEY, game_id INTEGER, reconcile_revision BIGINT
        );
        CREATE TABLE "VpnFlagTransportEvents" (id BIGINT PRIMARY KEY, game_id INTEGER);
        CREATE TABLE "CheatInfo" (id BIGINT PRIMARY KEY, game_id INTEGER);
        INSERT INTO "Games" VALUES (7);
        INSERT INTO "SuspicionReconciliationState" (game_id) VALUES (7);
        INSERT INTO "IdentityObservations" SELECT item, 7 FROM generate_series(1, 600) item;
        INSERT INTO "VpnDnsProviderBuckets"
          SELECT item, 7, item FROM generate_series(1, 700) item;
        INSERT INTO "VpnPeerNetworkObservations"
          SELECT item, 7, item FROM generate_series(1, 650) item;
        INSERT INTO "VpnFlagTransportEvents"
          SELECT item, 7 FROM generate_series(1, 550) item;
        INSERT INTO "CheatInfo" SELECT item, 7 FROM generate_series(1, 575) item;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let (left, right) = tokio::join!(
        claim_game_reconciliation(&pool, 7, false),
        claim_game_reconciliation(&pool, 7, false),
    );
    let claims = [left.unwrap(), right.unwrap()];
    assert_eq!(claims.iter().filter(|claim| claim.is_some()).count(), 1);
    let first = claims.into_iter().flatten().next().unwrap().sources;
    assert_eq!(
        first.through.identity_observation_id,
        RECONCILE_SOURCE_BATCH
    );
    assert_eq!(first.through.dns_revision, RECONCILE_SOURCE_BATCH);
    assert_eq!(first.backlog_mask, DIRTY_CORRELATION | DIRTY_EVENT_SECURITY);

    sqlx::query(
        r#"UPDATE "SuspicionReconciliationWatermarks"
              SET identity_observation_id = $2, dns_revision = $3,
                  network_revision = $4, flag_transport_id = $5,
                  cheat_info_id = $6 WHERE game_id = $1"#,
    )
    .bind(7_i32)
    .bind(first.through.identity_observation_id)
    .bind(first.through.dns_revision)
    .bind(first.through.network_revision)
    .bind(first.through.flag_transport_id)
    .bind(first.through.cheat_info_id)
    .execute(&pool)
    .await
    .unwrap();
    let second = capture_source_window(&pool, 7, false).await.unwrap();
    assert_eq!(second.through.identity_observation_id, 600);
    assert_eq!(second.through.dns_revision, 700);
    assert_eq!(second.backlog_mask, 0);
    sqlx::query(
        r#"UPDATE "SuspicionReconciliationWatermarks"
              SET identity_observation_id = $2, dns_revision = $3,
                  network_revision = $4, flag_transport_id = $5,
                  cheat_info_id = $6 WHERE game_id = $1"#,
    )
    .bind(7_i32)
    .bind(second.through.identity_observation_id)
    .bind(second.through.dns_revision)
    .bind(second.through.network_revision)
    .bind(second.through.flag_transport_id)
    .bind(second.through.cheat_info_id)
    .execute(&pool)
    .await
    .unwrap();
    let idle = capture_source_window(&pool, 7, false).await.unwrap();
    assert_eq!(idle.after, idle.through);
    assert_eq!(idle.backlog_mask, 0);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[test]
fn direct_source_validation_is_bounded_to_the_competitive_window() {
    let source = include_str!("outbox.rs");
    assert!(!source.contains("EvaluationSourceKind::HoneypotHit"));
    assert!(source.contains("access.connected_at_utc >= game.start_time_utc"));
    assert!(source.contains("access.connected_at_utc < game.end_time_utc"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn scheduler_closes_intake_after_grace_and_waits_for_every_job() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_suspicion_final_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
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
          start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "SuspicionReconciliationState" (
          game_id INTEGER PRIMARY KEY REFERENCES "Games"(id),
          evidence_closed_at_utc TIMESTAMPTZ,
          last_reconciled_at_utc TIMESTAMPTZ,
          sealed_at_utc TIMESTAMPTZ,
          attempts INTEGER NOT NULL DEFAULT 0,
          last_error TEXT,
          dirty_generation BIGINT NOT NULL DEFAULT 1,
          completed_generation BIGINT NOT NULL DEFAULT 0,
          dirty_mask BIGINT NOT NULL DEFAULT 63,
          lease_token UUID,
          lease_expires_at_utc TIMESTAMPTZ,
          CHECK (sealed_at_utc IS NULL OR evidence_closed_at_utc IS NOT NULL)
        );
        CREATE TABLE "SuspicionReconciliationOperations" (
          game_id INTEGER NOT NULL,
          generation BIGINT NOT NULL,
          status SMALLINT NOT NULL DEFAULT 0,
          inserted_count INTEGER,
          completed_at_utc TIMESTAMPTZ
        );
        CREATE TABLE "SuspicionReconciliationWatermarks" (
          game_id INTEGER PRIMARY KEY REFERENCES "Games"(id),
          identity_observation_id BIGINT NOT NULL DEFAULT 0,
          dns_revision BIGINT NOT NULL DEFAULT 0,
          network_revision BIGINT NOT NULL DEFAULT 0,
          flag_transport_id BIGINT NOT NULL DEFAULT 0,
          cheat_info_id BIGINT NOT NULL DEFAULT 0,
          updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
        );
        CREATE TABLE "IdentityObservations" (id BIGINT PRIMARY KEY, game_id INTEGER);
        CREATE TABLE "VpnDnsProviderBuckets" (
          id BIGINT PRIMARY KEY, game_id INTEGER, reconcile_revision BIGINT
        );
        CREATE TABLE "VpnPeerNetworkObservations" (
          id BIGINT PRIMARY KEY, game_id INTEGER, reconcile_revision BIGINT
        );
        CREATE TABLE "VpnFlagTransportEvents" (id BIGINT PRIMARY KEY, game_id INTEGER);
        CREATE TABLE "CheatInfo" (id BIGINT PRIMARY KEY, game_id INTEGER);
        CREATE TABLE "SuspicionEvaluationOutbox" (
          id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
          game_id INTEGER NOT NULL,
          observed_at_utc TIMESTAMPTZ NOT NULL,
          completed_at_utc TIMESTAMPTZ,
          lease_token UUID,
          lease_expires_at_utc TIMESTAMPTZ
        );
        INSERT INTO "Games" (id, start_time_utc, end_time_utc) VALUES
          (1, clock_timestamp() - INTERVAL '1 hour',
              clock_timestamp() + INTERVAL '1 hour'),
          (2, clock_timestamp() - INTERVAL '1 hour', clock_timestamp()),
          (3, clock_timestamp() - INTERVAL '2 hours',
              clock_timestamp() - INTERVAL '61 seconds'),
          (4, clock_timestamp() - INTERVAL '2 hours',
              clock_timestamp() - INTERVAL '61 seconds'),
          (5, clock_timestamp() - INTERVAL '2 hours',
              clock_timestamp() - INTERVAL '61 seconds');
        INSERT INTO "SuspicionReconciliationState" (game_id)
          SELECT id FROM "Games";
        UPDATE "SuspicionReconciliationState"
           SET evidence_closed_at_utc = clock_timestamp(),
               sealed_at_utc = clock_timestamp(), attempts = 1
         WHERE game_id = 4;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let phases: Vec<(i32, bool)> = sqlx::query_as(RECONCILE_GAMES_SQL)
        .bind(60_i64)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(phases, vec![(1, false), (3, true), (5, true)]);

    // A pre-barrier producer owns Games FOR SHARE and commits more than one
    // worker batch of intents, including a live lease. Closure must wait for
    // that transaction and the final state must remain unsealed until all 33
    // rows complete.
    let mut producer = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 5 FOR SHARE"#)
        .execute(&mut *producer)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "SuspicionEvaluationOutbox"
             (game_id, observed_at_utc, completed_at_utc,
              lease_token, lease_expires_at_utc)
           SELECT 5, game.end_time_utc - INTERVAL '1 minute', NULL,
                  CASE WHEN item = 33 THEN $1 ELSE NULL END,
                  CASE WHEN item = 33
                       THEN clock_timestamp() + INTERVAL '5 minutes'
                       ELSE NULL END
             FROM "Games" game
             CROSS JOIN generate_series(1, 33) item
            WHERE game.id = 5"#,
    )
    .bind(Uuid::new_v4())
    .execute(&mut *producer)
    .await
    .unwrap();
    let barrier_pool = pool.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let barrier = async move {
        started_tx.send(()).unwrap();
        close_competitive_evidence_window(&barrier_pool, 5, 60)
            .await
            .unwrap()
    };
    let finish_producer = async move {
        started_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        producer.commit().await.unwrap();
    };
    let (closed, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(barrier, finish_producer)
    })
    .await
    .expect("final barrier drains the admitted producer");
    assert!(closed);
    assert_eq!(incomplete_competitive_jobs(&pool, 5).await.unwrap(), 33);

    // The closure marker is authoritative even if the configured end moves
    // forward afterward (equivalent to a database-clock rollback).
    sqlx::query(
        r#"UPDATE "Games"
              SET end_time_utc = clock_timestamp() + INTERVAL '1 hour'
            WHERE id = 5"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut intake = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 5 FOR SHARE"#)
        .execute(&mut *intake)
        .await
        .unwrap();
    assert_eq!(
        crate::services::participation_evidence::competitive_evidence_is_open(&mut intake, 5)
            .await
            .unwrap(),
        Some(false)
    );
    intake.rollback().await.unwrap();
    sqlx::query(
        r#"UPDATE "Games"
              SET end_time_utc = clock_timestamp() - INTERVAL '61 seconds'
            WHERE id = 5"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(
        defer_final_for_incomplete_jobs(&pool, 5).await.unwrap(),
        Some(33)
    );
    let deferred: (bool, Option<String>) = sqlx::query_as(
        r#"SELECT sealed_at_utc IS NOT NULL, last_error
             FROM "SuspicionReconciliationState" WHERE game_id = 5"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!deferred.0);
    assert!(deferred
        .1
        .as_deref()
        .is_some_and(|error| error.contains("33 in-window")));

    sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET completed_at_utc = clock_timestamp(),
                  lease_token = NULL, lease_expires_at_utc = NULL
            WHERE game_id = 5"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        defer_final_for_incomplete_jobs(&pool, 5).await.unwrap(),
        None
    );
    let claim = claim_game_reconciliation(&pool, 5, true)
        .await
        .unwrap()
        .expect("final reconciliation state is claimable");
    let mut reconciliation = pool.begin().await.unwrap();
    record_game_reconciliation(&mut reconciliation, 5, &claim, true, 0, &[], DIRTY_ALL)
        .await
        .unwrap();
    reconciliation.commit().await.unwrap();
    let sealed: (bool, bool) = sqlx::query_as(
        r#"SELECT evidence_closed_at_utc IS NOT NULL, sealed_at_utc IS NOT NULL
             FROM "SuspicionReconciliationState" WHERE game_id = 5"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(sealed, (true, true));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn crash_retry_and_historical_replay_are_idempotent() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("rsctf_suspicion_outbox_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
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
          start_time_utc TIMESTAMPTZ NOT NULL,
          end_time_utc TIMESTAMPTZ NOT NULL,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
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
          competitive_admitted_at_utc TIMESTAMPTZ NOT NULL,
          suspicion_score INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
          deletion_pending BOOLEAN NOT NULL DEFAULT FALSE
        );
        CREATE TABLE "ContainerAccessEvents" (
          id INTEGER PRIMARY KEY,
          game_id INTEGER NOT NULL,
          challenge_id INTEGER NOT NULL,
          container_owner_participation_id INTEGER NOT NULL,
          accessing_participation_id INTEGER,
          is_monitor BOOLEAN,
          connected_at_utc TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "SuspicionRules" (
          rule_code TEXT PRIMARY KEY,
          weight INTEGER NOT NULL
        );
        CREATE TABLE "SuspicionEvents" (
          id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
          game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          challenge_id INTEGER,
          kind SMALLINT NOT NULL,
          evidence_key TEXT NOT NULL,
          score_delta INTEGER NOT NULL,
          created_at TIMESTAMPTZ NOT NULL,
          UNIQUE (game_id, participation_id, kind, evidence_key)
        );
        CREATE TABLE "SuspicionEvaluationOutbox" (
          id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
          job_kind SMALLINT NOT NULL,
          source_kind SMALLINT NOT NULL,
          source_id INTEGER NOT NULL,
          game_id INTEGER NOT NULL,
          participation_id INTEGER NOT NULL,
          challenge_id INTEGER,
          rule_kind SMALLINT,
          evidence_key TEXT NOT NULL,
          observed_at_utc TIMESTAMPTZ NOT NULL,
          evidence_payload JSONB NOT NULL,
          evidence_version SMALLINT NOT NULL,
          available_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
          lease_token UUID,
          lease_expires_at_utc TIMESTAMPTZ,
          attempts INTEGER NOT NULL DEFAULT 0,
          completed_at_utc TIMESTAMPTZ,
          last_error TEXT,
          CHECK (
            (job_kind = 0 AND source_kind = 0 AND rule_kind IS NULL
                          AND challenge_id IS NOT NULL)
            OR
            (job_kind = 1 AND source_kind = 2 AND rule_kind = 33
                          AND challenge_id IS NOT NULL)
          )
        );
        CREATE UNIQUE INDEX ux_test_outbox_source
          ON "SuspicionEvaluationOutbox"
             (source_kind, source_id, COALESCE(rule_kind, -1), evidence_key);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(
        r#"
        INSERT INTO "Games" (id, start_time_utc, end_time_utc)
          VALUES (1, clock_timestamp() - INTERVAL '1 hour',
                      clock_timestamp() + INTERVAL '1 hour');
        INSERT INTO "Teams" (id) VALUES (10);
        INSERT INTO "Participations"
          (id, game_id, team_id, status, competitive_admitted_at_utc)
          VALUES (20, 1, 10, 1, clock_timestamp() - INTERVAL '1 hour');
        INSERT INTO "GameChallenges" (id, game_id) VALUES (30, 1);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    let observed_at: DateTime<Utc> = sqlx::query_scalar(
        r#"INSERT INTO "ContainerAccessEvents"
             (id, game_id, challenge_id, container_owner_participation_id,
              accessing_participation_id, is_monitor, connected_at_utc)
           VALUES (99, 1, 30, 21, 20, FALSE, $1)
        RETURNING connected_at_utc"#,
    )
    .bind(Utc::now() - chrono::Duration::minutes(5))
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert!(enqueue_direct_suspicion_evaluation(
        &mut transaction,
        EvaluationSourceKind::ContainerAccess,
        99,
        1,
        20,
        Some(30),
        SuspicionType::CrossTeamContainerAccess,
        "challenge:30",
        observed_at,
        serde_json::json!({"containerId": "00000000-0000-0000-0000-000000000001"}),
    )
    .await
    .unwrap());
    assert!(!enqueue_direct_suspicion_evaluation(
        &mut transaction,
        EvaluationSourceKind::ContainerAccess,
        99,
        1,
        20,
        Some(30),
        SuspicionType::CrossTeamContainerAccess,
        "challenge:30",
        observed_at,
        serde_json::json!({"containerId": "00000000-0000-0000-0000-000000000001"}),
    )
    .await
    .unwrap());
    for telemetry_only in [
        SuspicionType::HoneypotHit,
        SuspicionType::HoneypotProtocolHit,
        SuspicionType::HoneypotChain,
    ] {
        let error = enqueue_direct_suspicion_evaluation(
            &mut transaction,
            EvaluationSourceKind::ContainerAccess,
            200 + i32::from(telemetry_only.kind()),
            1,
            20,
            Some(30),
            telemetry_only,
            "telemetry-only",
            observed_at,
            serde_json::json!({}),
        )
        .await
        .expect_err("raw-only honeypot kinds cannot enter the durable score queue");
        assert!(error.to_string().contains("container access provenance"));
    }
    transaction.commit().await.unwrap();

    // Simulate a process dying after claim. A live lease is skipped; expiry
    // makes the exact same durable job available to another control replica.
    sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET lease_token = $1,
                  lease_expires_at_utc = clock_timestamp() + INTERVAL '5 minutes'"#,
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .unwrap();
    let db = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    assert_eq!(reconcile_evaluation_outbox(&db, 8).await.unwrap(), 0);
    sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET lease_expires_at_utc = clock_timestamp() - INTERVAL '1 second'"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(reconcile_evaluation_outbox(&db, 8).await.unwrap(), 1);
    let first_score: i32 =
        sqlx::query_scalar(r#"SELECT suspicion_score FROM "Participations" WHERE id = 20"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(first_score > 0);

    // Administrative state changes do not erase or block historical replay.
    sqlx::raw_sql(
        r#"
        UPDATE "Participations" SET status = 3 WHERE id = 20;
        UPDATE "GameChallenges" SET is_enabled = FALSE WHERE id = 30;
        UPDATE "SuspicionEvaluationOutbox"
           SET completed_at_utc = NULL,
               available_at_utc = clock_timestamp(),
               lease_token = NULL,
               lease_expires_at_utc = NULL;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(reconcile_evaluation_outbox(&db, 8).await.unwrap(), 1);
    let (events, score): (i64, i32) = sqlx::query_as(
        r#"SELECT COUNT(*)::bigint, MAX(participation.suspicion_score)
             FROM "SuspicionEvents" event
             JOIN "Participations" participation
               ON participation.id = event.participation_id"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
    assert_eq!(score, first_score);
    let created_at: DateTime<Utc> =
        sqlx::query_scalar(r#"SELECT created_at FROM "SuspicionEvents" LIMIT 1"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(created_at, observed_at);

    // A container job without its exact raw source is retained as a
    // poison job with diagnostic state; it can never manufacture score.
    let mut transaction = pool.begin().await.unwrap();
    assert!(enqueue_direct_suspicion_evaluation(
        &mut transaction,
        EvaluationSourceKind::ContainerAccess,
        100,
        1,
        20,
        Some(30),
        SuspicionType::CrossTeamContainerAccess,
        "missing-source",
        observed_at,
        serde_json::json!({"containerId": "00000000-0000-0000-0000-000000000100"}),
    )
    .await
    .unwrap());
    transaction.commit().await.unwrap();
    assert_eq!(reconcile_evaluation_outbox(&db, 8).await.unwrap(), 1);
    let (attempts, completed, last_error): (i32, bool, Option<String>) = sqlx::query_as(
        r#"SELECT attempts, completed_at_utc IS NOT NULL, last_error
             FROM "SuspicionEvaluationOutbox"
            WHERE source_id = 100"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(attempts, 1);
    assert!(!completed);
    assert!(last_error
        .as_deref()
        .is_some_and(|error| error.contains("source provenance")));
    sqlx::query(
        r#"UPDATE "SuspicionEvaluationOutbox"
              SET available_at_utc = clock_timestamp() + INTERVAL '1 hour'
            WHERE source_id = 100"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    // An exact durable source outside the competitive [start,end) interval is
    // a valid post-end/practice observation, not a poison job. Complete it as
    // a no-op while retaining the raw row and emitting no suspicion event.
    let exact_end: DateTime<Utc> =
        sqlx::query_scalar(r#"SELECT end_time_utc FROM "Games" WHERE id = 1"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        r#"INSERT INTO "ContainerAccessEvents"
             (id, game_id, challenge_id, container_owner_participation_id,
              accessing_participation_id, is_monitor, connected_at_utc)
           VALUES (101, 1, 30, 21, 20, FALSE, $1)"#,
    )
    .bind(exact_end)
    .execute(&mut *transaction)
    .await
    .unwrap();
    assert!(enqueue_direct_suspicion_evaluation(
        &mut transaction,
        EvaluationSourceKind::ContainerAccess,
        101,
        1,
        20,
        Some(30),
        SuspicionType::CrossTeamContainerAccess,
        "post-end-source",
        exact_end,
        serde_json::json!({"containerId": "00000000-0000-0000-0000-000000000101"}),
    )
    .await
    .unwrap());
    transaction.commit().await.unwrap();
    assert_eq!(reconcile_evaluation_outbox(&db, 8).await.unwrap(), 1);
    let outside_job: (bool, Option<String>) = sqlx::query_as(
        r#"SELECT completed_at_utc IS NOT NULL, last_error
             FROM "SuspicionEvaluationOutbox" WHERE source_id = 101"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outside_job, (true, None));
    let outside_events: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "SuspicionEvents"
            WHERE evidence_key = 'post-end-source'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outside_events, 0);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
