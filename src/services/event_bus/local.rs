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
const RESYNC_QUEUE_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FanoutKey {
    game_id: Option<i32>,
    target: &'static str,
}

/// Process-local queues exist only for active target/game subscriptions. The
/// all-events queue remains for internal reconcilers that intentionally consume
/// multiple event kinds; public hubs never compete on it.
pub(super) struct LocalFanout {
    pub(super) all: broadcast::Sender<HubEvent>,
    targeted: DashMap<FanoutKey, broadcast::Sender<HubEvent>>,
    resync: broadcast::Sender<()>,
    pub(super) lagged_receivers: AtomicU64,
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
            resync,
            lagged_receivers: AtomicU64::new(0),
            distributed_drops: AtomicU64::new(0),
            distributed_loss_generation: AtomicU64::new(0),
            subscriber_gaps: AtomicU64::new(0),
        }
    }

    fn sender(&self, key: FanoutKey) -> broadcast::Sender<HubEvent> {
        self.targeted
            .entry(key)
            .or_insert_with(|| broadcast::channel(LOCAL_QUEUE_CAPACITY).0)
            .clone()
    }

    pub(super) fn subscribe(
        self: &Arc<Self>,
        game_id: Option<i32>,
        targets: &'static [&'static str],
    ) -> EventReceiver {
        let mut channels = Vec::with_capacity(targets.len() * (1 + usize::from(game_id.is_some())));
        for target in targets.iter().copied() {
            if channels
                .iter()
                .any(|channel: &TargetReceiver| channel.key.target == target)
            {
                continue;
            }
            let global_key = FanoutKey {
                game_id: None,
                target,
            };
            let global_sender = self.sender(global_key);
            channels.push(TargetReceiver {
                key: global_key,
                receiver: global_sender.subscribe(),
                sender: global_sender,
            });
            if let Some(game_id) = game_id {
                let game_key = FanoutKey {
                    game_id: Some(game_id),
                    target,
                };
                let game_sender = self.sender(game_key);
                channels.push(TargetReceiver {
                    key: game_key,
                    receiver: game_sender.subscribe(),
                    sender: game_sender,
                });
            }
        }
        EventReceiver {
            channels,
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
        let key = FanoutKey {
            game_id: event.game_id,
            target: event.target,
        };
        if let Some(sender) = self.targeted.get(&key) {
            let _ = sender.send(event);
        }
    }

    pub(super) fn record_distributed_drop(&self, reason: &'static str) {
        let dropped = self
            .distributed_drops
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        self.distributed_loss_generation
            .fetch_add(1, Ordering::Release);
        if dropped.is_power_of_two() {
            tracing::warn!(
                dropped,
                reason,
                "remote hub fanout lost data; clients must reconcile from HTTP"
            );
        }
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
        self.publish(HubEvent {
            target: RESYNC_TARGET,
            game_id: None,
            payload: serde_json::json!({ "subscriberGap": gaps }).to_string(),
        });
    }

    #[cfg(test)]
    pub(super) fn active_queue_count(&self) -> usize {
        self.targeted.len()
    }
}

struct TargetReceiver {
    key: FanoutKey,
    sender: broadcast::Sender<HubEvent>,
    receiver: broadcast::Receiver<HubEvent>,
}

/// Receiver used by public hubs. It merges only the exact target/game shards
/// requested by that hub plus a low-volume resync signal.
pub struct EventReceiver {
    channels: Vec<TargetReceiver>,
    resync: broadcast::Receiver<()>,
    fanout: Arc<LocalFanout>,
}

impl EventReceiver {
    pub async fn recv(&mut self) -> Result<HubEvent, broadcast::error::RecvError> {
        let (channels, resync) = (&mut self.channels, &mut self.resync);
        let receive_event = async {
            let pending = channels
                .iter_mut()
                .map(|channel| Box::pin(channel.receiver.recv()))
                .collect::<Vec<_>>();
            select_all(pending).await.0
        };
        let result = tokio::select! {
            result = receive_event => result,
            resync = resync.recv() => match resync {
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    Err(broadcast::error::RecvError::Lagged(0))
                }
                Err(broadcast::error::RecvError::Closed) => {
                    Err(broadcast::error::RecvError::Closed)
                }
            },
        };
        if matches!(result, Err(broadcast::error::RecvError::Lagged(_))) {
            self.fanout.lagged_receivers.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        for channel in &self.channels {
            let _ = self.fanout.targeted.remove_if(&channel.key, |_, current| {
                current.same_channel(&channel.sender) && current.receiver_count() == 1
            });
        }
    }
}
