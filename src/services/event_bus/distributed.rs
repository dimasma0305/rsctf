//! Bounded Redis publisher and subscriber tasks for [`super::EventBus`].

use super::*;
use std::sync::atomic::AtomicBool;
use tokio::sync::{mpsc, Notify};

pub(super) async fn run_publisher(
    client: redis::Client,
    channel: String,
    origin: Uuid,
    outbound: Arc<DistributedOutbox>,
    local: Arc<LocalFanout>,
) {
    let mut connection: Option<redis::aio::ConnectionManager> = None;
    loop {
        match outbound.next_work().await {
            OutboundWork::Barrier(barrier) => {
                let marker = match PreparedWireEvent::resync(origin, barrier) {
                    Ok(marker) => marker,
                    Err(reason) => {
                        if let Some(key) = barrier.key {
                            outbound.record_loss(key, &local, reason);
                        } else {
                            outbound.record_global_loss(&local, reason);
                        }
                        tokio::time::sleep(REDIS_RETRY_MIN).await;
                        continue;
                    }
                };
                if let Err(reason) =
                    publish_remote(&client, &channel, &marker.bytes, &mut connection).await
                {
                    if let Some(key) = barrier.key {
                        outbound.record_loss(key, &local, reason);
                    } else {
                        outbound.record_global_loss(&local, reason);
                    }
                    // Connection refusal can return immediately. Do not spin
                    // over a full outbox while Redis is unavailable.
                    tokio::time::sleep(REDIS_RETRY_MIN).await;
                    continue;
                }
            }
            OutboundWork::Event(event) => {
                // A producer may report loss after this event was dequeued.
                // Discard it; next_work drains every other pre-barrier item and
                // returns the marker before any later-generation event.
                if outbound.event_is_stale(&event) {
                    continue;
                }
                if let Err(reason) =
                    publish_remote(&client, &channel, &event.bytes, &mut connection).await
                {
                    outbound.record_loss(event.key, &local, reason);
                    tokio::time::sleep(REDIS_RETRY_MIN).await;
                }
            }
        }
    }
}

async fn publish_remote(
    client: &redis::Client,
    channel: &str,
    payload: &[u8],
    connection: &mut Option<redis::aio::ConnectionManager>,
) -> Result<(), &'static str> {
    if payload.len() > MAX_WIRE_BYTES {
        tracing::debug!(bytes = payload.len(), "dropping oversized remote hub event");
        return Err("wire_size_exceeded");
    }
    if connection.is_none() {
        *connection = match tokio::time::timeout(
            REDIS_CONNECT_TIMEOUT,
            crate::utils::redis::connection_manager(client),
        )
        .await
        {
            Ok(Ok(connection)) => Some(connection),
            Ok(Err(error)) => {
                tracing::debug!(%error, "hub event publisher could not connect to Redis");
                None
            }
            Err(_) => {
                tracing::debug!("hub event publisher Redis connection timed out");
                None
            }
        };
    }
    let Some(mut active) = connection.take() else {
        return Err("redis_unavailable");
    };
    let result = tokio::time::timeout(
        REDIS_IO_TIMEOUT,
        redis::cmd("PUBLISH")
            .arg(channel)
            .arg(payload)
            .query_async::<i64>(&mut active),
    )
    .await;
    match result {
        Ok(Ok(_)) => {
            *connection = Some(active);
            Ok(())
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "hub event publish failed; reconnecting on next event");
            Err("redis_publish_failed")
        }
        Err(_) => {
            tracing::debug!("hub event publish timed out; reconnecting on next event");
            Err("redis_publish_timeout")
        }
    }
}

pub(super) fn resp3_client(client: &redis::Client) -> redis::RedisResult<redis::Client> {
    let connection = client.get_connection_info().clone();
    let settings = connection
        .redis_settings()
        .clone()
        .set_protocol(redis::ProtocolVersion::RESP3);
    redis::Client::open(connection.set_redis_settings(settings))
}

fn bounded_pubsub_message(push: &redis::PushInfo) -> bool {
    let payload_index = match &push.kind {
        redis::PushKind::Message | redis::PushKind::SMessage => 1,
        redis::PushKind::PMessage => 2,
        _ => return false,
    };
    match push.data.get(payload_index) {
        Some(redis::Value::BulkString(payload)) => payload.len() <= MAX_WIRE_BYTES,
        Some(redis::Value::SimpleString(payload)) => payload.len() <= MAX_WIRE_BYTES,
        _ => false,
    }
}

