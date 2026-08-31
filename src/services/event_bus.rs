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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Notify};
use uuid::Uuid;

use crate::app_state::HubEvent;

#[path = "event_bus/distributed.rs"]
mod distributed;
#[path = "event_bus/local.rs"]
mod local;

use distributed::{resp3_client, run_publisher, run_subscriber};
pub use local::EventReceiver;
#[cfg(test)]
use local::LOCAL_QUEUE_CAPACITY;
use local::{LocalFanout, ResyncScope};

const OUTBOUND_QUEUE_CAPACITY: usize = 512;
const OUTBOUND_PARTITION_CAPACITY: usize = 128;
const OUTBOUND_PARTITION_LIMIT: usize = OUTBOUND_QUEUE_CAPACITY;
const OUTBOUND_BYTE_CAPACITY: usize = 8 * 1024 * 1024;
const OUTBOUND_PARTITION_BYTE_CAPACITY: usize = 512 * 1024;
const DEDUP_CAPACITY: usize = 4_096;
const MAX_WIRE_BYTES: usize = 256 * 1024;
// A valid JSON value can expand again when embedded as the wire event's JSON
// string. Reject before parsing so even one malformed publication has a firm
// allocation bound on the synchronous path.
const MAX_HUB_PAYLOAD_BYTES: usize = MAX_WIRE_BYTES;
const INBOUND_PUSH_CAPACITY: usize = 512;
const REDIS_IO_TIMEOUT: Duration = Duration::from_millis(750);
const REDIS_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REDIS_RETRY_MIN: Duration = Duration::from_secs(1);
const REDIS_RETRY_MAX: Duration = Duration::from_secs(10);
const REDIS_SUBSCRIBER_HEARTBEAT: Duration = Duration::from_secs(15);
const RESYNC_TARGET: &str = "InternalFeedResyncRequired";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetScope {
    Game,
    Global,
    Resync,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TargetSpec {
    name: &'static str,
    scope: TargetScope,
}

macro_rules! target_catalog {
    ($($name:literal => $scope:ident),+ $(,)?) => {
        const KNOWN_TARGETS: &[&str] = &[$($name),+];

        fn known_target(target: &str) -> Option<TargetSpec> {
            match target {
                $($name => Some(TargetSpec {
                    name: $name,
                    scope: TargetScope::$scope,
                }),)+
                _ => None,
            }
        }
    };
}

target_catalog! {
    "ReceivedAttack" => Game,
    "ReceivedGameEvent" => Game,
    "ReceivedGameNotice" => Game,
    "ReceivedGameNoticeChanged" => Game,
    "ReceivedFlagEgress" => Game,
    "ReceivedLog" => Global,
    "ReceivedSubmissions" => Game,
    "InternalByocRevokeParticipation" => Global,
    "InternalByocRevokeTeam" => Global,
    "InternalByocRevokeChallenge" => Global,
    "InternalTrafficCaptureReconcile" => Global,
    "InternalFeedResyncRequired" => Resync,
}

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
    pub rejected_events: u64,
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
    outbound: Arc<DistributedOutbox>,
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResyncMarkerPayload {
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}

impl WireEvent {
    #[cfg(test)]
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

    fn validated_target(&self) -> Option<&'static str> {
        if self.version != 1 {
            return None;
        }
        validate_hub_event(&self.target, self.game_id, &self.payload)
    }

    #[cfg(test)]
    fn into_hub(self) -> Option<HubEvent> {
        let target = self.validated_target()?;
        Some(HubEvent {
            target,
            game_id: self.game_id,
            payload: self.payload,
        })
    }

    fn resync(origin: Uuid, barrier: ScopedBarrier) -> Self {
        let (game_id, target) = barrier
            .key
            .map(|key| (key.game_id, Some(key.target)))
            .unwrap_or((None, None));
        Self {
            version: 1,
            id: Uuid::now_v7(),
            origin,
            target: RESYNC_TARGET.to_owned(),
            game_id,
            payload: serde_json::json!({
                "generation": barrier.generation,
                "target": target,
            })
            .to_string(),
        }
    }

    fn resync_scope(&self) -> Option<ResyncScope> {
        if self.target != RESYNC_TARGET {
            return None;
        }
        let marker = serde_json::from_str::<ResyncMarkerPayload>(&self.payload).ok()?;
        let _generation = marker.generation;
        let Some(target) = marker.target else {
            return self.game_id.is_none().then_some(ResyncScope::Global);
        };
        let target = known_target(&target)?;
        if target.scope == TargetScope::Resync {
            return None;
        }
        match (target.scope, self.game_id) {
            (TargetScope::Game, Some(_)) | (TargetScope::Global, None) => {
                Some(ResyncScope::Partition {
                    target: target.name,
                    game_id: self.game_id,
                })
            }
            _ => None,
        }
    }
}

