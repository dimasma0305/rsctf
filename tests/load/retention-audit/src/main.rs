//! Isolated release benchmark of the actual cleanup engine and v0.1.145 policy.
//! One scheduled pass/second accelerates the production 30-second cadence.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[path = "../../../../src/services/mutation_retention/cleanup.rs"]
mod candidate;

// Exact SQL/limits retained from 9ebf91ba (v0.1.145). The production engine
// above is compiled by path, so the candidate cannot drift from the app.
const BASELINE: [&str; 2] = [
    r#"DELETE FROM "MutationOperations" WHERE ctid IN (
           SELECT ctid FROM "MutationOperations" WHERE expires_at_utc <= clock_timestamp()
            ORDER BY expires_at_utc LIMIT $1)"#,
    r#"DELETE FROM "AccountLinkAttempts" WHERE token_digest IN (
           SELECT token_digest FROM "AccountLinkAttempts" WHERE expires_at_utc <= clock_timestamp()
            ORDER BY expires_at_utc LIMIT $1)"#,
];

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).expect("baseline or candidate");
    assert!(matches!(mode.as_str(), "baseline" | "candidate"));
    let url = std::env::var("RSCTF_TEST_DATABASE_URL")?;
    let schema = format!(
        "retention_audit_{}_{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await?;
    let options = PgConnectOptions::from_str(&url)?.options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await?;
    sqlx::raw_sql(r#"
       CREATE TABLE "MutationOperations" (id INTEGER PRIMARY KEY, expires_at_utc TIMESTAMPTZ NOT NULL);
       CREATE INDEX ON "MutationOperations" (expires_at_utc);
       CREATE TABLE "AccountLinkAttempts" (token_digest BYTEA PRIMARY KEY, expires_at_utc TIMESTAMPTZ NOT NULL);
       CREATE INDEX ON "AccountLinkAttempts" (expires_at_utc);
       INSERT INTO "MutationOperations" SELECT n, clock_timestamp()-INTERVAL '1 hour' FROM generate_series(1,8192) n;
       INSERT INTO "AccountLinkAttempts" SELECT decode(lpad(to_hex(n),8,'0'),'hex'), clock_timestamp()-INTERVAL '1 hour' FROM generate_series(1,8192) n;
       INSERT INTO "MutationOperations" SELECT n, clock_timestamp()+INTERVAL '1 day' FROM generate_series(8193,8208) n;
       INSERT INTO "AccountLinkAttempts" SELECT decode(lpad(to_hex(n),8,'0'),'hex'), clock_timestamp()+INTERVAL '1 day' FROM generate_series(8193,8208) n;
       ANALYZE "MutationOperations"; ANALYZE "AccountLinkAttempts";
    "#).execute(&pool).await?;
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut removed = 0_u64;
    for tick in 0..10 {
        tokio::time::sleep_until((started + Duration::from_secs(tick)).into()).await;
        let pass = Instant::now();
        if mode == "baseline" {
            for sql in BASELINE {
                removed += sqlx::query(sql)
                    .bind(64_i64)
                    .execute(&pool)
                    .await?
                    .rows_affected();
            }
        } else {
            let mut transaction = pool.begin().await?;
            let progress = candidate::purge_in_transaction(&mut transaction).await?;
            removed += progress.removed_mutations + progress.removed_links;
            transaction.commit().await?;
        }
        samples.push(pass.elapsed().as_secs_f64() * 1000.0);
    }
    let mut remaining = 0_i64;
    for sql in [
        r#"SELECT COUNT(*) FROM "MutationOperations" WHERE expires_at_utc <= clock_timestamp()"#,
        r#"SELECT COUNT(*) FROM "AccountLinkAttempts" WHERE expires_at_utc <= clock_timestamp()"#,
    ] {
        remaining += sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await?;
    }
    for sql in [
        r#"SELECT COUNT(*) FROM "MutationOperations" WHERE expires_at_utc > clock_timestamp()"#,
        r#"SELECT COUNT(*) FROM "AccountLinkAttempts" WHERE expires_at_utc > clock_timestamp()"#,
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await?,
            16
        );
    }
    assert_eq!(remaining + removed as i64, 16384);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await?;
    admin.close().await;
    println!(
        "{}",
        serde_json::json!({"mode":mode,"passes":10,"passIntervalMs":1000,
        "productionIntervalMs":30000,"initialExpired":16384,"removed":removed,"remainingExpired":remaining,
        "activeRowsPreserved":32,"latencyMs":samples})
    );
    Ok(())
}
