use super::*;

fn hub_event(payload: &str) -> HubEvent {
    HubEvent {
        target: "ReceivedAttack",
        game_id: Some(7),
        payload: payload.to_string(),
    }
}

fn received_log_event() -> HubEvent {
    HubEvent {
        target: "ReceivedLog",
        game_id: None,
        payload: r#"{"time":1784536800123,"level":"Information","msg":"audit"}"#.to_string(),
    }
}

fn received_game_event(game_id: i32, id: i32, cursor: i64) -> HubEvent {
    HubEvent {
        target: "ReceivedGameEvent",
        game_id: Some(game_id),
        payload: serde_json::json!({
            "id": id,
            "cursor": cursor,
            "type": "ChallengeOpened",
            "values": ["7", "fixture"],
            "time": 1_787_818_400_123_i64,
            "user": "player",
            "team": "team",
        })
        .to_string(),
    }
}

fn prepared(event: &HubEvent) -> PreparedWireEvent {
    let target = validate_hub_event(event.target, event.game_id, &event.payload).unwrap();
    PreparedWireEvent::from_hub(Uuid::new_v4(), target, event).unwrap()
}

fn decode_queued(event: QueuedWireEvent) -> HubEvent {
    serde_json::from_slice::<WireEvent>(&event.bytes)
        .unwrap()
        .into_hub()
        .unwrap()
}

fn test_distributed_bus(limits: OutboundLimits) -> EventBus {
    EventBus {
        local: Arc::new(LocalFanout::new()),
        distributed: Some(DistributedPublisher {
            origin: Uuid::new_v4(),
            outbound: Arc::new(DistributedOutbox::new(limits)),
            _tasks: Arc::new(TaskSet {
                handles: Vec::new(),
            }),
        }),
    }
}

async fn wait_for_remote_subscription(
    sender: &EventBus,
    receiver: &mut broadcast::Receiver<HubEvent>,
    probe: HubEvent,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            sender.publish(probe.clone());
            if let Ok(Ok(received)) =
                tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await
            {
                if received.target == probe.target && received.payload == probe.payload {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Redis replica subscription did not become ready");

    // A quiet-period drain can pass while an old probe is still in Redis or
    // the local broadcast queue. One ordered barrier from the same publisher
    // proves every readiness probe is already consumed before assertions.
    let mut barrier = probe;
    barrier.payload = serde_json::json!({
        "probe": "rsctf-ready-barrier",
        "id": Uuid::new_v4(),
    })
    .to_string();
    sender.publish(barrier.clone());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let received = receiver
                .recv()
                .await
                .expect("Redis readiness stream closed");
            if received.target == barrier.target && received.payload == barrier.payload {
                return;
            }
        }
    })
    .await
    .expect("Redis readiness barrier was not delivered");
}

async fn wait_for_remote_target_subscription(
    sender: &EventBus,
    receiver: &mut EventReceiver,
    probe: HubEvent,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            sender.publish(probe.clone());
            if let Ok(Ok(received)) =
                tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await
            {
                if received.target == probe.target && received.payload == probe.payload {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Redis target subscription did not become ready");

    let mut barrier = probe;
    barrier.payload = serde_json::json!({
        "probe": "rsctf-target-ready-barrier",
        "id": Uuid::new_v4(),
    })
    .to_string();
    sender.publish(barrier.clone());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let received = receiver.recv().await.expect("target stream closed");
            if received.target == barrier.target && received.payload == barrier.payload {
                return;
            }
        }
    })
    .await
    .expect("Redis target readiness barrier was not delivered");
}

