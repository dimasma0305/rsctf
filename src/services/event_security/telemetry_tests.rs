use super::*;

#[test]
fn bounds_are_deliberately_small_and_gameplay_independent() {
    assert_eq!(EVENT_LOGICAL_QUOTA_BYTES, 256 * 1024 * 1024);
    assert_eq!(GLOBAL_LOGICAL_QUOTA_BYTES, 5 * 1024 * 1024 * 1024);
    assert_eq!(MAX_PATTERNS, 50_000);
    assert_eq!(MAX_PATTERN_BYTES, 4 * 1024 * 1024);
    assert_eq!(MAX_TRACKED_FLOWS, 65_536);
    assert_eq!(MAX_INGEST_ROWS, 4_096);
    assert_eq!(INGEST_INTERVAL_SECONDS, 30);
}

#[test]
fn invalid_bucket_and_raw_values_are_rejected_before_database_work() {
    let batch = TelemetryBatch {
        game_id: 1,
        flows: vec![FlowBucketInput {
            user_id: Uuid::nil(),
            participation_id: 1,
            peer_id: Uuid::nil(),
            challenge_id: None,
            container_generation: None,
            bucket_start_utc: Utc::now(),
            packets_up: 0,
            packets_down: 0,
            bytes_up: 0,
            bytes_down: 0,
            distinct_destinations: 0,
            connection_count: 0,
            active_seconds: 0,
        }],
        dns_providers: Vec::new(),
        peer_networks: Vec::new(),
        flag_transports: Vec::new(),
        sensor_dropped_rows: 0,
        sensor_dropped_bytes: 0,
    };
    assert!(batch.validate().is_err());
    assert!(decode_hash("not-an-address-or-hash").is_err());
}

#[test]
fn internal_bulk_timestamps_are_rfc3339_not_wire_milliseconds() {
    let timestamp = DateTime::parse_from_rfc3339("2026-08-20T13:40:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let flow = FlowBucketInput {
        user_id: Uuid::nil(),
        participation_id: 1,
        peer_id: Uuid::nil(),
        challenge_id: None,
        container_generation: None,
        bucket_start_utc: timestamp,
        packets_up: 1,
        packets_down: 1,
        bytes_up: 1,
        bytes_down: 1,
        distinct_destinations: 1,
        connection_count: 1,
        active_seconds: 1,
    };
    let dns = DnsProviderBucketInput {
        user_id: Uuid::nil(),
        participation_id: 1,
        peer_id: Uuid::nil(),
        provider_category: 1,
        bucket_start_utc: timestamp,
        query_count: 1,
        first_seen_at_utc: timestamp,
        last_seen_at_utc: timestamp,
    };

    assert_eq!(
        flow_database_rows(&[flow])[0]["bucketStartUtc"],
        "2026-08-20T13:40:00Z"
    );
    assert_eq!(
        dns_database_rows(&[dns])[0]["firstSeenAtUtc"],
        "2026-08-20T13:40:00Z"
    );
}

