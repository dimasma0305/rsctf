use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn personal_transport_is_independent_of_api_gate_but_requires_a_live_event() {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("RSCTF_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(
        r#"CREATE TEMP TABLE "Games" (
               id INT PRIMARY KEY, deletion_pending BOOL NOT NULL DEFAULT FALSE,
               vpn_access_required BOOL NOT NULL DEFAULT FALSE,
               end_time_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp() + INTERVAL '1 hour'
           ) ON COMMIT DROP;
           CREATE TEMP TABLE "GameChallenges" (
               game_id INT, "Type" INT, is_enabled BOOL DEFAULT TRUE,
               review_status INT DEFAULT 0
           ) ON COMMIT DROP;
           INSERT INTO "Games" (id) SELECT generate_series(1, 10);
           UPDATE "Games" SET vpn_access_required = TRUE WHERE id IN (1, 8, 9);
           INSERT INTO "GameChallenges" (game_id, "Type") VALUES
               (2, 4), (3, 5), (4, 1), (5, 4), (6, 5), (8, 4), (9, 5), (10, 4);
           UPDATE "GameChallenges" SET is_enabled = FALSE WHERE game_id = 5;
           UPDATE "GameChallenges" SET review_status = 1 WHERE game_id = 6;
           UPDATE "Games" SET end_time_utc = clock_timestamp() - INTERVAL '1 second'
               WHERE id IN (8, 10);
           UPDATE "Games" SET deletion_pending = TRUE WHERE id = 9;"#,
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    let query = format!(
        r#"SELECT EXISTS(SELECT 1 FROM "Games" game
             WHERE game.id = $1 AND ({PERSONAL_PEER_GAME_ELIGIBLE_SQL}))"#,
    );
    for (game_id, expected) in [
        (1, true),   // VPN-gated Jeopardy-only event
        (2, true),   // A&D with API gate off
        (3, true),   // KotH with API gate off
        (4, false),  // ordinary container is not an A&D/KotH transport
        (5, false),  // disabled challenge
        (6, false),  // unreviewed challenge
        (7, false),  // empty ungated event
        (8, false),  // ended, even with API gate on
        (9, false),  // deletion pending
        (10, false), // ended ungated event
        (11, false), // nonexistent event
    ] {
        let actual: bool = sqlx::query_scalar(&query)
            .bind(game_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(actual, expected, "game {game_id}");
    }
    // Switching the API gate off must not invalidate an active game's player
    // peers. With no approved challenge left, transport must fail closed.
    sqlx::query(r#"UPDATE "Games" SET vpn_access_required = TRUE WHERE id = 2"#)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET vpn_access_required = FALSE WHERE id = 2"#)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(sqlx::query_scalar::<_, bool>(&query)
        .bind(2)
        .fetch_one(&mut *tx)
        .await
        .unwrap());
    sqlx::query(r#"UPDATE "GameChallenges" SET review_status = 2 WHERE game_id = 2"#)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(!sqlx::query_scalar::<_, bool>(&query)
        .bind(2)
        .fetch_one(&mut *tx)
        .await
        .unwrap());
    tx.rollback().await.unwrap();
    pool.close().await;
}

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
