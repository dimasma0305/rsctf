//! Best-effort real-time hub event fanout.
//!
//! A local [`tokio::sync::broadcast`] channel remains the only path to WebSocket
//! clients. In replica mode, Redis Pub/Sub mirrors each process' locally-published
//! events to the other processes, whose subscribers inject them into their own
//! local channel. Database correctness must never depend on this service: queues
//! are bounded, slow consumers may lag, and messages published during a Redis
//! outage may be dropped.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::app_state::HubEvent;

const LOCAL_QUEUE_CAPACITY: usize = 512;
const OUTBOUND_QUEUE_CAPACITY: usize = 512;
const DEDUP_CAPACITY: usize = 4_096;
const MAX_WIRE_BYTES: usize = 256 * 1024;
const REDIS_IO_TIMEOUT: Duration = Duration::from_millis(750);
const REDIS_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REDIS_RETRY_MIN: Duration = Duration::from_secs(1);
const REDIS_RETRY_MAX: Duration = Duration::from_secs(10);

/// Default channel for installations that use Redis only for one RSCTF cluster.
pub const DEFAULT_REDIS_CHANNEL: &str = "rsctf:hub-events:v1";

/// Cloneable hub-event handle. Publishing is synchronous and non-blocking: the
/// event reaches this process immediately and is offered to a bounded Redis
/// publisher queue when distributed fanout is enabled.
#[derive(Clone)]
pub struct EventBus {
    local: broadcast::Sender<HubEvent>,
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
}

/// Redis is not an authorization boundary. Still, accept only methods the
/// server itself can publish, both to reject malformed messages and to retain
/// the allocation-free `&'static str` target used by every WebSocket hot path.
fn known_target(target: &str) -> Option<&'static str> {
    match target {
        "ReceivedAttack" => Some("ReceivedAttack"),
        "ReceivedGameNotice" => Some("ReceivedGameNotice"),
        "ReceivedSubmissions" => Some("ReceivedSubmissions"),
        "InternalByocRevokeParticipation" => Some("InternalByocRevokeParticipation"),
        "InternalByocRevokeChallenge" => Some("InternalByocRevokeChallenge"),
        "InternalTrafficCaptureReconcile" => Some("InternalTrafficCaptureReconcile"),
        _ => None,
    }
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
        let (local, _) = broadcast::channel(LOCAL_QUEUE_CAPACITY);
        Self {
            local,
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
        let (local, _) = broadcast::channel(LOCAL_QUEUE_CAPACITY);
        let (outbound, outbound_rx) = mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        let origin = Uuid::new_v4();
        let channel = channel.to_string();

        let publisher = tokio::spawn(run_publisher(client.clone(), channel.clone(), outbound_rx));
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
        self.local.subscribe()
    }

    /// Publish locally, then offer the same event to other replicas. A full or
    /// unavailable distributed queue drops only remote fanout; local clients are
    /// never delayed by Redis.
    pub fn publish(&self, event: HubEvent) {
        let wire = self
            .distributed
            .as_ref()
            .map(|distributed| WireEvent::from_hub(distributed.origin, &event));
        let _ = self.local.send(event);
        if let (Some(distributed), Some(wire)) = (&self.distributed, wire) {
            if distributed.outbound.try_send(wire).is_err() {
                tracing::debug!("dropping remote hub event: publisher queue unavailable");
            }
        }
    }

    pub fn is_distributed(&self) -> bool {
        self.distributed.is_some()
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
    mut outbound: mpsc::Receiver<WireEvent>,
) {
    let mut connection: Option<redis::aio::ConnectionManager> = None;
    while let Some(event) = outbound.recv().await {
        let Ok(payload) = serde_json::to_vec(&event) else {
            continue;
        };
        if payload.len() > MAX_WIRE_BYTES {
            tracing::debug!(bytes = payload.len(), "dropping oversized remote hub event");
            continue;
        }

        if connection.is_none() {
            connection = match tokio::time::timeout(
                REDIS_CONNECT_TIMEOUT,
                crate::utils::redis::connection_manager(&client),
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
            continue;
        };
        let result = tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            redis::cmd("PUBLISH")
                .arg(&channel)
                .arg(payload)
                .query_async::<i64>(&mut active),
        )
        .await;
        match result {
            Ok(Ok(_)) => connection = Some(active),
            Ok(Err(error)) => {
                tracing::debug!(%error, "hub event publish failed; reconnecting on next event");
            }
            Err(_) => {
                tracing::debug!("hub event publish timed out; reconnecting on next event");
            }
        }
    }
}

async fn run_subscriber(
    client: redis::Client,
    channel: String,
    origin: Uuid,
    local: broadcast::Sender<HubEvent>,
) {
    let mut dedup = InboundDedup::new(origin);
    let mut retry = REDIS_RETRY_MIN;
    loop {
        let connected =
            tokio::time::timeout(REDIS_CONNECT_TIMEOUT, client.get_async_pubsub()).await;
        let mut pubsub = match connected {
            Ok(Ok(pubsub)) => pubsub,
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
        let subscribed = tokio::time::timeout(REDIS_IO_TIMEOUT, pubsub.subscribe(&channel)).await;
        if !matches!(subscribed, Ok(Ok(()))) {
            tracing::debug!("hub event Redis subscription failed");
            tokio::time::sleep(retry).await;
            retry = retry.saturating_mul(2).min(REDIS_RETRY_MAX);
            continue;
        }

        retry = REDIS_RETRY_MIN;
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
                let _ = local.send(event);
            }
        }

        tracing::debug!("hub event Redis subscription ended; reconnecting");
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
    fn internal_control_targets_are_valid_distributed_events() {
        for target in [
            "InternalByocRevokeParticipation",
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

    /// Run explicitly with `RSCTF_TEST_REDIS_URL=redis://... cargo test
    /// redis_bus_fans_out_once_between_processes -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "requires RSCTF_TEST_REDIS_URL and a reachable Redis server"]
    async fn redis_bus_fans_out_once_between_processes() {
        let url = std::env::var("RSCTF_TEST_REDIS_URL").expect("RSCTF_TEST_REDIS_URL");
        let channel = format!("rsctf:test:hub-events:{}", Uuid::new_v4());
        let sender = EventBus::distributed_on(&url, &channel).unwrap();
        let receiver_bus = EventBus::distributed_on(&url, &channel).unwrap();
        let mut sender_local = sender.subscribe();
        let mut receiver_remote = receiver_bus.subscribe();

        // Give both best-effort subscribers time to establish their subscriptions.
        tokio::time::sleep(Duration::from_millis(150)).await;
        sender.publish(hub_event(r#"{"kind":"attack"}"#));

        let local = tokio::time::timeout(Duration::from_secs(2), sender_local.recv())
            .await
            .unwrap()
            .unwrap();
        let remote = tokio::time::timeout(Duration::from_secs(2), receiver_remote.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(local.payload, remote.payload);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), sender_local.recv())
                .await
                .is_err()
        );
    }
}