pub(super) async fn run_subscriber(
    client: redis::Client,
    channel: String,
    origin: Uuid,
    local: Arc<LocalFanout>,
) {
    let mut dedup = InboundDedup::new(origin);
    let mut retry = REDIS_RETRY_MIN;
    let mut subscribed_once = false;
    loop {
        // redis::aio::PubSub uses an unbounded internal channel. RESP3 lets us
        // supply the push sink, so both retained message count and each
        // retained payload are bounded before this task receives them.
        let (pushes_tx, mut pushes_rx) = mpsc::channel(INBOUND_PUSH_CAPACITY);
        let push_loss = Arc::new(AtomicBool::new(false));
        let push_loss_wakeup = Arc::new(Notify::new());
        let closure_loss = Arc::clone(&push_loss);
        let closure_wakeup = Arc::clone(&push_loss_wakeup);
        let closure_local = Arc::clone(&local);
        let config = redis::AsyncConnectionConfig::new().set_push_sender(
            move |push: redis::PushInfo| -> Result<(), ()> {
                match &push.kind {
                    redis::PushKind::Message
                    | redis::PushKind::SMessage
                    | redis::PushKind::PMessage => {
                        if !bounded_pubsub_message(&push) {
                            closure_local.record_rejected_event();
                            return Ok(());
                        }
                        if pushes_tx.try_send(push).is_ok() {
                            return Ok(());
                        }
                    }
                    redis::PushKind::Disconnection => {}
                    _ => return Ok(()),
                }
                closure_loss.store(true, Ordering::Release);
                closure_wakeup.notify_one();
                Err(())
            },
        );
        let connected = tokio::time::timeout(
            REDIS_CONNECT_TIMEOUT,
            client.get_multiplexed_async_connection_with_config(&config),
        )
        .await;
        let mut connection = match connected {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                tracing::debug!(%error, "hub event subscriber could not connect to Redis");
                tokio::time::sleep(retry).await;
                retry = retry.saturating_mul(2).min(REDIS_RETRY_MAX);
                continue;
            }
            Err(_) => {
                tracing::debug!("hub event subscriber Redis connection timed out");
                tokio::time::sleep(retry).await;
                retry = retry.saturating_mul(2).min(REDIS_RETRY_MAX);
                continue;
            }
        };
        let subscribed =
            tokio::time::timeout(REDIS_IO_TIMEOUT, connection.subscribe(&channel)).await;
        if !matches!(subscribed, Ok(Ok(()))) {
            tracing::debug!("hub event Redis subscription failed");
            tokio::time::sleep(retry).await;
            retry = retry.saturating_mul(2).min(REDIS_RETRY_MAX);
            continue;
        }

        retry = REDIS_RETRY_MIN;
        if subscribed_once {
            local.force_resync_after_subscriber_gap();
        } else {
            // Clients may attach after the task spawned but before Redis
            // acknowledged SUBSCRIBE. Force an authoritative read once that
            // initial blind window is closed as well as after reconnects.
            local.force_resync_after_initial_subscribe();
            subscribed_once = true;
        }
        let mut heartbeat = tokio::time::interval(REDIS_SUBSCRIBER_HEARTBEAT);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        loop {
            if push_loss.swap(false, Ordering::AcqRel) {
                tracing::debug!("hub event Redis subscriber bounded push queue lost data");
                break;
            }
            tokio::select! {
                push = pushes_rx.recv() => {
                    let Some(push) = push else {
                        break;
                    };
                    let Some(message) = redis::Msg::from_push_info(push) else {
                        local.record_rejected_event();
                        continue;
                    };
                    if message.get_channel_name() != channel {
                        continue;
                    }
                    let Ok(payload) = message.get_payload::<Vec<u8>>() else {
                        local.record_rejected_event();
                        continue;
                    };
                    let Some((event, target)) = decode_inbound_wire(&payload, &local) else {
                        continue;
                    };
                    if !dedup.should_deliver(&event) {
                        continue;
                    }
                    if event.target == RESYNC_TARGET {
                        match event.resync_scope() {
                            Some(ResyncScope::Global) => {
                                local.force_resync_global();
                            }
                            Some(ResyncScope::Partition { target, game_id }) => {
                                local.force_resync_for_partition(target, game_id);
                            }
                            None => {}
                        }
                        continue;
                    }
                    local.publish(HubEvent {
                        target,
                        game_id: event.game_id,
                        payload: event.payload,
                    });
                }
                _ = heartbeat.tick() => {
                    let alive = tokio::time::timeout(
                        REDIS_IO_TIMEOUT,
                        redis::cmd("PING").query_async::<redis::Value>(&mut connection),
                    )
                    .await;
                    if !matches!(alive, Ok(Ok(_))) {
                        tracing::debug!("hub event Redis subscriber heartbeat failed");
                        break;
                    }
                }
                _ = push_loss_wakeup.notified() => {
                    if push_loss.load(Ordering::Acquire) {
                        tracing::debug!("hub event Redis subscriber push queue overflowed");
                        break;
                    }
                }
            }
        }

        tracing::debug!("hub event Redis subscription ended; reconnecting");
        tokio::time::sleep(retry).await;
    }
}
