use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::{SuspicionType, INSERT_SUSPICION_EVENT_SQL};

pub(super) async fn assert_detector_replay_does_not_redirty_reconciliation() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("detector_replay_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
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
        r#"CREATE TABLE "Participations" (
               id INTEGER PRIMARY KEY,
               game_id INTEGER NOT NULL,
               competitive_admitted_at_utc TIMESTAMPTZ NOT NULL
           );
           CREATE TABLE "SuspicionEvents" (
               id BIGSERIAL PRIMARY KEY,
               game_id INTEGER NOT NULL,
               participation_id INTEGER NOT NULL,
               challenge_id INTEGER NULL,
               kind SMALLINT NOT NULL,
               evidence_key TEXT NOT NULL,
               score_delta INTEGER NOT NULL,
               created_at TIMESTAMPTZ NOT NULL,
               reconciliation_version BIGINT NOT NULL
           );
           CREATE UNIQUE INDEX ux_test_suspicion_incident
             ON "SuspicionEvents"
                (game_id, participation_id, kind, evidence_key);
           CREATE TABLE "SuspicionReconciliationState" (
               game_id INTEGER PRIMARY KEY
           );
           CREATE TABLE "AntiCheatReconciliationSources" (
               game_id INTEGER NOT NULL,
               source_kind SMALLINT NOT NULL,
               applied_version BIGINT NOT NULL,
               dirty_version BIGINT NOT NULL,
               PRIMARY KEY (game_id, source_kind)
           );
           CREATE TABLE "AntiCheatReconciliationQueue" (
               game_id INTEGER PRIMARY KEY,
               applied_generation BIGINT NOT NULL,
               desired_generation BIGINT NOT NULL
           );
           INSERT INTO "Participations"
               (id, game_id, competitive_admitted_at_utc)
               VALUES (9, 7, clock_timestamp() - INTERVAL '1 hour');
           INSERT INTO "SuspicionReconciliationState" (game_id) VALUES (7);
           INSERT INTO "AntiCheatReconciliationSources"
               (game_id, source_kind, applied_version, dirty_version)
               VALUES (7, 7, 1, 1);
           INSERT INTO "AntiCheatReconciliationQueue"
               (game_id, applied_generation, desired_generation)
               VALUES (7, 1, 1);
           CREATE FUNCTION test_stamp_suspicion_insert()
           RETURNS trigger LANGUAGE plpgsql AS $$
           DECLARE stamped_version BIGINT;
           BEGIN
               IF NEW.reconciliation_version IS NOT NULL THEN
                   RAISE EXCEPTION 'reconciliation version is database-owned';
               END IF;
               UPDATE "AntiCheatReconciliationSources"
                  SET dirty_version = dirty_version + 1
                WHERE game_id = NEW.game_id AND source_kind = 7
               RETURNING dirty_version INTO stamped_version;
               UPDATE "AntiCheatReconciliationQueue"
                  SET desired_generation = desired_generation + 1
                WHERE game_id = NEW.game_id;
               NEW.reconciliation_version := stamped_version;
               RETURN NEW;
           END
           $$;
           CREATE TRIGGER zz_test_suspicion_anticheat_stamp
             BEFORE INSERT ON "SuspicionEvents"
             FOR EACH ROW EXECUTE FUNCTION test_stamp_suspicion_insert();"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let game_id = 7;
    let participation_id = 9;
    let evidence_key = format!("reconciliation-replay:{}", uuid::Uuid::new_v4().simple());
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        r#"SELECT 1 FROM "SuspicionReconciliationState"
            WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"SELECT 1 FROM "AntiCheatReconciliationQueue"
            WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();

    let first: (bool, bool) = sqlx::query_as(INSERT_SUSPICION_EVENT_SQL)
        .bind(game_id)
        .bind(participation_id)
        .bind(None::<i32>)
        .bind(SuspicionType::SharedFingerprint.kind())
        .bind(&evidence_key)
        .bind(1_i32)
        .bind(chrono::Utc::now())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(first, (true, true));
    let dirty_after_first: (i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT source.applied_version, source.dirty_version,
                  queue.applied_generation, queue.desired_generation
             FROM "AntiCheatReconciliationSources" source
             JOIN "AntiCheatReconciliationQueue" queue
               ON queue.game_id = source.game_id
            WHERE source.game_id = $1 AND source.source_kind = 7"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(dirty_after_first.1, dirty_after_first.0 + 1);
    assert_eq!(dirty_after_first.3, dirty_after_first.2 + 1);

    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationSources"
              SET applied_version = dirty_version
            WHERE game_id = $1 AND source_kind = 7"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET applied_generation = desired_generation WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    let clean_after_first: (i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT source.applied_version, source.dirty_version,
                  queue.applied_generation, queue.desired_generation
             FROM "AntiCheatReconciliationSources" source
             JOIN "AntiCheatReconciliationQueue" queue
               ON queue.game_id = source.game_id
            WHERE source.game_id = $1 AND source.source_kind = 7"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(clean_after_first.0, clean_after_first.1);
    assert_eq!(clean_after_first.2, clean_after_first.3);

    let second: (bool, bool) = sqlx::query_as(INSERT_SUSPICION_EVENT_SQL)
        .bind(game_id)
        .bind(participation_id)
        .bind(None::<i32>)
        .bind(SuspicionType::SharedFingerprint.kind())
        .bind(&evidence_key)
        .bind(1_i32)
        .bind(chrono::Utc::now())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(second, (true, false));
    let clean_after_replay: (i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT source.applied_version, source.dirty_version,
                  queue.applied_generation, queue.desired_generation
             FROM "AntiCheatReconciliationSources" source
             JOIN "AntiCheatReconciliationQueue" queue
               ON queue.game_id = source.game_id
            WHERE source.game_id = $1 AND source.source_kind = 7"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(clean_after_replay, clean_after_first);
    transaction.rollback().await.unwrap();
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