/// Redis is not an authorization boundary. Still, only exact server-owned
/// target/scope pairs carrying one complete JSON value may enter local fanout.
/// Besides rejecting malformed frames, this prevents a game event with a null
/// scope from becoming an accidental cross-game broadcast.
fn validate_hub_event(target: &str, game_id: Option<i32>, payload: &str) -> Option<&'static str> {
    if payload.len() > MAX_HUB_PAYLOAD_BYTES {
        return None;
    }
    let target = known_target(target)?;
    match (target.scope, game_id) {
        (TargetScope::Game, Some(_)) | (TargetScope::Global, None) | (TargetScope::Resync, _) => {}
        _ => return None,
    }
    let mut value = serde_json::Deserializer::from_str(payload);
    serde::de::IgnoredAny::deserialize(&mut value).ok()?;
    value.end().ok()?;
    if target.scope == TargetScope::Resync {
        let probe = WireEvent {
            version: 1,
            id: Uuid::nil(),
            origin: Uuid::nil(),
            target: RESYNC_TARGET.to_owned(),
            game_id,
            payload: payload.to_owned(),
        };
        probe.resync_scope()?;
    }
    Some(target.name)
}

fn decode_inbound_wire(payload: &[u8], local: &LocalFanout) -> Option<(WireEvent, &'static str)> {
    if payload.len() > MAX_WIRE_BYTES {
        local.record_rejected_event();
        return None;
    }
    let event = match serde_json::from_slice::<WireEvent>(payload) {
        Ok(event) => event,
        Err(_) => {
            local.record_rejected_event();
            return None;
        }
    };
    let Some(target) = event.validated_target() else {
        local.record_rejected_event();
        return None;
    };
    Some((event, target))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedWireEvent<'a> {
    version: u8,
    id: Uuid,
    origin: Uuid,
    target: &'static str,
    game_id: Option<i32>,
    payload: &'a str,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct OutboundKey {
    target: &'static str,
    game_id: Option<i32>,
}

#[derive(Debug)]
struct PreparedWireEvent {
    key: OutboundKey,
    bytes: Vec<u8>,
}

impl PreparedWireEvent {
    fn from_hub(
        origin: Uuid,
        target: &'static str,
        event: &HubEvent,
    ) -> Result<Self, &'static str> {
        let bytes = serde_json::to_vec(&BorrowedWireEvent {
            version: 1,
            id: Uuid::now_v7(),
            origin,
            target,
            game_id: event.game_id,
            payload: &event.payload,
        })
        .map_err(|_| "encode_failed")?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err("wire_size_exceeded");
        }
        Ok(Self {
            key: OutboundKey {
                target,
                game_id: event.game_id,
            },
            bytes,
        })
    }

    fn resync(origin: Uuid, barrier: ScopedBarrier) -> Result<Self, &'static str> {
        let wire = WireEvent::resync(origin, barrier);
        let bytes = serde_json::to_vec(&wire).map_err(|_| "encode_failed")?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err("wire_size_exceeded");
        }
        Ok(Self {
            key: barrier.key.unwrap_or(OutboundKey {
                target: RESYNC_TARGET,
                game_id: None,
            }),
            bytes,
        })
    }
}

#[derive(Clone, Copy)]
struct OutboundLimits {
    total_events: usize,
    partition_events: usize,
    partitions: usize,
    total_bytes: usize,
    partition_bytes: usize,
}

impl Default for OutboundLimits {
    fn default() -> Self {
        Self {
            total_events: OUTBOUND_QUEUE_CAPACITY,
            partition_events: OUTBOUND_PARTITION_CAPACITY,
            partitions: OUTBOUND_PARTITION_LIMIT,
            total_bytes: OUTBOUND_BYTE_CAPACITY,
            partition_bytes: OUTBOUND_PARTITION_BYTE_CAPACITY,
        }
    }
}

struct QueuedWireEvent {
    key: OutboundKey,
    generation: u64,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScopedBarrier {
    key: Option<OutboundKey>,
    generation: u64,
}

struct OutboundPartition {
    key: OutboundKey,
    events: VecDeque<QueuedWireEvent>,
    bytes: usize,
}

struct OutboundState {
    limits: OutboundLimits,
    partitions: VecDeque<OutboundPartition>,
    pending_barriers: VecDeque<ScopedBarrier>,
    prefer_scoped_barrier: bool,
    events: usize,
    bytes: usize,
}

impl OutboundState {
    fn new(limits: OutboundLimits) -> Self {
        Self {
            limits,
            partitions: VecDeque::with_capacity(limits.partitions),
            pending_barriers: VecDeque::with_capacity(limits.partitions),
            prefer_scoped_barrier: true,
            events: 0,
            bytes: 0,
        }
    }