#[test]
fn wire_event_round_trips_and_rejects_unknown_targets_or_versions() {
    let origin = Uuid::new_v4();
    let event = WireEvent::from_hub(origin, &hub_event(r#"{"kind":"attack"}"#));
    let bytes = serde_json::to_vec(&event).unwrap();
    let decoded: WireEvent = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded, event);

    let hub = decoded.into_hub().unwrap();
    assert_eq!(hub.target, "ReceivedAttack");
    assert_eq!(hub.game_id, Some(7));

    let mut unknown = event.clone();
    unknown.target = "ArbitraryClientMethod".to_string();
    assert!(unknown.into_hub().is_none());
    let mut future = event;
    future.version = 2;
    assert!(future.into_hub().is_none());
}

#[test]
fn wire_event_rejects_wrong_target_scope() {
    for target in KNOWN_TARGETS {
        let spec = known_target(target).expect("catalog names must have scope metadata");
        let valid_game_id = match spec.scope {
            TargetScope::Game => Some(7),
            TargetScope::Global | TargetScope::Resync => None,
        };
        let valid = WireEvent {
            version: 1,
            id: Uuid::now_v7(),
            origin: Uuid::new_v4(),
            target: target.to_string(),
            game_id: valid_game_id,
            payload: if spec.scope == TargetScope::Resync {
                r#"{"generation":1}"#.to_string()
            } else {
                "{}".to_string()
            },
        };
        assert!(valid.clone().into_hub().is_some(), "{target}");

        if spec.scope == TargetScope::Resync {
            continue;
        }

        let mut wrong_scope = valid;
        wrong_scope.game_id = match spec.scope {
            TargetScope::Game => None,
            TargetScope::Global => Some(7),
            TargetScope::Resync => unreachable!(),
        };
        assert!(wrong_scope.into_hub().is_none(), "{target}");
    }
}

#[test]
fn rejected_remote_wire_events_are_counted() {
    let local = LocalFanout::new();
    assert!(decode_inbound_wire(b"not-json", &local).is_none());
    assert!(decode_inbound_wire(&vec![b'x'; MAX_WIRE_BYTES + 1], &local).is_none());

    let mut unknown = WireEvent::from_hub(Uuid::new_v4(), &hub_event(r#"{"ok":true}"#));
    unknown.target = "UnknownClientMethod".to_owned();
    assert!(decode_inbound_wire(&serde_json::to_vec(&unknown).unwrap(), &local).is_none());

    let mut unscoped = WireEvent::from_hub(Uuid::new_v4(), &hub_event(r#"{"ok":true}"#));
    unscoped.game_id = None;
    assert!(decode_inbound_wire(&serde_json::to_vec(&unscoped).unwrap(), &local).is_none());
    assert_eq!(local.rejected_events.load(Ordering::Relaxed), 4);
}

#[test]
fn wire_event_rejects_malformed_or_signalr_framed_payloads() {
    let valid = WireEvent::from_hub(Uuid::new_v4(), &hub_event(r#"{"kind":"attack"}"#));

    for payload in [
        "not-json".to_string(),
        format!(r#"{{"kind":"attack"}}{}"#, '\u{1e}'),
        format!(
            r#"{{"kind":"attack"}}{}{{"type":1,"target":"ReceivedLog","arguments":[]}}"#,
            '\u{1e}'
        ),
    ] {
        let mut injected = valid.clone();
        injected.payload = payload;
        assert!(injected.into_hub().is_none());
    }

    let mut escaped_separator = valid;
    escaped_separator.payload = r#"{"message":"\u001e"}"#.to_string();
    assert!(escaped_separator.into_hub().is_some());
}

#[test]
fn flag_egress_is_a_game_scoped_distributed_target() {
    let wire = WireEvent {
        version: 1,
        id: Uuid::now_v7(),
        origin: Uuid::new_v4(),
        target: "ReceivedFlagEgress".to_owned(),
        game_id: Some(7),
        payload: r#"{"id":11,"cursor":19,"gameId":7}"#.to_owned(),
    };
    let delivered = wire.into_hub().unwrap();
    assert_eq!(delivered.target, "ReceivedFlagEgress");
    assert_eq!(delivered.game_id, Some(7));
}

#[test]
fn internal_control_targets_are_valid_distributed_events() {
    for target in [
        "InternalByocRevokeParticipation",
        "InternalByocRevokeTeam",
        "InternalByocRevokeChallenge",
        "InternalTrafficCaptureReconcile",
    ] {
        let wire = WireEvent {
            version: 1,
            id: Uuid::now_v7(),
            origin: Uuid::new_v4(),
            target: target.to_string(),
            game_id: None,
            payload: "42".to_string(),
        };
        assert_eq!(wire.into_hub().unwrap().target, target);
    }
}

#[test]
fn received_log_round_trips_through_the_distributed_wire_allowlist() {
    let event = received_log_event();
    let wire = WireEvent::from_hub(Uuid::new_v4(), &event);
    let delivered = wire
        .into_hub()
        .expect("ReceivedLog must fan out to replicas");

    assert_eq!(delivered.target, "ReceivedLog");
    assert_eq!(delivered.game_id, None);
    assert_eq!(delivered.payload, event.payload);
}

#[test]
fn received_game_event_round_trips_through_the_distributed_wire_allowlist() {
    let event = received_game_event(7, 11, 19);
    let delivered = WireEvent::from_hub(Uuid::new_v4(), &event)
        .into_hub()
        .expect("ReceivedGameEvent must fan out to replicas");
    assert_eq!(delivered.target, "ReceivedGameEvent");
    assert_eq!(delivered.game_id, Some(7));
    assert_eq!(delivered.payload, event.payload);
}

#[test]
fn inbound_dedup_drops_self_echoes_and_duplicate_remote_ids() {
    let local_origin = Uuid::new_v4();
    let mut dedup = InboundDedup::new(local_origin);

    let own = WireEvent::from_hub(local_origin, &hub_event("{}"));
    assert!(!dedup.should_deliver(&own));

    let remote = WireEvent::from_hub(Uuid::new_v4(), &hub_event("{}"));
    assert!(dedup.should_deliver(&remote));
    assert!(!dedup.should_deliver(&remote));

    let another = WireEvent::from_hub(Uuid::new_v4(), &hub_event("{}"));
    assert!(dedup.should_deliver(&another));
}

#[tokio::test]
async fn local_bus_delivers_synchronously_without_redis() {
    let bus = EventBus::local();
    assert!(!bus.is_distributed());
    let mut receiver = bus.subscribe();
    bus.publish(hub_event(r#"{"kind":"koth"}"#));

    let received = receiver.recv().await.unwrap();
    assert_eq!(received.target, "ReceivedAttack");
    assert_eq!(received.game_id, Some(7));
    assert_eq!(received.payload, r#"{"kind":"koth"}"#);
}

#[tokio::test]
async fn local_bus_delivers_received_log_without_redis() {
    let bus = EventBus::local();
    let mut receiver = bus.subscribe();
    let event = received_log_event();
    bus.publish(event.clone());

    let received = receiver.recv().await.unwrap();
    assert_eq!(received.target, "ReceivedLog");
    assert_eq!(received.game_id, None);
    assert_eq!(received.payload, event.payload);
}

#[tokio::test]
async fn local_bus_delivers_received_game_event_once() {
    let bus = EventBus::local();
    let mut receiver = bus.subscribe();
    let event = received_game_event(7, 11, 19);
    bus.publish(event.clone());

    let received = receiver.recv().await.unwrap();
    assert_eq!(received.target, "ReceivedGameEvent");
    assert_eq!(received.game_id, Some(7));
    assert_eq!(received.payload, event.payload);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), receiver.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn local_bus_rejects_unknown_wrong_scope_and_non_json_events() {
    let bus = EventBus::local();
    let mut receiver = bus.subscribe();
    for event in [
        HubEvent {
            target: "ArbitraryClientMethod",
            game_id: Some(7),
            payload: "{}".to_string(),
        },
        HubEvent {
            target: "ReceivedGameEvent",
            game_id: None,
            payload: "{}".to_string(),
        },
        HubEvent {
            target: "ReceivedLog",
            game_id: Some(7),
            payload: "{}".to_string(),
        },
        HubEvent {
            target: "ReceivedAttack",
            game_id: Some(7),
            payload: format!(r#"{{"kind":"attack"}}{}{{"type":1}}"#, '\u{1e}'),
        },
    ] {
        bus.publish(event);
    }

    assert!(
        tokio::time::timeout(Duration::from_millis(20), receiver.recv())
            .await
            .is_err()
    );
    assert_eq!(bus.operational_metrics().rejected_events, 4);
}

#[tokio::test]
async fn per_game_queues_isolate_former_hash_collisions_and_unrelated_targets() {
    let bus = EventBus::local();
    let mut game_seven = bus.subscribe_game_targets(7, &["ReceivedGameEvent"]);
    let mut formerly_colliding_game = bus.subscribe_game_targets(71, &["ReceivedGameEvent"]);

    for cursor in 0..(LOCAL_QUEUE_CAPACITY * 2) {
        bus.publish(received_game_event(7, cursor as i32, cursor as i64));
    }
    bus.publish(received_game_event(71, 1, 1));
    bus.publish(received_log_event());

    let received = formerly_colliding_game.recv().await.unwrap();
    assert_eq!(received.game_id, Some(71));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), formerly_colliding_game.recv())
            .await
            .is_err()
    );

    assert!(matches!(
        game_seven.recv().await,
        Err(broadcast::error::RecvError::Lagged(_))
    ));
    assert_eq!(bus.local.lagged_receivers.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn explicit_all_game_subscription_observes_each_game_without_cross_delivery() {
    let bus = EventBus::local();
    let mut all_games = bus.subscribe_global_targets(&["ReceivedGameEvent"]);
    let mut game_seven = bus.subscribe_game_targets(7, &["ReceivedGameEvent"]);

    bus.publish(received_game_event(7, 1, 1));
    bus.publish(received_game_event(8, 2, 2));

    assert_eq!(all_games.recv().await.unwrap().game_id, Some(7));
    assert_eq!(all_games.recv().await.unwrap().game_id, Some(8));
    assert_eq!(game_seven.recv().await.unwrap().game_id, Some(7));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), game_seven.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn distributed_loss_marker_forces_authoritative_resync() {
    let bus = EventBus::local();
    let mut receiver = bus.subscribe_game_targets(7, &["ReceivedGameEvent"]);
    bus.publish(received_game_event(7, 11, 19));
    bus.local.force_resync_after_subscriber_gap();

    // Even when ordinary data was already ready, the resync marker is an
    // ordering barrier and must win the next receive.
    assert!(matches!(
        receiver.recv().await,
        Err(broadcast::error::RecvError::Lagged(0))
    ));
    assert_eq!(receiver.recv().await.unwrap().target, "ReceivedGameEvent");
    assert_eq!(bus.local.lagged_receivers.load(Ordering::Relaxed), 1);
    assert_eq!(bus.local.subscriber_gaps.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn initial_redis_subscription_forces_resync_without_counting_a_gap() {
    let bus = EventBus::local();
    let mut receiver = bus.subscribe_game_targets(7, &["ReceivedGameEvent"]);
    bus.local.force_resync_after_initial_subscribe();

    assert!(matches!(
        receiver.recv().await,
        Err(broadcast::error::RecvError::Lagged(0))
    ));
    assert_eq!(bus.local.subscriber_gaps.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn scoped_resync_flood_lags_only_the_affected_game() {
    let bus = EventBus::local();
    let mut quiet = bus.subscribe_game_targets(7, &["ReceivedGameEvent"]);
    let mut affected = bus.subscribe_game_targets(8, &["ReceivedGameEvent"]);

    for _ in 0..64 {
        bus.local
            .force_resync_for_partition("ReceivedGameEvent", Some(8));
    }
    bus.publish(received_game_event(7, 11, 19));

    let quiet_event = quiet.recv().await.unwrap();
    assert_eq!(quiet_event.game_id, Some(7));
    assert!(matches!(
        affected.recv().await,
        Err(broadcast::error::RecvError::Lagged(0))
    ));
}

#[tokio::test]
async fn full_distributed_queue_advances_the_resync_generation() {
    let bus = test_distributed_bus(OutboundLimits {
        total_events: 2,
        partition_events: 1,
        partitions: 2,
        total_bytes: MAX_WIRE_BYTES * 2,
        partition_bytes: MAX_WIRE_BYTES,
    });
    bus.publish(received_game_event(7, 1, 1));
    bus.publish(received_game_event(7, 2, 2));

    assert_eq!(bus.local.distributed_drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        bus.local
            .distributed_loss_generation
            .load(Ordering::Acquire),
        1
    );
    assert_eq!(
        bus.operational_metrics(),
        EventBusOperationalMetrics {
            active_target_queues: 0,
            lagged_receivers: 0,
            rejected_events: 0,
            distributed_drops: 1,
            distributed_loss_generation: 1,
            subscriber_gaps: 0,
        }
    );
}

#[tokio::test]
async fn partitioned_outbox_isolates_noisy_games_and_dequeues_fairly() {
    let local = LocalFanout::new();
    let outbox = DistributedOutbox::new(OutboundLimits {
        total_events: 4,
        partition_events: 2,
        partitions: 2,
        total_bytes: MAX_WIRE_BYTES * 4,
        partition_bytes: MAX_WIRE_BYTES * 2,
    });
    let noisy_one = received_game_event(7, 1, 1);
    let noisy_two = received_game_event(7, 2, 2);
    let noisy_overflow = received_game_event(7, 3, 3);
    let quiet = received_game_event(8, 4, 4);

    outbox.try_push(prepared(&noisy_one), &local).unwrap();
    outbox.try_push(prepared(&noisy_two), &local).unwrap();
    assert_eq!(
        outbox.try_push(prepared(&noisy_overflow), &local),
        Err("outbound_partition_full")
    );
    outbox.try_push(prepared(&quiet), &local).unwrap();

    let mut game_order = Vec::new();
    for _ in 0..3 {
        let OutboundWork::Event(event) = outbox.next_work().await else {
            panic!("no loss barrier expected");
        };
        game_order.push(decode_queued(event).game_id.unwrap());
    }
    assert_eq!(game_order, [7, 8, 7]);
}

#[test]
fn continuously_losing_partition_cannot_starve_an_unrelated_event() {
    let local = LocalFanout::new();
    let outbox = DistributedOutbox::new(OutboundLimits::default());
    let noisy_key = OutboundKey {
        target: "ReceivedGameEvent",
        game_id: Some(8),
    };
    outbox
        .try_push(prepared(&received_game_event(7, 1, 1)), &local)
        .unwrap();
    outbox.record_loss(noisy_key, &local, "noisy_partition_loss");

    let mut state = outbox.lock_state();
    let OutboundWork::Barrier(first) = state.pop_work().unwrap() else {
        panic!("the first scoped barrier should be ready");
    };
    assert_eq!(first.key, Some(noisy_key));
    let generation = local.record_distributed_drop("repeated_noisy_partition_loss");
    state.record_loss(noisy_key, generation);

    let OutboundWork::Event(quiet) = state.pop_work().unwrap() else {
        panic!("quiet partition must receive the next fair turn");
    };
    assert_eq!(decode_queued(quiet).game_id, Some(7));
}

#[tokio::test]
async fn outbox_total_count_and_byte_limits_are_hard() {
    let local = LocalFanout::new();
    let outbox = DistributedOutbox::new(OutboundLimits {
        total_events: 1,
        partition_events: 2,
        partitions: 2,
        total_bytes: MAX_WIRE_BYTES * 2,
        partition_bytes: MAX_WIRE_BYTES,
    });
    outbox
        .try_push(prepared(&received_game_event(7, 1, 1)), &local)
        .unwrap();
    assert_eq!(
        outbox.try_push(prepared(&received_game_event(8, 2, 2)), &local),
        Err("outbound_total_full")
    );

    let outbox = DistributedOutbox::new(OutboundLimits {
        total_events: 2,
        partition_events: 2,
        partitions: 2,
        total_bytes: 1,
        partition_bytes: MAX_WIRE_BYTES,
    });
    assert_eq!(
        outbox.try_push(prepared(&received_game_event(7, 1, 1)), &local),
        Err("outbound_total_full")
    );

    let outbox = DistributedOutbox::new(OutboundLimits {
        total_events: 2,
        partition_events: 2,
        partitions: 2,
        total_bytes: MAX_WIRE_BYTES * 2,
        partition_bytes: 1,
    });
    assert_eq!(
        outbox.try_push(prepared(&received_game_event(7, 1, 1)), &local),
        Err("outbound_partition_full")
    );
}

#[test]
fn concurrent_different_keys_are_admitted_without_false_contention_drops() {
    let local = Arc::new(LocalFanout::new());
    let outbox = Arc::new(DistributedOutbox::new(OutboundLimits::default()));
    std::thread::scope(|scope| {
        let handles = (0..64)
            .map(|game_id| {
                let local = Arc::clone(&local);
                let outbox = Arc::clone(&outbox);
                scope.spawn(move || {
                    outbox.try_push(
                        prepared(&received_game_event(game_id, game_id, game_id.into())),
                        &local,
                    )
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
    });

    let state = outbox.lock_state();
    assert_eq!(state.events, 64);
    assert_eq!(state.partitions.len(), 64);
    assert!(state.pending_barriers.is_empty());
    assert_eq!(local.distributed_drops.load(Ordering::Relaxed), 0);
}

#[test]
fn prepared_event_waiting_on_admission_is_tagged_after_a_concurrent_loss() {
    let local = Arc::new(LocalFanout::new());
    let outbox = Arc::new(DistributedOutbox::new(OutboundLimits::default()));
    let event = received_game_event(7, 1, 1);
    let key = prepared(&event).key;
    let mut state = outbox.lock_state();

    std::thread::scope(|scope| {
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let local_for_push = Arc::clone(&local);
        let outbox_for_push = Arc::clone(&outbox);
        let handle = scope.spawn(move || {
            let prepared = prepared(&event);
            started_tx.send(()).unwrap();
            outbox_for_push.try_push(prepared, &local_for_push)
        });
        started_rx.recv().unwrap();

        let generation = local.record_distributed_drop("interleaved_test_loss");
        state.record_loss(key, generation);
        drop(state);
        handle.join().unwrap().unwrap();
    });

    let mut state = outbox.lock_state();
    let OutboundWork::Barrier(barrier) = state.pop_work().unwrap() else {
        panic!("barrier must be ordered before the waiting event");
    };
    let OutboundWork::Event(event) = state.pop_work().unwrap() else {
        panic!("waiting event must follow the barrier");
    };
    assert_eq!(barrier.generation, event.generation);
}

#[test]
fn distinct_loss_keys_coalesce_to_one_bounded_global_barrier() {
    let local = LocalFanout::new();
    let outbox = DistributedOutbox::new(OutboundLimits {
        total_events: 8,
        partition_events: 2,
        partitions: 4,
        total_bytes: MAX_WIRE_BYTES * 2,
        partition_bytes: MAX_WIRE_BYTES,
    });
    for game_id in 0..32 {
        outbox.record_loss(
            OutboundKey {
                target: "ReceivedGameEvent",
                game_id: Some(game_id),
            },
            &local,
            "distinct_test_loss",
        );
    }

    let state = outbox.lock_state();
    assert_eq!(state.pending_barriers.len(), 1);
    assert_eq!(state.pending_barriers[0].key, None);
    assert_eq!(local.distributed_drops.load(Ordering::Relaxed), 32);
}

#[tokio::test]
async fn loss_barrier_discards_dequeued_and_queued_pre_loss_events() {
    let local = LocalFanout::new();
    let outbox = DistributedOutbox::new(OutboundLimits::default());
    outbox
        .try_push(prepared(&received_game_event(7, 1, 1)), &local)
        .unwrap();
    outbox
        .try_push(prepared(&received_game_event(7, 2, 2)), &local)
        .unwrap();

    let OutboundWork::Event(dequeued_before_loss) = outbox.next_work().await else {
        panic!("event expected before loss");
    };
    outbox.record_loss(dequeued_before_loss.key, &local, "test_loss");
    let generation = local.distributed_loss_generation.load(Ordering::Acquire);
    outbox
        .try_push(
            prepared(&received_game_event(7, 3, generation as i64)),
            &local,
        )
        .unwrap();

    assert!(dequeued_before_loss.generation < generation);
    let OutboundWork::Barrier(barrier) = outbox.next_work().await else {
        panic!("loss barrier must precede post-loss data");
    };
    assert_eq!(barrier.generation, generation);
    assert_eq!(barrier.key, Some(dequeued_before_loss.key));

    let OutboundWork::Event(post_barrier) = outbox.next_work().await else {
        panic!("post-loss event expected after barrier");
    };
    assert_eq!(decode_queued(post_barrier).game_id, Some(7));
    let state = outbox.lock_state();
    assert_eq!(state.events, 0);
    assert_eq!(state.bytes, 0);
}

#[tokio::test]
async fn oversized_escaped_wire_payload_is_rejected_before_enqueue() {
    let bus = test_distributed_bus(OutboundLimits::default());
    let mut event = received_game_event(7, 1, 1);
    event.payload = serde_json::to_string(&"\"".repeat(MAX_WIRE_BYTES / 4)).unwrap();
    assert!(event.payload.len() <= MAX_HUB_PAYLOAD_BYTES);
    let target = validate_hub_event(event.target, event.game_id, &event.payload).unwrap();
    assert_eq!(
        PreparedWireEvent::from_hub(Uuid::new_v4(), target, &event).unwrap_err(),
        "wire_size_exceeded"
    );
    bus.publish(event);

    assert_eq!(bus.operational_metrics().distributed_drops, 1);
    let outbox = &bus.distributed.as_ref().unwrap().outbound;
    let state = outbox.lock_state();
    assert_eq!(state.events, 0);
    assert_eq!(state.bytes, 0);
}

#[tokio::test]
async fn noisy_target_cannot_evict_a_notice_in_the_same_game() {
    let bus = EventBus::local();
    let mut notice = bus.subscribe_game_targets(7, &["ReceivedGameNotice"]);
    for cursor in 0..(LOCAL_QUEUE_CAPACITY * 2) {
        bus.publish(received_game_event(7, cursor as i32, cursor as i64));
    }
    bus.publish(HubEvent {
        target: "ReceivedGameNotice",
        game_id: Some(7),
        payload: r#"{"id":91}"#.to_owned(),
    });

    let received = notice.recv().await.unwrap();
    assert_eq!(received.target, "ReceivedGameNotice");
    assert_eq!(received.payload, r#"{"id":91}"#);
    assert_eq!(bus.local.lagged_receivers.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn continuously_ready_target_cannot_starve_a_quiet_target() {
    let bus = EventBus::local();
    let mut receiver = bus.subscribe_game_targets(7, &["ReceivedGameEvent", "ReceivedGameNotice"]);
    for cursor in 0..LOCAL_QUEUE_CAPACITY {
        bus.publish(received_game_event(7, cursor as i32, cursor as i64));
    }
    bus.publish(HubEvent {
        target: "ReceivedGameNotice",
        game_id: Some(7),
        payload: r#"{"id":91}"#.to_owned(),
    });

    assert_eq!(receiver.recv().await.unwrap().target, "ReceivedGameEvent");
    assert_eq!(receiver.recv().await.unwrap().target, "ReceivedGameNotice");
}

#[tokio::test]
async fn idle_target_game_channels_are_removed_after_the_last_receiver() {
    let bus = EventBus::local();
    let receiver = bus.subscribe_game_targets(7, &["ReceivedAttack"]);
    assert_eq!(bus.local.active_queue_count(), 2);
    drop(receiver);
    assert_eq!(bus.local.active_queue_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_last_receiver_disconnects_remove_idle_shards() {
    for _ in 0..128 {
        let bus = EventBus::local();
        let first = bus.subscribe_game_targets(7, &["ReceivedAttack"]);
        let second = bus.subscribe_game_targets(7, &["ReceivedAttack"]);
        assert_eq!(bus.local.active_queue_count(), 2);

        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let first_drop = tokio::spawn(async move {
            first_barrier.wait().await;
            drop(first);
        });
        let second_barrier = Arc::clone(&barrier);
        let second_drop = tokio::spawn(async move {
            second_barrier.wait().await;
            drop(second);
        });
        barrier.wait().await;
        first_drop.await.unwrap();
        second_drop.await.unwrap();

        assert_eq!(bus.local.active_queue_count(), 0);
    }
}

#[path = "tests/redis.rs"]
mod redis_integration;
