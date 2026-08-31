use super::UP_SQL;

#[test]
fn terminal_only_sources_are_auto_acknowledged_under_the_queue_lock() {
    assert!(UP_SQL.contains("dirty_source_kind IN (6, 9)"));
    assert!(UP_SQL.contains("queue.final_applied_at_utc IS NOT NULL"));
    assert!(UP_SQL.contains("CASE WHEN terminal_auto_ack THEN 1 ELSE 0 END"));
    assert!(UP_SQL.contains("NOT terminal_auto_ack"));
    assert!(UP_SQL.contains("applied_version = source.dirty_version"));
}

#[tokio::test]
#[ignore = "requires migrated disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn postgres_sealed_source_six_and_nine_writes_do_not_redirty_the_game() {
    use sqlx::postgres::PgPoolOptions;

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let game_id: i32 = sqlx::query_scalar(
        r#"SELECT game_id FROM "AntiCheatReconciliationQueue"
            ORDER BY game_id LIMIT 1"#,
    )
    .fetch_one(&pool)
    .await
    .expect("the disposable database needs one game");
    let mut transaction = pool.begin().await.unwrap();

    sqlx::query(
        r#"UPDATE "SuspicionReconciliationState"
              SET evidence_closed_at_utc = COALESCE(evidence_closed_at_utc, clock_timestamp()),
                  sealed_at_utc = COALESCE(sealed_at_utc, clock_timestamp())
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationQueue"
              SET applied_generation = desired_generation,
                  final_requested_at_utc = COALESCE(final_requested_at_utc, clock_timestamp()),
                  final_applied_at_utc = COALESCE(final_applied_at_utc, clock_timestamp())
            WHERE game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query(
        r#"UPDATE "AntiCheatReconciliationSources"
              SET applied_version = dirty_version
            WHERE game_id = $1 AND source_kind IN (6, 9)"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();
    let generation_before: (i64, i64) = sqlx::query_as(
        r#"SELECT desired_generation, applied_generation
              FROM "AntiCheatReconciliationQueue" WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    let sources_before: Vec<(i16, i64)> = sqlx::query_as(
        r#"SELECT source_kind, dirty_version
              FROM "AntiCheatReconciliationSources"
             WHERE game_id = $1 AND source_kind IN (6, 9)
             ORDER BY source_kind"#,
    )
    .bind(game_id)
    .fetch_all(&mut *transaction)
    .await
    .unwrap();

    sqlx::raw_sql(
        r#"CREATE TEMP TABLE terminal_source6_probe (
               game_id INTEGER NOT NULL,
               reconciliation_version BIGINT NULL
           ) ON COMMIT DROP;
           CREATE TRIGGER terminal_source6_stamp
           BEFORE INSERT ON terminal_source6_probe
           FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_insert('6');
           CREATE TEMP TABLE terminal_source9_probe (
               game_id INTEGER NOT NULL,
               status SMALLINT NOT NULL,
               competitive_admitted_at_utc TIMESTAMPTZ NULL,
               reconciliation_version BIGINT NULL
           ) ON COMMIT DROP;
           CREATE TRIGGER terminal_source9_stamp
           BEFORE INSERT ON terminal_source9_probe
           FOR EACH ROW EXECUTE FUNCTION rsctf_stamp_anticheat_participation();"#,
    )
    .execute(&mut *transaction)
    .await
    .unwrap();
    sqlx::query("INSERT INTO terminal_source6_probe (game_id) VALUES ($1)")
        .bind(game_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
    sqlx::query("INSERT INTO terminal_source9_probe (game_id, status) VALUES ($1, 1)")
        .bind(game_id)
        .execute(&mut *transaction)
        .await
        .unwrap();

    let generation_after: (i64, i64) = sqlx::query_as(
        r#"SELECT desired_generation, applied_generation
              FROM "AntiCheatReconciliationQueue" WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    let sources_after: Vec<(i16, i64, i64)> = sqlx::query_as(
        r#"SELECT source_kind, dirty_version, applied_version
              FROM "AntiCheatReconciliationSources"
             WHERE game_id = $1 AND source_kind IN (6, 9)
             ORDER BY source_kind"#,
    )
    .bind(game_id)
    .fetch_all(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(generation_before, generation_after);
    assert_eq!(generation_after.0, generation_after.1);
    for ((kind, before), (after_kind, dirty, applied)) in
        sources_before.into_iter().zip(sources_after)
    {
        assert_eq!(kind, after_kind);
        assert_eq!(dirty, before + 1);
        assert_eq!(applied, dirty);
    }

    transaction.rollback().await.unwrap();
    pool.close().await;
}
