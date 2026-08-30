//! Best-effort real-time hub event fanout.
//!
//! A local [`tokio::sync::broadcast`] channel remains the only path to WebSocket
//! clients. In replica mode, Redis Pub/Sub mirrors each process' locally-published
//! events to the other processes, whose subscribers inject them into their own
//! local channel. Database correctness must never depend on this service: queues
//! are bounded, slow consumers may lag, and messages published during a Redis
//! outage may be dropped.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::app_state::HubEvent;

#[path = "event_bus/local.rs"]
mod local;

pub use local::EventReceiver;
use local::LocalFanout;
#[cfg(test)]
use local::LOCAL_QUEUE_CAPACITY;

const OUTBOUND_QUEUE_CAPACITY: usize = 512;
const DEDUP_CAPACITY: usize = 4_096;
const MAX_WIRE_BYTES: usize = 256 * 1024;
const REDIS_IO_TIMEOUT: Duration = Duration::from_millis(750);
const REDIS_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REDIS_RETRY_MIN: Duration = Duration::from_secs(1);
const REDIS_RETRY_MAX: Duration = Duration::from_secs(10);
const RESYNC_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const RESYNC_TARGET: &str = "InternalFeedResyncRequired";
const KNOWN_TARGETS: &[&str] = &[
    "ReceivedAttack",
    "ReceivedGameEvent",
    "ReceivedGameNotice",
    "ReceivedGameNoticeChanged",
    "ReceivedFlagEgress",
    "ReceivedLog",
    "ReceivedSubmissions",
    "InternalByocRevokeParticipation",
    "InternalByocRevokeTeam",
    "InternalByocRevokeChallenge",
    "InternalTrafficCaptureReconcile",
    RESYNC_TARGET,
];

/// Default channel for installations that use Redis only for one RSCTF cluster.
pub const DEFAULT_REDIS_CHANNEL: &str = "rsctf:hub-events:v1";

/// Fixed-cardinality process-local visibility for fanout loss and retained
/// target/game shards. No label or map key is exposed, so untrusted game IDs
/// cannot grow the metrics response.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventBusOperationalMetrics {
    pub active_target_queues: usize,
    pub lagged_receivers: u64,
    pub distributed_drops: u64,
    pub distributed_loss_generation: u64,
    pub subscriber_gaps: u64,
}

/// Cloneable hub-event handle. Publishing is synchronous and non-blocking: the
/// event reaches this process immediately and is offered to a bounded Redis
/// publisher queue when distributed fanout is enabled.
#[derive(Clone)]
pub struct EventBus {
    local: Arc<LocalFanout>,
    distributed: Option<DistributedPublisher>,
}

#[derive(Clone)]
struct DistributedPublisher {
    origin: Uuid,
    outbound: mpsc::Sender<WireEvent>,
    // Abort the detached publisher/subscriber tasks when the final EventBus
    // handle is dropped (not when an intermediate clone is dropped).
    _tasks: Arc<TaskSet>,
}

struct TaskSet {
    handles: Vec<tokio::task::AbortHandle>,
}

