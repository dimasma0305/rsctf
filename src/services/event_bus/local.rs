//! Target- and game-scoped process-local realtime queues.
//!
//! WebSocket feeds subscribe to the exact methods they serve. This keeps a
//! burst in one event or one target out of unrelated sockets' bounded history.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use futures::future::select_all;
use tokio::sync::broadcast;

use super::RESYNC_TARGET;
use crate::app_state::HubEvent;

pub(super) const LOCAL_QUEUE_CAPACITY: usize = 512;
const RESYNC_QUEUE_CAPACITY: usize = 512;
const SCOPED_RESYNC_QUEUE_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResyncScope {
    Global,
    Partition {
        target: &'static str,
        game_id: Option<i32>,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FanoutKey {
    scope: FanoutScope,
    target: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FanoutScope {
    /// Events explicitly broadcast to every game.
    Global,
    /// Events for one exact game.
    Game(i32),
    /// An operator feed that intentionally observes every game.
    Any,
}

/// Process-local queues exist only for active target/game subscriptions. The
/// all-events queue remains for internal reconcilers that intentionally consume
/// multiple event kinds; public hubs never compete on it.
pub(super) struct LocalFanout {
    pub(super) all: broadcast::Sender<HubEvent>,
    targeted: DashMap<FanoutKey, broadcast::Sender<HubEvent>>,
    targeted_resync: DashMap<FanoutKey, broadcast::Sender<()>>,
    resync: broadcast::Sender<()>,
    pub(super) lagged_receivers: AtomicU64,
    pub(super) rejected_events: AtomicU64,
    pub(super) distributed_drops: AtomicU64,
    pub(super) distributed_loss_generation: AtomicU64,
    pub(super) subscriber_gaps: AtomicU64,
}

impl LocalFanout {
    pub(super) fn new() -> Self {
        let (all, _) = broadcast::channel(LOCAL_QUEUE_CAPACITY);
        let (resync, _) = broadcast::channel(RESYNC_QUEUE_CAPACITY);
        Self {
            all,
            targeted: DashMap::new(),
            targeted_resync: DashMap::new(),
            resync,
            lagged_receivers: AtomicU64::new(0),
            rejected_events: AtomicU64::new(0),
            distributed_drops: AtomicU64::new(0),
            distributed_loss_generation: AtomicU64::new(0),
            subscriber_gaps: AtomicU64::new(0),
        }
    }

    fn subscribe_key(&self, key: FanoutKey) -> (TargetReceiver, ScopedResyncReceiver) {
        // Create the receiver while the DashMap entry is held. Otherwise the
        // previous last receiver can remove the entry after we clone its sender
        // but before we subscribe, detaching the new receiver from publishers.
        let (sender, receiver) = {
            let sender = self
                .targeted
                .entry(key)
                .or_insert_with(|| broadcast::channel(LOCAL_QUEUE_CAPACITY).0);
            (sender.clone(), sender.subscribe())
        };
        let (resync_sender, resync_receiver) = {
            let sender = self
                .targeted_resync
                .entry(key)
                .or_insert_with(|| broadcast::channel(SCOPED_RESYNC_QUEUE_CAPACITY).0);
            (sender.clone(), sender.subscribe())
        };
        (
            TargetReceiver {
                key,
                receiver,
                sender,
            },
            ScopedResyncReceiver {
                key,
                receiver: resync_receiver,
                sender: resync_sender,
            },
        )
    }

    pub(super) fn subscribe(
        self: &Arc<Self>,
        game_id: Option<i32>,
        targets: &'static [&'static str],
    ) -> EventReceiver {
        let mut channels = Vec::with_capacity(targets.len() * (1 + usize::from(game_id.is_some())));
        let mut resync_channels = Vec::with_capacity(channels.capacity());
        for target in targets.iter().copied() {
            if channels
                .iter()
                .any(|channel: &TargetReceiver| channel.key.target == target)
            {
                continue;
            }
            if let Some(game_id) = game_id {
                let global_key = FanoutKey {
                    scope: FanoutScope::Global,
                    target,
                };
                let (channel, resync) = self.subscribe_key(global_key);
                channels.push(channel);
                resync_channels.push(resync);
                let game_key = FanoutKey {
                    scope: FanoutScope::Game(game_id),
                    target,
                };
                let (channel, resync) = self.subscribe_key(game_key);
                channels.push(channel);
                resync_channels.push(resync);
            } else {
                let any_key = FanoutKey {
                    scope: FanoutScope::Any,
                    target,
                };
                let (channel, resync) = self.subscribe_key(any_key);
                channels.push(channel);
                resync_channels.push(resync);
            }
        }
        EventReceiver {
            channels,
            resync_channels,
            resync: self.resync.subscribe(),
            fanout: Arc::clone(self),
        }
    }

    pub(super) fn publish(&self, event: HubEvent) {
        let _ = self.all.send(event.clone());
        if event.target == RESYNC_TARGET {
            let _ = self.resync.send(());
            return;
        }
        let exact_key = FanoutKey {
            scope: event.game_id.map_or(FanoutScope::Global, FanoutScope::Game),
            target: event.target,
        };
        if let Some(sender) = self.targeted.get(&exact_key) {
            let _ = sender.send(event.clone());
        }
        let any_key = FanoutKey {
            scope: FanoutScope::Any,
            target: event.target,
        };
        if let Some(sender) = self.targeted.get(&any_key) {
            let _ = sender.send(event);
        }
    }

    pub(super) fn record_distributed_drop(&self, reason: &'static str) -> u64 {
        let dropped = self
            .distributed_drops
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let generation = self
            .distributed_loss_generation
            .fetch_add(1, Ordering::Release)
            .saturating_add(1);
        if dropped.is_power_of_two() {
            tracing::warn!(
                dropped,
                reason,
                "remote hub fanout lost data; clients must reconcile from HTTP"
            );
        }
        generation
    }

    pub(super) fn record_rejected_event(&self) {
        let rejected = self
            .rejected_events
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if rejected.is_power_of_two() {
            tracing::warn!(
                rejected,
                "rejected malformed or incorrectly scoped hub event"
            );
        }
    }

    fn force_resync(&self, scope: ResyncScope) {
        match scope {
            ResyncScope::Global => {
                let _ = self.resync.send(());
            }
            ResyncScope::Partition { target, game_id } => {
                let exact_key = FanoutKey {
                    scope: game_id.map_or(FanoutScope::Global, FanoutScope::Game),
                    target,
                };
                if let Some(sender) = self.targeted_resync.get(&exact_key) {
                    let _ = sender.send(());
                }
                let any_key = FanoutKey {
                    scope: FanoutScope::Any,
                    target,
                };
                if let Some(sender) = self.targeted_resync.get(&any_key) {
                    let _ = sender.send(());
                }
            }
        }
    }

    pub(super) fn force_resync_after_initial_subscribe(&self) {
        self.force_resync(ResyncScope::Global);
    }

    pub(super) fn force_resync_global(&self) {
        self.force_resync(ResyncScope::Global);
    }

    pub(super) fn force_resync_for_partition(&self, target: &'static str, game_id: Option<i32>) {
        self.force_resync(ResyncScope::Partition { target, game_id });
    }

    pub(super) fn force_resync_after_subscriber_gap(&self) {
        let gaps = self
            .subscriber_gaps
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if gaps.is_power_of_two() {
            tracing::warn!(
                gaps,
                "Redis hub subscription recovered after possible data loss; forcing client resync"
            );
        }
        self.force_resync(ResyncScope::Global);
    }

    pub(super) fn active_queue_count(&self) -> usize {
        self.targeted.len()
    }
}

struct TargetReceiver {
    key: FanoutKey,
    sender: broadcast::Sender<HubEvent>,
    receiver: broadcast::Receiver<HubEvent>,
}

struct ScopedResyncReceiver {
    key: FanoutKey,
    sender: broadcast::Sender<()>,
    receiver: broadcast::Receiver<()>,
}

/// Receiver used by public hubs. It merges only the exact target/game shards
/// requested by that hub plus a low-volume resync signal.
pub struct EventReceiver {
    channels: Vec<TargetReceiver>,
    resync_channels: Vec<ScopedResyncReceiver>,
    resync: broadcast::Receiver<()>,
    fanout: Arc<LocalFanout>,
}

impl EventReceiver {
    pub async fn recv(&mut self) -> Result<HubEvent, broadcast::error::RecvError> {
        enum Selected {
            GlobalResync(Result<(), broadcast::error::RecvError>),
            ScopedResync(Result<(), broadcast::error::RecvError>),
            Event(Result<HubEvent, broadcast::error::RecvError>, usize),
        }

        loop {
            let selected = {
                let (channels, resync_channels, resync) = (
                    &mut self.channels,
                    &mut self.resync_channels,
                    &mut self.resync,
                );
                let receive_event = async {
                    if channels.is_empty() {
                        return std::future::pending().await;
                    }
                    let pending = channels
                        .iter_mut()
                        .map(|channel| Box::pin(channel.receiver.recv()))
                        .collect::<Vec<_>>();
                    let (result, selected, _) = select_all(pending).await;
                    (result, selected)
                };
                let receive_scoped_resync = async {
                    if resync_channels.is_empty() {
                        return std::future::pending().await;
                    }
                    let pending = resync_channels
                        .iter_mut()
                        .map(|channel| Box::pin(channel.receiver.recv()))
                        .collect::<Vec<_>>();
                    let (result, _, _) = select_all(pending).await;
                    result
                };
                tokio::select! {
                    biased;
                    resync = resync.recv() => Selected::GlobalResync(resync),
                    resync = receive_scoped_resync => Selected::ScopedResync(resync),
                    (result, index) = receive_event => Selected::Event(result, index),
                }
            };

            let result = match selected {
                Selected::GlobalResync(Ok(()))
                | Selected::GlobalResync(Err(broadcast::error::RecvError::Lagged(_)))
                | Selected::ScopedResync(Ok(()))
                | Selected::ScopedResync(Err(broadcast::error::RecvError::Lagged(_))) => {
                    Err(broadcast::error::RecvError::Lagged(0))
                }
                Selected::GlobalResync(Err(broadcast::error::RecvError::Closed))
                | Selected::ScopedResync(Err(broadcast::error::RecvError::Closed)) => {
                    Err(broadcast::error::RecvError::Closed)
                }
                Selected::Event(result, index) => {
                    // `select_all` resolves the first ready future. Rotate past
                    // the shard that won so a continuously-ready target cannot
                    // starve another target served by the same connection.
                    let channel_count = self.channels.len();
                    self.channels.rotate_left((index + 1) % channel_count);
                    result
                }
            };
            if matches!(result, Err(broadcast::error::RecvError::Lagged(_))) {
                self.fanout.lagged_receivers.fetch_add(1, Ordering::Relaxed);
            }
            return result;
        }
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        for channel in self.channels.drain(..) {
            let TargetReceiver {
                key,
                sender,
                receiver,
            } = channel;
            // Remove this receiver before testing the shared sender. Two last
            // connections may disconnect concurrently; checking while both
            // receivers are still live lets each observe two and leak the
            // now-unused shard forever.
            drop(receiver);
            let _ = self.fanout.targeted.remove_if(&key, |_, current| {
                current.same_channel(&sender) && current.receiver_count() == 0
            });
        }
        for channel in self.resync_channels.drain(..) {
            let ScopedResyncReceiver {
                key,
                sender,
                receiver,
            } = channel;
            drop(receiver);
            let _ = self.fanout.targeted_resync.remove_if(&key, |_, current| {
                current.same_channel(&sender) && current.receiver_count() == 0
            });
        }
    }
}