    fn push(&mut self, prepared: PreparedWireEvent, generation: u64) -> Result<(), &'static str> {
        let event_bytes = prepared.bytes.len();
        if self.events >= self.limits.total_events
            || self.bytes.saturating_add(event_bytes) > self.limits.total_bytes
        {
            return Err("outbound_total_full");
        }

        if let Some(partition) = self
            .partitions
            .iter_mut()
            .find(|partition| partition.key == prepared.key)
        {
            if partition.events.len() >= self.limits.partition_events
                || partition.bytes.saturating_add(event_bytes) > self.limits.partition_bytes
            {
                return Err("outbound_partition_full");
            }
            partition.events.push_back(QueuedWireEvent {
                key: prepared.key,
                generation,
                bytes: prepared.bytes,
            });
            partition.bytes += event_bytes;
        } else {
            if self.partitions.len() >= self.limits.partitions
                || event_bytes > self.limits.partition_bytes
            {
                return Err("outbound_partition_full");
            }
            self.partitions.push_back(OutboundPartition {
                key: prepared.key,
                events: VecDeque::from([QueuedWireEvent {
                    key: prepared.key,
                    generation,
                    bytes: prepared.bytes,
                }]),
                bytes: event_bytes,
            });
        }
        self.events += 1;
        self.bytes += event_bytes;
        Ok(())
    }

    fn pop_fair(&mut self) -> Option<QueuedWireEvent> {
        let mut partition = self.partitions.pop_front()?;
        let event = partition
            .events
            .pop_front()
            .expect("outbound partitions are retained only while non-empty");
        let event_bytes = event.bytes.len();
        partition.bytes -= event_bytes;
        self.events -= 1;
        self.bytes -= event_bytes;
        if !partition.events.is_empty() {
            self.partitions.push_back(partition);
        }
        Some(event)
    }

    fn pop_fair_unblocked(&mut self) -> Option<QueuedWireEvent> {
        let partition_count = self.partitions.len();
        for _ in 0..partition_count {
            let partition = self.partitions.front()?;
            let blocked = self
                .pending_barriers
                .iter()
                .any(|barrier| barrier.key.is_none() || barrier.key == Some(partition.key));
            if !blocked {
                return self.pop_fair();
            }
            self.partitions.rotate_left(1);
        }
        None
    }

    fn discard_before(&mut self, key: Option<OutboundKey>, generation: u64) {
        for partition in &mut self.partitions {
            if key.is_some_and(|key| key != partition.key) {
                continue;
            }
            partition
                .events
                .retain(|event| event.generation >= generation);
            partition.bytes = partition.events.iter().map(|event| event.bytes.len()).sum();
        }
        self.partitions
            .retain(|partition| !partition.events.is_empty());
        self.events = self
            .partitions
            .iter()
            .map(|partition| partition.events.len())
            .sum();
        self.bytes = self
            .partitions
            .iter()
            .map(|partition| partition.bytes)
            .sum();
    }

    fn record_loss(&mut self, key: OutboundKey, generation: u64) {
        self.discard_before(Some(key), generation);
        if let Some(index) = self
            .pending_barriers
            .iter()
            .position(|barrier| barrier.key.is_none())
        {
            self.pending_barriers[index].generation =
                self.pending_barriers[index].generation.max(generation);
            self.discard_before(None, generation);
            return;
        }
        if let Some(barrier) = self
            .pending_barriers
            .iter_mut()
            .find(|barrier| barrier.key == Some(key))
        {
            barrier.generation = barrier.generation.max(generation);
        } else if self.pending_barriers.len() < self.limits.partitions {
            self.pending_barriers.push_back(ScopedBarrier {
                key: Some(key),
                generation,
            });
        } else {
            // Loss keys themselves are attacker-influenced game ids. Collapse
            // a saturated scoped set to one conservative global barrier rather
            // than letting recovery metadata grow without bound.
            let generation = self
                .pending_barriers
                .iter()
                .map(|barrier| barrier.generation)
                .fold(generation, u64::max);
            self.pending_barriers.clear();
            self.pending_barriers.push_back(ScopedBarrier {
                key: None,
                generation,
            });
            self.discard_before(None, generation);
        }
    }

    fn record_global_loss(&mut self, generation: u64) {
        self.discard_before(None, generation);
        self.pending_barriers.clear();
        self.pending_barriers.push_back(ScopedBarrier {
            key: None,
            generation,
        });
    }

    fn pop_work(&mut self) -> Option<OutboundWork> {
        if self
            .pending_barriers
            .front()
            .is_some_and(|barrier| barrier.key.is_none())
        {
            return self.pending_barriers.pop_front().map(OutboundWork::Barrier);
        }
        if self.prefer_scoped_barrier {
            if let Some(barrier) = self.pending_barriers.pop_front() {
                self.prefer_scoped_barrier = false;
                return Some(OutboundWork::Barrier(barrier));
            }
        }
        if let Some(event) = self.pop_fair_unblocked() {
            self.prefer_scoped_barrier = true;
            return Some(OutboundWork::Event(event));
        }
        self.pending_barriers.pop_front().map(|barrier| {
            self.prefer_scoped_barrier = false;
            OutboundWork::Barrier(barrier)
        })
    }