impl Drop for TaskSet {
    fn drop(&mut self) {
        for handle in &self.handles {
            handle.abort();
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WireEvent {
    version: u8,
    id: Uuid,
    origin: Uuid,
    target: String,
    game_id: Option<i32>,
    payload: String,
}

impl WireEvent {
    fn from_hub(origin: Uuid, event: &HubEvent) -> Self {
        Self {
            version: 1,
            id: Uuid::now_v7(),
            origin,
            target: event.target.to_string(),
            game_id: event.game_id,
            payload: event.payload.clone(),
        }
    }

    fn into_hub(self) -> Option<HubEvent> {
        if self.version != 1 {
            return None;
        }
        let target = known_target(&self.target)?;
        Some(HubEvent {
            target,
            game_id: self.game_id,
            payload: self.payload,
        })
    }

    fn resync(origin: Uuid, generation: u64) -> Self {
        Self {
            version: 1,
            id: Uuid::now_v7(),
            origin,
            target: RESYNC_TARGET.to_owned(),
            game_id: None,
            payload: serde_json::json!({ "generation": generation }).to_string(),
        }
    }
}

/// Redis is not an authorization boundary. Still, accept only methods the
/// server itself can publish, both to reject malformed messages and to retain
/// the allocation-free `&'static str` target used by every WebSocket hot path.
fn known_target(target: &str) -> Option<&'static str> {
    KNOWN_TARGETS.iter().copied().find(|known| *known == target)
}

struct InboundDedup {
    origin: Uuid,
    ids: HashSet<Uuid>,
    order: VecDeque<Uuid>,
}

impl InboundDedup {
    fn new(origin: Uuid) -> Self {
        Self {
            origin,
            ids: HashSet::with_capacity(DEDUP_CAPACITY),
            order: VecDeque::with_capacity(DEDUP_CAPACITY),
        }
    }

    fn should_deliver(&mut self, event: &WireEvent) -> bool {
        // Redis sends a publisher its own Pub/Sub message. The local publish was
        // already delivered synchronously, so forwarding this echo would duplicate it.
        if event.origin == self.origin || self.ids.contains(&event.id) {
            return false;
        }
        if self.order.len() == DEDUP_CAPACITY {
            if let Some(expired) = self.order.pop_front() {
                self.ids.remove(&expired);
            }
        }
        self.order.push_back(event.id);
        self.ids.insert(event.id);
        true
    }
}

impl EventBus {
    /// Process-local event delivery, matching the historical single-node behavior.
    pub fn local() -> Self {
        Self {
            local: Arc::new(LocalFanout::new()),
            distributed: None,
        }
    }

    /// Start best-effort cross-replica fanout over Redis Pub/Sub.
    ///
    /// Only URL parsing and the presence of a Tokio runtime are checked here;
    /// Redis may be unavailable at startup. Both background tasks reconnect on
    /// later operations so an outage does not prevent the HTTP service starting.
    pub fn distributed(redis_url: &str) -> anyhow::Result<Self> {
        Self::distributed_on(redis_url, DEFAULT_REDIS_CHANNEL)
    }

    fn distributed_on(redis_url: &str, channel: &str) -> anyhow::Result<Self> {
        tokio::runtime::Handle::try_current()
            .map_err(|_| anyhow::anyhow!("distributed event bus requires a Tokio runtime"))?;
        let client = redis::Client::open(redis_url)?;
        let local = Arc::new(LocalFanout::new());
        let (outbound, outbound_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        let origin = Uuid::new_v4();
        let channel = channel.to_string();

        let publisher = tokio::spawn(run_publisher(
            client.clone(),
            channel.clone(),
            origin,
            outbound_rx,
            local.clone(),
        ));
        let subscriber = tokio::spawn(run_subscriber(client, channel, origin, local.clone()));
        let tasks = Arc::new(TaskSet {
            handles: vec![publisher.abort_handle(), subscriber.abort_handle()],
        });

        Ok(Self {
            local,
            distributed: Some(DistributedPublisher {
                origin,
                outbound,
                _tasks: tasks,
            }),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.local.all.subscribe()
    }

    /// Compatibility subscription for internal consumers that intentionally
    /// need every known target for one game. Public hubs should use the exact
    /// target-scoped form below.
    pub fn subscribe_game(&self, game_id: i32) -> EventReceiver {
        self.subscribe_game_targets(game_id, KNOWN_TARGETS)
    }

    pub fn subscribe_game_targets(
        &self,
        game_id: i32,
        targets: &'static [&'static str],
    ) -> EventReceiver {
        self.local.subscribe(Some(game_id), targets)
    }

    pub fn subscribe_global(&self) -> EventReceiver {
        self.subscribe_global_targets(KNOWN_TARGETS)
    }

    pub fn subscribe_global_targets(&self, targets: &'static [&'static str]) -> EventReceiver {
        self.local.subscribe(None, targets)
    }

    /// Publish locally, then offer the same event to other replicas. A full or
    /// unavailable distributed queue drops only remote fanout; local clients are
    /// never delayed by Redis.
    pub fn publish(&self, event: HubEvent) {
        let wire = self
            .distributed
            .as_ref()
            .map(|distributed| WireEvent::from_hub(distributed.origin, &event));
        self.local.publish(event);
        if let (Some(distributed), Some(wire)) = (&self.distributed, wire) {
            if distributed.outbound.try_send(wire).is_err() {
                self.local.record_distributed_drop("outbound_queue_full");
            }
        }
    }

    pub fn is_distributed(&self) -> bool {
        self.distributed.is_some()
    }

    pub(crate) fn operational_metrics(&self) -> EventBusOperationalMetrics {
        EventBusOperationalMetrics {
            active_target_queues: self.local.active_queue_count(),
            lagged_receivers: self.local.lagged_receivers.load(Ordering::Relaxed),
            distributed_drops: self.local.distributed_drops.load(Ordering::Relaxed),
            distributed_loss_generation: self
                .local
                .distributed_loss_generation
                .load(Ordering::Relaxed),
            subscriber_gaps: self.local.subscriber_gaps.load(Ordering::Relaxed),
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::local()
    }
}

async fn run_publisher(
    client: redis::Client,
    channel: String,
    origin: Uuid,
    mut outbound: mpsc::Receiver<WireEvent>,
    local: Arc<LocalFanout>,
) {
    let mut connection: Option<redis::aio::ConnectionManager> = None;
    let mut acknowledged_loss_generation = 0;
    let mut resync_retry = tokio::time::interval(RESYNC_RETRY_INTERVAL);
    resync_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    resync_retry.tick().await;
    loop {
        let event = tokio::select! {
            event = outbound.recv() => event,
            _ = resync_retry.tick() => None,
        };
        let loss_generation = local.distributed_loss_generation.load(Ordering::Acquire);
        if loss_generation > acknowledged_loss_generation {
            let marker = WireEvent::resync(origin, loss_generation);
            if publish_remote(&client, &channel, &marker, &mut connection)
                .await
                .is_err()
            {
                if event.is_some() {
                    local.record_distributed_drop("resync_marker_unavailable");
                } else if outbound.is_closed() {
                    break;
                }
                continue;
            }
            acknowledged_loss_generation = loss_generation;
        }
        let Some(event) = event else {
            if outbound.is_closed() {
                break;
            }
            continue;
        };
        if let Err(reason) = publish_remote(&client, &channel, &event, &mut connection).await {
            local.record_distributed_drop(reason);
        }
    }
}

async fn publish_remote(
    client: &redis::Client,
    channel: &str,
    event: &WireEvent,
    connection: &mut Option<redis::aio::ConnectionManager>,
) -> Result<(), &'static str> {
    let payload = serde_json::to_vec(event).map_err(|_| "encode_failed")?;
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

async fn run_subscriber(
    client: redis::Client,
    channel: String,
    origin: Uuid,
    local: Arc<LocalFanout>,
) {
    let mut dedup = InboundDedup::new(origin);
    let mut retry = REDIS_RETRY_MIN;
    let mut requires_resync = false;
    loop {
        let connected =
            tokio::time::timeout(REDIS_CONNECT_TIMEOUT, client.get_async_pubsub()).await;
        let mut pubsub = match connected {
            Ok(Ok(pubsub)) => pubsub,
            Ok(Err(error)) => {
                tracing::debug!(%error, "hub event subscriber could not connect to Redis");
                requires_resync = true;
                tokio::time::sleep(retry).await;
                retry = retry.saturating_mul(2).min(REDIS_RETRY_MAX);
                continue;
            }
            Err(_) => {
                tracing::debug!("hub event subscriber Redis connection timed out");
                requires_resync = true;
                tokio::time::sleep(retry).await;
                retry = retry.saturating_mul(2).min(REDIS_RETRY_MAX);
                continue;
            }
        };
        let subscribed = tokio::time::timeout(REDIS_IO_TIMEOUT, pubsub.subscribe(&channel)).await;
        if !matches!(subscribed, Ok(Ok(()))) {
            tracing::debug!("hub event Redis subscription failed");
            requires_resync = true;
            tokio::time::sleep(retry).await;
            retry = retry.saturating_mul(2).min(REDIS_RETRY_MAX);
            continue;
        }

        retry = REDIS_RETRY_MIN;
        if std::mem::take(&mut requires_resync) {
            local.force_resync_after_subscriber_gap();
        }
        let mut messages = pubsub.on_message();
        while let Some(message) = messages.next().await {
            let Ok(payload) = message.get_payload::<Vec<u8>>() else {
                continue;
            };
            if payload.len() > MAX_WIRE_BYTES {
                continue;
            }
            let Ok(event) = serde_json::from_slice::<WireEvent>(&payload) else {
                continue;
            };
            if !dedup.should_deliver(&event) {
                continue;
            }
            if let Some(event) = event.into_hub() {
                local.publish(event);
            }
        }

        tracing::debug!("hub event Redis subscription ended; reconnecting");
        requires_resync = true;
        tokio::time::sleep(retry).await;
    }
}

#[cfg(test)]
mod tests {
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
        barrier.payload = format!("rsctf-ready-barrier:{}", Uuid::new_v4());
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
        barrier.payload = format!("rsctf-target-ready-barrier:{}", Uuid::new_v4());
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
        bus.local.force_resync_after_subscriber_gap();

        assert!(matches!(
            receiver.recv().await,
            Err(broadcast::error::RecvError::Lagged(0))
        ));
        assert_eq!(bus.local.lagged_receivers.load(Ordering::Relaxed), 1);
        assert_eq!(bus.local.subscriber_gaps.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn full_distributed_queue_advances_the_resync_generation() {
        let local = Arc::new(LocalFanout::new());
        let (outbound, _outbound_rx) = mpsc::channel(1);
        let bus = EventBus {
            local: Arc::clone(&local),
            distributed: Some(DistributedPublisher {
                origin: Uuid::new_v4(),
                outbound,
                _tasks: Arc::new(TaskSet {
                    handles: Vec::new(),
                }),
            }),
        };
        bus.publish(received_game_event(7, 1, 1));
        bus.publish(received_game_event(7, 2, 2));

        assert_eq!(local.distributed_drops.load(Ordering::Relaxed), 1);
        assert_eq!(local.distributed_loss_generation.load(Ordering::Acquire), 1);
        assert_eq!(
            bus.operational_metrics(),
            EventBusOperationalMetrics {
                active_target_queues: 0,
                lagged_receivers: 0,
                distributed_drops: 1,
                distributed_loss_generation: 1,
                subscriber_gaps: 0,
            }
        );
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
        let mut receiver =
            bus.subscribe_game_targets(7, &["ReceivedGameEvent", "ReceivedGameNotice"]);
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

        wait_for_remote_subscription(&sender, &mut receiver_remote, hub_event("redis-ready")).await;
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
            first_replica.publish(received_game_event(8, cursor as i32, cursor as i64));
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
}
