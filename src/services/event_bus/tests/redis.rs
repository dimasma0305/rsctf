use super::*;

/// Run explicitly with `RSCTF_TEST_REDIS_URL=redis://... cargo test
/// redis_bus_fans_out_once_between_processes -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires RSCTF_TEST_REDIS_URL and a reachable Redis server"]
async fn redis_bus_fans_out_once_between_processes() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("RSCTF_TEST_REDIS_URL");
    let channel = format!("rsctf:test:hub-events:{}", Uuid::new_v4());
    let sender = EventBus::distributed_on(&url, &channel).unwrap();
    let receiver_bus = EventBus::distributed_on(&url, &channel).unwrap();
    let mut receiver_remote = receiver_bus.subscribe();

    wait_for_remote_subscription(
        &sender,
        &mut receiver_remote,
        hub_event(r#"{"probe":"redis-ready"}"#),
    )
    .await;
    let mut sender_local = sender.subscribe();
    sender.publish(received_log_event());

    let local = tokio::time::timeout(Duration::from_secs(2), sender_local.recv())
        .await
        .unwrap()
        .unwrap();
    let remote = tokio::time::timeout(Duration::from_secs(2), receiver_remote.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local.target, "ReceivedLog");
    assert_eq!(remote.target, "ReceivedLog");
    assert_eq!(local.payload, remote.payload);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), sender_local.recv())
            .await
            .is_err()
    );
}

/// Run against a dedicated disposable Redis instance with `--test-threads=1`:
/// `RSCTF_TEST_REDIS_URL=redis://... cargo test
/// redis_healthy_subscriber_survives_a_heartbeat -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires RSCTF_TEST_REDIS_URL and a reachable Redis server"]
async fn redis_healthy_subscriber_survives_a_heartbeat() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("RSCTF_TEST_REDIS_URL");
    let channel = format!("rsctf:test:heartbeat-events:{}", Uuid::new_v4());
    let sender = EventBus::distributed_on(&url, &channel).unwrap();
    let receiver_bus = EventBus::distributed_on(&url, &channel).unwrap();
    let mut remote = receiver_bus.subscribe_game_targets(7, &["ReceivedGameEvent"]);
    wait_for_remote_target_subscription(&sender, &mut remote, received_game_event(7, -1, -1)).await;
    let gaps = receiver_bus.operational_metrics().subscriber_gaps;

    tokio::time::sleep(REDIS_SUBSCRIBER_HEARTBEAT + REDIS_RETRY_MIN + REDIS_IO_TIMEOUT).await;
    assert_eq!(receiver_bus.operational_metrics().subscriber_gaps, gaps);
}

/// This deliberately kills every Pub/Sub connection on the configured server.
/// Run only against a dedicated disposable Redis instance with
/// `--test-threads=1`: `RSCTF_TEST_REDIS_URL=redis://... cargo test
/// redis_forced_disconnect_lags_existing_receiver_after_reconnect -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires a dedicated disposable RSCTF_TEST_REDIS_URL"]
async fn redis_forced_disconnect_lags_existing_receiver_after_reconnect() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("RSCTF_TEST_REDIS_URL");
    let channel = format!("rsctf:test:disconnect-events:{}", Uuid::new_v4());
    let sender = EventBus::distributed_on(&url, &channel).unwrap();
    let receiver_bus = EventBus::distributed_on(&url, &channel).unwrap();
    let mut remote = receiver_bus.subscribe_game_targets(7, &["ReceivedGameEvent"]);
    wait_for_remote_target_subscription(&sender, &mut remote, received_game_event(7, -1, -1)).await;
    let gaps = receiver_bus.operational_metrics().subscriber_gaps;

    let client = redis::Client::open(url).unwrap();
    let mut admin = crate::utils::redis::connection_manager(&client)
        .await
        .unwrap();
    let killed = redis::cmd("CLIENT")
        .arg("KILL")
        .arg("TYPE")
        .arg("PUBSUB")
        .arg("SKIPME")
        .arg("YES")
        .query_async::<i64>(&mut admin)
        .await
        .unwrap();
    assert!(killed >= 1);

    tokio::time::timeout(Duration::from_secs(10), async {
        while receiver_bus.operational_metrics().subscriber_gaps <= gaps {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("subscriber did not reconnect after forced disconnect");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap(),
        Err(broadcast::error::RecvError::Lagged(0))
    ));
}