#[test]
fn trigger_stamped_telemetry_prefilters_exact_replays() {
    let source = include_str!("telemetry.rs");
    assert_eq!(source.matches("deduped_input AS MATERIALIZED").count(), 3);
    assert_eq!(
        source
            .matches("Reconciliation uses a BEFORE INSERT stamp")
            .count(),
        3
    );
    for exact_lookup in [
        "SELECT 1 FROM \"VpnDnsProviderBuckets\" existing",
        "SELECT 1 FROM \"VpnPeerNetworkObservations\" existing",
        "SELECT 1 FROM \"VpnFlagTransportEvents\" existing",
    ] {
        assert!(source.contains(exact_lookup));
    }
    assert_eq!(
        source.matches("ON CONFLICT DO NOTHING RETURNING 1").count(),
        4
    );
    assert!(source.contains("let policy = load_ingest_policy"));
    assert!(source.contains("if !policy.5"));
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn postgres_final_barrier_rejects_a_delayed_telemetry_reader() {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use tokio::sync::oneshot;

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("telemetry_barrier_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(&format!(
        r#"CREATE SCHEMA "{schema}";
           CREATE TABLE "{schema}"."Games" (
               id INTEGER PRIMARY KEY,
               deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
               vpn_behavior_telemetry_enabled BOOLEAN NOT NULL DEFAULT TRUE,
               vpn_flag_scan_enabled BOOLEAN NOT NULL DEFAULT TRUE,
               vpn_provider_dns_telemetry_enabled BOOLEAN NOT NULL DEFAULT TRUE,
               vpn_source_asn_telemetry_enabled BOOLEAN NOT NULL DEFAULT TRUE,
               vpn_device_sharing_telemetry_enabled BOOLEAN NOT NULL DEFAULT TRUE
           );
           CREATE TABLE "{schema}"."SuspicionReconciliationState" (
               game_id INTEGER PRIMARY KEY,
               evidence_closed_at_utc TIMESTAMPTZ NULL
           );
           INSERT INTO "{schema}"."Games" (id) VALUES (1);
           INSERT INTO "{schema}"."SuspicionReconciliationState" (game_id)
               VALUES (1);"#
    ))
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

    let mut finalizer = pool.begin().await.unwrap();
    sqlx::query(r#"SELECT id FROM "Games" WHERE id = 1 FOR UPDATE"#)
        .execute(&mut *finalizer)
        .await
        .unwrap();
    sqlx::query(
        r#"UPDATE "SuspicionReconciliationState"
              SET evidence_closed_at_utc = clock_timestamp()
            WHERE game_id = 1"#,
    )
    .execute(&mut *finalizer)
    .await
    .unwrap();
    let finalizer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *finalizer)
        .await
        .unwrap();

    let delayed_pool = pool.clone();
    let (started_tx, started_rx) = oneshot::channel();
    let delayed = tokio::spawn(async move {
        let mut transaction = delayed_pool.begin().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        started_tx.send(pid).unwrap();
        let policy = load_ingest_policy(&mut transaction, 1).await;
        transaction.rollback().await.unwrap();
        policy
    });
    let delayed_pid = started_rx.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                .bind(finalizer_pid)
                .bind(delayed_pid)
                .fetch_one(&pool)
                .await
                .unwrap();
            if blocked {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("delayed telemetry reader never waited behind the final game barrier");
    finalizer.commit().await.unwrap();
    let policy = tokio::time::timeout(std::time::Duration::from_secs(3), delayed)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !policy.5,
        "a reader released after sealing must see closure"
    );

    pool.close().await;
    sqlx::raw_sql(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires migrated disposable PostgreSQL with VPN telemetry fixtures"]
async fn postgres_exact_batch_replay_leaves_reconciliation_clean() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .unwrap();
    let game_id: i32 = sqlx::query_scalar(
        r#"SELECT game.id
              FROM "Games" game
             WHERE EXISTS (
                       SELECT 1 FROM "VpnDnsProviderBuckets" dns
                       JOIN "EventVpnUserPeers" peer ON peer.id = dns.peer_id
                      WHERE dns.game_id = game.id AND peer.revoked_at_utc IS NULL
                   )
               AND EXISTS (
                       SELECT 1 FROM "VpnPeerNetworkObservations" network
                       JOIN "EventVpnUserPeers" peer ON peer.id = network.peer_id
                      WHERE network.game_id = game.id AND peer.revoked_at_utc IS NULL
                   )
               AND EXISTS (
                       SELECT 1 FROM "VpnFlagTransportEvents" flag
                       JOIN "EventVpnUserPeers" peer ON peer.id = flag.peer_id
                       JOIN "GameChallenges" challenge
                         ON challenge.game_id = flag.game_id
                        AND challenge.id = flag.challenge_id
                      WHERE flag.game_id = game.id AND peer.revoked_at_utc IS NULL
                        AND challenge."Type" NOT IN (4, 5)
                        AND (
                             challenge.flag_template IS NOT NULL
                             OR EXISTS (
                                 SELECT 1 FROM "ChallengeVariants" variant
                                  WHERE variant.game_id = flag.game_id
                                    AND variant.challenge_id = flag.challenge_id
                                    AND variant.participation_id =
                                          flag.owning_participation_id
                                    AND variant.frozen_at_utc IS NOT NULL
                             )
                        )
                   )
             ORDER BY game.id LIMIT 1"#,
    )
    .fetch_one(&pool)
    .await
    .expect("fixture needs one game with replayable DNS, peer, and flag telemetry");
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(
        r#"UPDATE "Games"
              SET vpn_provider_dns_telemetry_enabled = TRUE,
                  vpn_source_asn_telemetry_enabled = TRUE,
                  vpn_flag_scan_enabled = TRUE
            WHERE id = $1"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .unwrap();

    let dns_row: (
        Uuid,
        i32,
        Uuid,
        i16,
        DateTime<Utc>,
        i32,
        DateTime<Utc>,
        DateTime<Utc>,
    ) = sqlx::query_as(
        r#"SELECT dns.user_id, dns.participation_id, dns.peer_id,
                      dns.provider_category, dns.bucket_start_utc,
                      dns.query_count, dns.first_seen_at_utc, dns.last_seen_at_utc
                 FROM "VpnDnsProviderBuckets" dns
                 JOIN "EventVpnUserPeers" peer ON peer.id = dns.peer_id
                WHERE dns.game_id = $1 AND peer.revoked_at_utc IS NULL
                ORDER BY dns.id LIMIT 1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    let network_row: (
        Uuid,
        i32,
        Uuid,
        Vec<u8>,
        Option<i64>,
        i16,
        DateTime<Utc>,
        DateTime<Utc>,
        i32,
    ) = sqlx::query_as(
        r#"SELECT network.user_id, network.participation_id, network.peer_id,
                  network.endpoint_hash, network.source_asn, network.network_class,
                  network.first_seen_at_utc, network.last_seen_at_utc,
                  network.handshake_count
             FROM "VpnPeerNetworkObservations" network
             JOIN "EventVpnUserPeers" peer ON peer.id = network.peer_id
            WHERE network.game_id = $1 AND peer.revoked_at_utc IS NULL
            ORDER BY network.id LIMIT 1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    let flag_row: (i32, Uuid, i32, i32, Uuid, Vec<u8>, i16, i16, DateTime<Utc>) = sqlx::query_as(
        r#"SELECT flag.challenge_id, flag.receiving_user_id,
                      flag.receiving_participation_id, flag.owning_participation_id,
                      flag.peer_id, flag.flag_value_hash, flag.transport,
                      flag.direction, flag.observed_at_utc
                 FROM "VpnFlagTransportEvents" flag
                 JOIN "EventVpnUserPeers" peer ON peer.id = flag.peer_id
                 JOIN "GameChallenges" challenge
                   ON challenge.game_id = flag.game_id
                  AND challenge.id = flag.challenge_id
                WHERE flag.game_id = $1 AND peer.revoked_at_utc IS NULL
                  AND challenge."Type" NOT IN (4, 5)
                  AND (
                       challenge.flag_template IS NOT NULL
                       OR EXISTS (
                           SELECT 1 FROM "ChallengeVariants" variant
                            WHERE variant.game_id = flag.game_id
                              AND variant.challenge_id = flag.challenge_id
                              AND variant.participation_id =
                                    flag.owning_participation_id
                              AND variant.frozen_at_utc IS NOT NULL
                       )
                  )
                ORDER BY flag.id LIMIT 1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    let before_sources: Vec<(i16, i64, i64)> = sqlx::query_as(
        r#"SELECT source_kind, applied_version, dirty_version
             FROM "AntiCheatReconciliationSources"
            WHERE game_id = $1 AND source_kind IN (3, 4, 5)
            ORDER BY source_kind"#,
    )
    .bind(game_id)
    .fetch_all(&mut *transaction)
    .await
    .unwrap();
    let before_queue: (i64, i64) = sqlx::query_as(
        r#"SELECT applied_generation, desired_generation
             FROM "AntiCheatReconciliationQueue" WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();

    let dns = DnsProviderBucketInput {
        user_id: dns_row.0,
        participation_id: dns_row.1,
        peer_id: dns_row.2,
        provider_category: dns_row.3,
        bucket_start_utc: dns_row.4,
        query_count: dns_row.5,
        first_seen_at_utc: dns_row.6,
        last_seen_at_utc: dns_row.7,
    };
    let network = PeerNetworkInput {
        user_id: network_row.0,
        participation_id: network_row.1,
        peer_id: network_row.2,
        endpoint_hash: hex::encode(network_row.3),
        source_asn: network_row.4,
        network_class: network_row.5,
        first_seen_at_utc: network_row.6,
        last_seen_at_utc: network_row.7,
        handshake_count: network_row.8,
    };
    let flag = FlagTransportInput {
        challenge_id: flag_row.0,
        receiving_user_id: flag_row.1,
        receiving_participation_id: flag_row.2,
        owning_participation_id: flag_row.3,
        peer_id: flag_row.4,
        flag_value_hash: hex::encode(flag_row.5),
        transport: flag_row.6,
        direction: flag_row.7,
        observed_at_utc: flag_row.8,
    };
    assert_eq!(
        insert_dns(&mut transaction, game_id, &[dns]).await.unwrap(),
        0
    );
    assert_eq!(
        insert_networks(&mut transaction, game_id, &[network])
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        insert_flags(&mut transaction, game_id, &[flag])
            .await
            .unwrap(),
        0
    );
    let after_sources: Vec<(i16, i64, i64)> = sqlx::query_as(
        r#"SELECT source_kind, applied_version, dirty_version
             FROM "AntiCheatReconciliationSources"
            WHERE game_id = $1 AND source_kind IN (3, 4, 5)
            ORDER BY source_kind"#,
    )
    .bind(game_id)
    .fetch_all(&mut *transaction)
    .await
    .unwrap();
    let after_queue: (i64, i64) = sqlx::query_as(
        r#"SELECT applied_generation, desired_generation
             FROM "AntiCheatReconciliationQueue" WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_one(&mut *transaction)
    .await
    .unwrap();
    assert_eq!(after_sources, before_sources);
    assert_eq!(after_queue, before_queue);
    transaction.rollback().await.unwrap();
    pool.close().await;
}
