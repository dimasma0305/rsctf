use super::purge_expired;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

async fn fixture() -> (sqlx::PgPool, sqlx::PgPool, String) {
    let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("requires disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("retention_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(r#"
        CREATE TABLE "MutationOperations" (id INTEGER PRIMARY KEY, expires_at_utc TIMESTAMPTZ NOT NULL);
        CREATE INDEX ON "MutationOperations" (expires_at_utc);
        CREATE TABLE "AccountLinkAttempts" (token_digest BYTEA PRIMARY KEY, expires_at_utc TIMESTAMPTZ NOT NULL);
        CREATE INDEX ON "AccountLinkAttempts" (expires_at_utc);
        INSERT INTO "MutationOperations" SELECT n, clock_timestamp()-INTERVAL '1 hour' FROM generate_series(1,1000) n;
        INSERT INTO "AccountLinkAttempts" SELECT decode(lpad(to_hex(n),8,'0'),'hex'), clock_timestamp()-INTERVAL '1 hour' FROM generate_series(1,1000) n;
        INSERT INTO "MutationOperations" SELECT n, clock_timestamp()+INTERVAL '1 hour' FROM generate_series(1001,1003) n;
        INSERT INTO "AccountLinkAttempts" SELECT decode(lpad(to_hex(n),8,'0'),'hex'), clock_timestamp()+INTERVAL '1 hour' FROM generate_series(1001,1003) n;
    "#).execute(&pool).await.unwrap();
    (admin, pool, schema)
}

async fn finish(admin: sqlx::PgPool, pool: sqlx::PgPool, schema: String) {
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn catch_up_is_bounded_skips_locked_rows_and_preserves_live_records() {
    let (admin, pool, schema) = fixture().await;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "MutationOperations" WHERE id=1 FOR UPDATE"#)
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let first = purge_expired(&pool).await.unwrap();
    assert_eq!(first.removed_mutations, 512);
    assert_eq!(first.removed_links, 512);
    assert_eq!(first.delete_statements, 16);
    assert!(first.oldest_expired_seconds >= 3600);
    let locked: bool =
        sqlx::query_scalar(r#"SELECT EXISTS(SELECT 1 FROM "MutationOperations" WHERE id=1)"#)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(locked);
    lock.rollback().await.unwrap();
    let second = purge_expired(&pool).await.unwrap();
    assert_eq!(second.removed_mutations, 488);
    assert_eq!(second.removed_links, 488);
    assert_eq!(second.oldest_expired_seconds, 0);
    for query in [
        r#"SELECT COUNT(*) FROM "MutationOperations""#,
        r#"SELECT COUNT(*) FROM "AccountLinkAttempts""#,
    ] {
        let count: i64 = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 3);
    }
    let empty = purge_expired(&pool).await.unwrap();
    assert_eq!(empty.removed_mutations + empty.removed_links, 0);
    assert_eq!(empty.delete_statements, 2);
    finish(admin, pool, schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn blocked_maintenance_rolls_back_and_leaves_the_pool_usable() {
    let (admin, pool, schema) = fixture().await;
    let mut lock = pool.begin().await.unwrap();
    sqlx::query(r#"LOCK TABLE "AccountLinkAttempts" IN ACCESS EXCLUSIVE MODE"#)
        .execute(&mut *lock)
        .await
        .unwrap();
    assert!(purge_expired(&pool).await.is_err());
    lock.rollback().await.unwrap();
    let count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "MutationOperations""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1003,
        "failed pass must roll back its earlier deletes"
    );
    let next = purge_expired(&pool).await.unwrap();
    assert_eq!(next.removed_mutations, 512);
    finish(admin, pool, schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn expiration_cutoff_bounds_the_index_scan_before_live_rows() {
    let (admin, pool, schema) = fixture().await;
    sqlx::raw_sql(r#"
        DELETE FROM "MutationOperations" WHERE id > 7 AND id <= 1000;
        INSERT INTO "MutationOperations" SELECT n, clock_timestamp()+INTERVAL '1 day' FROM generate_series(1004,51003) n;
        INSERT INTO "AccountLinkAttempts" SELECT decode(lpad(to_hex(n),8,'0'),'hex'), clock_timestamp()+INTERVAL '1 day' FROM generate_series(1004,51003) n;
        ANALYZE "MutationOperations"; ANALYZE "AccountLinkAttempts";
    "#).execute(&pool).await.unwrap();
    for delete in super::cleanup::DELETES {
        let plan = sqlx::query_scalar::<_, String>(&format!("EXPLAIN {delete}"))
            .bind(64_i64)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(
            plan.iter()
                .any(|line| line.contains("Index Cond:") && line.contains("expires_at_utc")),
            "expiration must bound the index range, not filter live rows: {plan:#?}"
        );
    }
    let progress = purge_expired(&pool).await.unwrap();
    assert_eq!(progress.removed_mutations, 7);
    assert_eq!(progress.removed_links, 512);
    finish(admin, pool, schema).await;
}