/// Run explicitly with `RSCTF_TEST_REDIS_URL=redis://... cargo test
/// redis_resync_marker_forces_remote_authoritative_recovery -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires RSCTF_TEST_REDIS_URL and a reachable Redis server"]
async fn redis_resync_marker_forces_remote_authoritative_recovery() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("RSCTF_TEST_REDIS_URL");
    let channel = format!("rsctf:test:resync-events:{}", Uuid::new_v4());
    let first_replica = EventBus::distributed_on(&url, &channel).unwrap();
    let second_replica = EventBus::distributed_on(&url, &channel).unwrap();
    let mut remote = second_replica.subscribe_game_targets(7, &["ReceivedGameEvent"]);

    wait_for_remote_target_subscription(
        &first_replica,
        &mut remote,
        received_game_event(7, -1, -1),
    )
    .await;
    let before_resync = second_replica.operational_metrics();
    first_replica.publish(HubEvent {
        target: RESYNC_TARGET,
        game_id: None,
        payload: serde_json::json!({ "generation": 1 }).to_string(),
    });

    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap(),
        Err(broadcast::error::RecvError::Lagged(0))
    ));
    let after_resync = second_replica.operational_metrics();
    assert_eq!(
        after_resync.lagged_receivers,
        before_resync.lagged_receivers.saturating_add(1)
    );
    assert_eq!(after_resync.subscriber_gaps, before_resync.subscriber_gaps);
}

/// Run explicitly with `RSCTF_TEST_REDIS_URL=redis://... cargo test
/// redis_monitor_feed_preserves_order_dedup_and_scope -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires RSCTF_TEST_REDIS_URL and a reachable Redis server"]
async fn redis_monitor_feed_preserves_order_dedup_and_scope() {
    let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("RSCTF_TEST_REDIS_URL");
    let channel = format!("rsctf:test:monitor-events:{}", Uuid::new_v4());
    let first_replica = EventBus::distributed_on(&url, &channel).unwrap();
    let second_replica = EventBus::distributed_on(&url, &channel).unwrap();
    let mut remote = second_replica.subscribe_game_targets(7, &["ReceivedGameEvent"]);

    wait_for_remote_target_subscription(
        &first_replica,
        &mut remote,
        received_game_event(7, -1, -1),
    )
    .await;
    // The maximum accepted A&D flag batch in another game must not enter
    // this target/game receiver's bounded history.
    for cursor in 0..100 {
        first_replica.publish(received_game_event(8, cursor, cursor as i64));
    }
    for (id, cursor) in [(31, 101), (32, 102), (33, 103)] {
        first_replica.publish(received_game_event(7, id, cursor));
    }
    for expected_cursor in [101, 102, 103] {
        let received = tokio::time::timeout(Duration::from_secs(2), remote.recv())
            .await
            .unwrap()
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&received.payload).unwrap();
        assert_eq!(payload["cursor"], expected_cursor);
        assert!(crate::hubs::signalr::event_matches(
            &["ReceivedGameEvent"],
            Some(7),
            &received,
        ));
        assert!(!crate::hubs::signalr::event_matches(
            &["ReceivedGameEvent"],
            Some(8),
            &received,
        ));
    }

    // A duplicate Redis wire id must be injected only once on the remote
    // replica, even if Redis delivers the same payload twice.
    let duplicate = WireEvent {
        version: 1,
        id: Uuid::now_v7(),
        origin: Uuid::new_v4(),
        target: "ReceivedGameEvent".to_owned(),
        game_id: Some(7),
        payload: received_game_event(7, 34, 104).payload,
    };
    let bytes = serde_json::to_vec(&duplicate).unwrap();
    let client = redis::Client::open(url).unwrap();
    let mut publisher = crate::utils::redis::connection_manager(&client)
        .await
        .unwrap();
    for _ in 0..2 {
        redis::cmd("PUBLISH")
            .arg(&channel)
            .arg(&bytes)
            .query_async::<i64>(&mut publisher)
            .await
            .unwrap();
    }
    let once = tokio::time::timeout(Duration::from_secs(2), remote.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&once.payload).unwrap()["cursor"],
        104
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), remote.recv())
            .await
            .is_err()
    );
}
