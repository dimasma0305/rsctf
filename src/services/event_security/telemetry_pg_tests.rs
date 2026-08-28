use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn duplicate_rows_replay_at_quota_but_novel_rows_are_rolled_back() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("telemetry_novel_quota_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"CREATE TABLE "Games" (
               id INTEGER PRIMARY KEY,
               deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
               vpn_behavior_telemetry_enabled BOOLEAN NOT NULL DEFAULT TRUE,
               vpn_flag_scan_enabled BOOLEAN NOT NULL DEFAULT FALSE,
               vpn_provider_dns_telemetry_enabled BOOLEAN NOT NULL DEFAULT FALSE,
               vpn_source_asn_telemetry_enabled BOOLEAN NOT NULL DEFAULT FALSE,
               vpn_device_sharing_telemetry_enabled BOOLEAN NOT NULL DEFAULT FALSE,
               start_time_utc TIMESTAMPTZ NOT NULL,
               end_time_utc TIMESTAMPTZ NOT NULL
           );
           CREATE TABLE "EventVpnUserPeers" (
               id UUID PRIMARY KEY, game_id INTEGER NOT NULL,
               user_id UUID NOT NULL, participation_id INTEGER NOT NULL,
               revoked_at_utc TIMESTAMPTZ NULL
           );
           CREATE TABLE "EventTelemetryBatches" (
               batch_id UUID PRIMARY KEY, game_id INTEGER NOT NULL,
               request_fingerprint BYTEA NOT NULL, result JSONB NULL,
               created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
               completed_at_utc TIMESTAMPTZ NULL
           );
           CREATE TABLE "AntiCheatTelemetryUsage" (
               game_id INTEGER PRIMARY KEY, logical_bytes BIGINT NOT NULL DEFAULT 0,
               row_count BIGINT NOT NULL DEFAULT 0, disabled_at_utc TIMESTAMPTZ NULL,
               updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
           );
           CREATE TABLE "AntiCheatTelemetryGlobalUsage" (
               id SMALLINT PRIMARY KEY, logical_bytes BIGINT NOT NULL DEFAULT 0,
               row_count BIGINT NOT NULL DEFAULT 0,
               updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
           );
           CREATE TABLE "AntiCheatTelemetryDrops" (
               id BIGSERIAL PRIMARY KEY, game_id INTEGER NULL,
               source SMALLINT NOT NULL, reason SMALLINT NOT NULL,
               dropped_rows BIGINT NOT NULL, dropped_bytes BIGINT NOT NULL,
               bucket_start_utc TIMESTAMPTZ NOT NULL,
               observed_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
           );
           CREATE UNIQUE INDEX ux_test_telemetry_drops
             ON "AntiCheatTelemetryDrops" (
               COALESCE(game_id, -1), source, reason, bucket_start_utc
             );
           CREATE TABLE "VpnFlowTelemetryBuckets" (
               id BIGSERIAL PRIMARY KEY, game_id INTEGER NOT NULL,
               user_id UUID NOT NULL, participation_id INTEGER NOT NULL,
               peer_id UUID NOT NULL, challenge_id INTEGER NULL,
               container_generation INTEGER NULL,
               bucket_start_utc TIMESTAMPTZ NOT NULL,
               packets_up BIGINT NOT NULL, packets_down BIGINT NOT NULL,
               bytes_up BIGINT NOT NULL, bytes_down BIGINT NOT NULL,
               distinct_destinations INTEGER NOT NULL,
               connection_count INTEGER NOT NULL, active_seconds INTEGER NOT NULL,
               created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
           );
           CREATE UNIQUE INDEX ux_test_flow_identity
             ON "VpnFlowTelemetryBuckets" (
               game_id, user_id, participation_id, peer_id,
               COALESCE(challenge_id, -1), COALESCE(container_generation, -1),
               bucket_start_utc
             );
           CREATE TABLE "VpnDnsProviderBuckets" (id BIGSERIAL PRIMARY KEY);
           CREATE TABLE "VpnPeerNetworkObservations" (id BIGSERIAL PRIMARY KEY);
           CREATE TABLE "VpnFlagTransportEvents" (id BIGSERIAL PRIMARY KEY);"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let user_id = Uuid::new_v4();
    let peer_id = Uuid::new_v4();
    let first_bucket = DateTime::parse_from_rfc3339("2026-08-20T13:40:00Z")
        .unwrap()
        .with_timezone(&Utc);
    sqlx::query(
        r#"INSERT INTO "Games" (id, start_time_utc, end_time_utc)
           VALUES (7, '2026-08-20T00:00:00Z', '2026-08-21T00:00:00Z')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "EventVpnUserPeers"
             (id, game_id, user_id, participation_id)
           VALUES ($1, 7, $2, 9)"#,
    )
    .bind(peer_id)
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AntiCheatTelemetryUsage"
             (game_id, logical_bytes, row_count) VALUES (7, $1, 1)"#,
    )
    .bind(EVENT_LOGICAL_QUOTA_BYTES)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "AntiCheatTelemetryGlobalUsage"
             (id, logical_bytes, row_count) VALUES (1, $1, 1)"#,
    )
    .bind(EVENT_LOGICAL_QUOTA_BYTES)
    .execute(&pool)
    .await
    .unwrap();

    let flow = FlowBucketInput {
        user_id,
        participation_id: 9,
        peer_id,
        challenge_id: None,
        container_generation: None,
        bucket_start_utc: first_bucket,
        packets_up: 1,
        packets_down: 1,
        bytes_up: 1,
        bytes_down: 1,
        distinct_destinations: 1,
        connection_count: 1,
        active_seconds: 1,
    };
    let mut seed = pool.begin().await.unwrap();
    assert_eq!(
        insert_flows(&mut seed, 7, std::slice::from_ref(&flow))
            .await
            .unwrap(),
        1
    );
    seed.commit().await.unwrap();

    let batch = |batch_id, flow| TelemetryBatch {
        batch_id,
        game_id: 7,
        flows: vec![flow],
        dns_providers: Vec::new(),
        peer_networks: Vec::new(),
        flag_transports: Vec::new(),
        sensor_dropped_rows: 0,
        sensor_dropped_bytes: 0,
    };
    let duplicate = ingest_batch_with_pool(&pool, &batch(Uuid::new_v4(), flow.clone()))
        .await
        .unwrap();
    assert_eq!(duplicate.accepted_rows, 0);
    assert_eq!(duplicate.duplicate_or_invalid_rows, 1);
    assert!(!duplicate.dropped_for_quota);

    let novel_flow = FlowBucketInput {
        bucket_start_utc: first_bucket + chrono::Duration::minutes(5),
        ..flow
    };
    let rejected = ingest_batch_with_pool(&pool, &batch(Uuid::new_v4(), novel_flow))
        .await
        .unwrap();
    assert_eq!(rejected.accepted_rows, 0);
    assert_eq!(rejected.duplicate_or_invalid_rows, 0);
    assert!(rejected.dropped_for_quota);
    let stored_rows: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "VpnFlowTelemetryBuckets""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_rows, 1);
    let usage: i64 = sqlx::query_scalar(
        r#"SELECT logical_bytes FROM "AntiCheatTelemetryUsage" WHERE game_id = 7"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, EVENT_LOGICAL_QUOTA_BYTES);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
