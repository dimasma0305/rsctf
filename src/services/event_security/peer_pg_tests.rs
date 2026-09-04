use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn revoked_peer_addresses_remain_reserved_for_future_allocations() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("event_vpn_peer_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"CREATE TABLE "AdVpnPeers" (address TEXT NOT NULL);
           CREATE TABLE "EventVpnUserPeers" (
               address TEXT NOT NULL UNIQUE,
               revoked_at_utc TIMESTAMPTZ NULL
           );"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let user_id = Uuid::new_v4();
    let first = allocate_address("10.14.0.0/24", 35, user_id, &HashSet::new()).unwrap();
    sqlx::query(
        r#"INSERT INTO "EventVpnUserPeers" (address, revoked_at_utc)
           VALUES ($1, clock_timestamp())"#,
    )
    .bind(&first)
    .execute(&pool)
    .await
    .unwrap();

    let reserved = load_reserved_addresses(&pool).await.unwrap();
    assert!(reserved.contains(&first.parse().unwrap()));
    let next = allocate_address("10.14.0.0/24", 35, user_id, &reserved).unwrap();
    assert_ne!(next, first);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
