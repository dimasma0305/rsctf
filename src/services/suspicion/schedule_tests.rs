use super::*;
use crate::services::suspicion::record_schedule_change;
use chrono::{Duration as ChronoDuration, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn schedule_reopens_sealed_evidence_and_fences_old_finalizers() {
    let url = std::env::var("RSCTF_TEST_DATABASE_URL").unwrap();
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("schedule_reopen_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(
            PgConnectOptions::from_str(&url)
                .unwrap()
                .options([("search_path", schema.as_str())]),
        )
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY, start_time_utc TIMESTAMPTZ,
            end_time_utc TIMESTAMPTZ, deletion_pending BOOLEAN DEFAULT FALSE);
        CREATE TABLE "SuspicionReconciliationState" (game_id INTEGER PRIMARY KEY,
            evidence_closed_at_utc TIMESTAMPTZ, sealed_at_utc TIMESTAMPTZ,
            last_reconciled_at_utc TIMESTAMPTZ, attempts INTEGER DEFAULT 0, last_error TEXT);
        CREATE TABLE "AntiCheatReconciliationQueue" (game_id INTEGER PRIMARY KEY,
            desired_generation BIGINT, applied_generation BIGINT,
            final_requested_at_utc TIMESTAMPTZ, final_applied_at_utc TIMESTAMPTZ,
            available_at_utc TIMESTAMPTZ, lease_token UUID, lease_expires_at_utc TIMESTAMPTZ,
            attempts INTEGER DEFAULT 0, last_started_at_utc TIMESTAMPTZ,
            last_completed_at_utc TIMESTAMPTZ, last_error TEXT, updated_at_utc TIMESTAMPTZ);
        CREATE TABLE "AntiCheatReconciliationSources" (game_id INTEGER, source_kind SMALLINT,
            dirty_version BIGINT, applied_version BIGINT, applied_at_utc TIMESTAMPTZ,
            PRIMARY KEY (game_id,source_kind));
        CREATE TABLE "SuspicionEvaluationOutbox" (id BIGINT PRIMARY KEY, game_id INTEGER,
            completed_at_utc TIMESTAMPTZ, observed_at_utc TIMESTAMPTZ,
            reconciliation_version BIGINT, source_kind SMALLINT, source_id INTEGER);
        CREATE TABLE "SuspicionEvents" (id BIGINT PRIMARY KEY, evidence JSONB);
        INSERT INTO "Games" VALUES (1,now()-interval '2 hours',now()-interval '1 hour',FALSE);
        INSERT INTO "SuspicionReconciliationState" VALUES (1,now(),now(),now(),1,NULL);
        INSERT INTO "AntiCheatReconciliationQueue"
            (game_id,desired_generation,applied_generation,final_requested_at_utc,
             final_applied_at_utc,available_at_utc,updated_at_utc)
            VALUES (1,2,2,now(),now(),now(),now());
        INSERT INTO "AntiCheatReconciliationSources" VALUES (1,0,2,2,now());
        INSERT INTO "SuspicionEvaluationOutbox" VALUES
            (1,1,now(),now()-interval '90 minutes',2,0,1),
            (2,1,now(),now()-interval '30 minutes',1,0,2);
        INSERT INTO "SuspicionEvents" VALUES (1,'{"original":true}');
    "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0359_game_schedule_changes::UP_SQL)
            .execute(&pool)
            .await
            .unwrap();
    }
    let previous: (chrono::DateTime<Utc>, chrono::DateTime<Utc>) =
        sqlx::query_as(r#"SELECT start_time_utc,end_time_utc FROM "Games" WHERE id=1"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    let requested = (previous.0, Utc::now() + ChronoDuration::hours(1));
    let actor = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let mut edit = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id=1 FOR UPDATE"#)
        .execute(&mut *edit)
        .await
        .unwrap();
    record_schedule_change(&mut edit, 1, actor, operation, 2, previous, requested)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET end_time_utc=$1 WHERE id=1"#)
        .bind(requested.1)
        .execute(&mut *edit)
        .await
        .unwrap();
    edit.commit().await.unwrap();
    let state: (bool, bool, i64, i64) = sqlx::query_as(
        r#"
        SELECT state.evidence_closed_at_utc IS NULL AND state.sealed_at_utc IS NULL,
               queue.final_requested_at_utc IS NULL AND queue.final_applied_at_utc IS NULL,
               queue.desired_generation,source.applied_version
        FROM "SuspicionReconciliationState" state
        JOIN "AntiCheatReconciliationQueue" queue USING(game_id)
        JOIN "AntiCheatReconciliationSources" source USING(game_id) WHERE game_id=1
    "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, (true, true, 3, 0));
    let audit: (Uuid, bool, bool) = sqlx::query_as(
        r#"
        SELECT actor_user_id,previous_evidence_closed_at IS NOT NULL,
               previous_sealed_at IS NOT NULL FROM "GameScheduleChanges" WHERE operation_id=$1
    "#,
    )
    .bind(operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit, (actor, true, true));
    let replay = super::super::schedule::load_replay_submissions(
        &pool,
        1,
        SourceCursor {
            kind: 0,
            after: 0,
            through: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        replay,
        vec![2],
        "only the old practice interval needs submission replay"
    );
    assert!(super::super::schedule::load_replay_submissions(
        &pool,
        1,
        SourceCursor {
            kind: 0,
            after: 1,
            through: 2
        },
    )
    .await
    .unwrap()
    .is_empty());
    request_final_if_ready(&pool, 1, 0).await.unwrap();
    let claim = claim_reconciliation(&pool, 1).await.unwrap().unwrap();
    assert!(!claim.final_snapshot);
    assert_eq!(
        claim.cursors,
        vec![SourceCursor {
            kind: 0,
            after: 0,
            through: 2
        }]
    );

    let next = (requested.0, requested.1 + ChronoDuration::hours(1));
    let mut edit = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id=1 FOR UPDATE"#)
        .execute(&mut *edit)
        .await
        .unwrap();
    let rejected =
        record_schedule_change(&mut edit, 1, actor, Uuid::new_v4(), 3, requested, next).await;
    assert!(matches!(rejected, Err(AppError::Conflict(_))));
    edit.rollback().await.unwrap();

    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
        SET lease_expires_at_utc=now()-interval '1 second' WHERE game_id=1"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut edit = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id=1 FOR UPDATE"#)
        .execute(&mut *edit)
        .await
        .unwrap();
    record_schedule_change(&mut edit, 1, actor, Uuid::new_v4(), 3, requested, next)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET end_time_utc=$1 WHERE id=1"#)
        .bind(next.1)
        .execute(&mut *edit)
        .await
        .unwrap();
    let queued_pool = pool.clone();
    let queued_claim = tokio::spawn(async move { claim_reconciliation(&queued_pool, 1).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !queued_claim.is_finished(),
        "claim must wait for the schedule transaction"
    );
    edit.commit().await.unwrap();
    let new_claim = queued_claim.await.unwrap().unwrap().unwrap();
    assert_ne!(new_claim.lease_token, claim.lease_token);
    assert!(!new_claim.final_snapshot);
    assert!(
        finish_success(&pool, &claim).await.is_err(),
        "old pass cannot reseal reopened game"
    );
    finish_success(&pool, &new_claim).await.unwrap();

    sqlx::query(r#"UPDATE "Games" SET end_time_utc=now()-interval '1 second' WHERE id=1"#)
        .execute(&pool)
        .await
        .unwrap();
    request_final_if_ready(&pool, 1, 0).await.unwrap();
    let final_claim = claim_reconciliation(&pool, 1).await.unwrap().unwrap();
    assert!(final_claim.final_snapshot);
    finish_success(&pool, &final_claim).await.unwrap();
    let sealed: bool = sqlx::query_scalar(
        r#"SELECT sealed_at_utc IS NOT NULL
        FROM "SuspicionReconciliationState" WHERE game_id=1"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(sealed);
    let preserved: (bool, bool, i64) = sqlx::query_as(
        r#"
        SELECT (SELECT evidence='{"original":true}'::jsonb FROM "SuspicionEvents" WHERE id=1),
               (SELECT completed_at_utc IS NOT NULL AND reconciliation_version=2
                  FROM "SuspicionEvaluationOutbox" WHERE id=1),
               (SELECT count(*) FROM "GameScheduleChanges" WHERE game_id=1)
    "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(preserved, (true, true, 2));
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