    fn event_is_stale(&self, event: &QueuedWireEvent) -> bool {
        self.pending_barriers.iter().any(|barrier| {
            (barrier.key.is_none() || barrier.key == Some(event.key))
                && barrier.generation > event.generation
        })
    }
}

enum OutboundWork {
    Barrier(ScopedBarrier),
    Event(QueuedWireEvent),
}

struct DistributedOutbox {
    state: Mutex<OutboundState>,
    ready: Notify,
}

impl DistributedOutbox {
    fn new(limits: OutboundLimits) -> Self {
        Self {
            state: Mutex::new(OutboundState::new(limits)),
            ready: Notify::new(),
        }
    }

    fn try_push(
        &self,
        prepared: PreparedWireEvent,
        local: &LocalFanout,
    ) -> Result<(), &'static str> {
        // The critical section is count/byte accounting plus at most 512
        // bounded partition probes. It never performs allocation or I/O after
        // the event has been prepared, and admission is not falsely rejected
        // merely because the publisher is popping another partition.
        let mut state = self.lock_state();
        let generation = local.distributed_loss_generation.load(Ordering::Acquire);
        state.push(prepared, generation)?;
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, OutboundState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record_loss(&self, key: OutboundKey, local: &LocalFanout, reason: &'static str) {
        let mut state = self.lock_state();
        let generation = local.record_distributed_drop(reason);
        state.record_loss(key, generation);
        drop(state);
        self.ready.notify_one();
    }

    fn record_global_loss(&self, local: &LocalFanout, reason: &'static str) {
        let mut state = self.lock_state();
        let generation = local.record_distributed_drop(reason);
        state.record_global_loss(generation);
        drop(state);
        self.ready.notify_one();
    }

    fn event_is_stale(&self, event: &QueuedWireEvent) -> bool {
        self.lock_state().event_is_stale(event)
    }

    async fn next_work(&self) -> OutboundWork {
        loop {
            // Register before inspecting state so a producer cannot notify in
            // the gap between the empty check and this task going to sleep.
            let notified = self.ready.notified();
            {
                let mut state = self.lock_state();
                if let Some(work) = state.pop_work() {
                    return work;
                }
            }
            notified.await;
        }
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
        let subscriber_client = resp3_client(&client)?;
        let local = Arc::new(LocalFanout::new());
        let outbound = Arc::new(DistributedOutbox::new(OutboundLimits::default()));
        let origin = Uuid::new_v4();
        let channel = channel.to_string();

        let publisher = tokio::spawn(run_publisher(
            client.clone(),
            channel.clone(),
            origin,
            Arc::clone(&outbound),
            local.clone(),
        ));
        let subscriber = tokio::spawn(run_subscriber(
            subscriber_client,
            channel,
            origin,
            local.clone(),
        ));
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

    /// Validate and publish locally, then offer the same event to other replicas.
    /// Redis serialization and queue admission are synchronous and bounded; no
    /// caller waits for remote I/O.
    pub fn publish(&self, event: HubEvent) {
        let Some(target) = validate_hub_event(event.target, event.game_id, &event.payload) else {
            self.local.record_rejected_event();
            return;
        };
        let event = HubEvent { target, ..event };
        let Some(distributed) = &self.distributed else {
            self.local.publish(event);
            return;
        };

        // Preserve immediate local delivery even though exact wire-size
        // accounting serializes the remote copy on the caller's thread.
        self.local.publish(event.clone());
        let prepared = match PreparedWireEvent::from_hub(distributed.origin, target, &event) {
            Ok(prepared) => prepared,
            Err(reason) => {
                distributed.outbound.record_loss(
                    OutboundKey {
                        target,
                        game_id: event.game_id,
                    },
                    &self.local,
                    reason,
                );
                return;
            }
        };
        let key = prepared.key;
        if let Err(reason) = distributed.outbound.try_push(prepared, &self.local) {
            distributed.outbound.record_loss(key, &self.local, reason);
        }
    }

    pub fn is_distributed(&self) -> bool {
        self.distributed.is_some()
    }

    pub(crate) fn operational_metrics(&self) -> EventBusOperationalMetrics {
        EventBusOperationalMetrics {
            active_target_queues: self.local.active_queue_count(),
            lagged_receivers: self.local.lagged_receivers.load(Ordering::Relaxed),
            rejected_events: self.local.rejected_events.load(Ordering::Relaxed),
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

#[cfg(test)]
#[path = "event_bus/tests.rs"]
mod tests;
