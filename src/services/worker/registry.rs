use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use rsctf_worker_protocol::InventoryItem;
use rsctf_worker_protocol::{
    AckDisposition, ControlEnvelope, ControlMessage, DataStreamRequest, ObservedWorkloadState,
    SessionFence, WorkloadStatus, PROTOCOL_REVISION,
};
use tokio::sync::{mpsc, watch, RwLock};
use uuid::Uuid;

use super::data::{DataLane, WorkerDataStream};
use super::{WorkerError, WorkerResult};

mod commands;
mod inventory;
mod session;

pub(crate) use session::SessionRegistration;
use session::{InventoryProgress, SessionEntry};
pub use session::{RegistryConfig, SessionContext};
#[cfg(test)]
mod lane_tests;

use commands::{lifecycle_command, prune_expired_commands, remove_tracked_command};
#[cfg(test)]
use commands::{CommandTracker, InFlightCommand};

/// Bounded, process-local index of live worker sessions.
///
/// PostgreSQL remains the authority for placement and desired state. This map
/// only owns connections, bounded queues, and yamux lanes on the singleton
/// network/control instance.
#[derive(Clone)]
pub struct WorkerRegistry {
    config: RegistryConfig,
    sessions: Arc<RwLock<HashMap<Uuid, Arc<SessionEntry>>>>,
    inventory_bytes: Arc<AtomicUsize>,
}

impl WorkerRegistry {
    pub fn new(config: RegistryConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            inventory_bytes: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn config(&self) -> &RegistryConfig {
        &self.config
    }

    pub async fn connected_workers(&self) -> usize {
        self.sessions.read().await.len()
    }

    pub async fn is_online(&self, worker_id: Uuid) -> bool {
        self.sessions
            .read()
            .await
            .get(&worker_id)
            .is_some_and(|session| session.lease_is_current(self.config.heartbeat_lease))
    }

    pub async fn session_context(&self, worker_id: Uuid) -> Option<SessionContext> {
        self.sessions
            .read()
            .await
            .get(&worker_id)
            .filter(|session| session.lease_is_current(self.config.heartbeat_lease))
            .map(|session| session.context.clone())
    }

    #[cfg(test)]
    pub(crate) async fn register_control(
        &self,
        worker_id: Uuid,
        boot_id: Uuid,
        fence: SessionFence,
    ) -> WorkerResult<SessionRegistration> {
        self.register_authenticated_control(
            SessionContext {
                worker_id,
                boot_id,
                certificate_fingerprint_sha256: [0; 32],
                fence,
            },
            self.config.max_data_lanes_per_worker,
        )
        .await
    }

    pub(crate) async fn register_authenticated_control(
        &self,
        context: SessionContext,
        max_data_lanes: usize,
    ) -> WorkerResult<SessionRegistration> {
        if max_data_lanes > self.config.max_data_lanes_per_worker {
            return Err(WorkerError::Protocol(
                "negotiated data lane limit exceeds registry limit",
            ));
        }
        let (control_tx, control_rx) = mpsc::channel(self.config.control_queue_capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let worker_id = context.worker_id;
        let entry = Arc::new(SessionEntry::new(
            context.clone(),
            max_data_lanes,
            control_tx,
            shutdown_tx,
        ));

        let old = {
            let mut sessions = self.sessions.write().await;
            if let Some(current) = sessions.get(&worker_id) {
                if current.context.fence.session_epoch >= context.fence.session_epoch {
                    return Err(WorkerError::StaleSession);
                }
            } else if sessions.len() >= self.config.max_workers {
                return Err(WorkerError::RegistryFull);
            }
            sessions.insert(worker_id, entry)
        };
        if let Some(old) = old {
            let _ = old.shutdown.send(true);
        }

        Ok(SessionRegistration {
            context,
            outbound: control_rx,
            shutdown: shutdown_rx,
        })
    }

    pub(crate) async fn touch(&self, worker_id: Uuid, fence: &SessionFence) -> WorkerResult<()> {
        let session = self.current(worker_id, fence).await?;
        session.touch();
        Ok(())
    }

    pub async fn send(&self, worker_id: Uuid, body: ControlMessage) -> WorkerResult<Uuid> {
        if !matches!(
            &body,
            ControlMessage::InventoryRequest(_)
                | ControlMessage::EnsureWorkload(_)
                | ControlMessage::EnsureAbsent(_)
                | ControlMessage::WriteFlag(_)
        ) {
            return Err(WorkerError::Protocol(
                "server attempted to send a worker-only control message",
            ));
        }
        let session = self
            .sessions
            .read()
            .await
            .get(&worker_id)
            .cloned()
            .ok_or(WorkerError::Offline)?;
        if !session.lease_is_current(self.config.heartbeat_lease) {
            return Err(WorkerError::Offline);
        }
        let message_id = Uuid::new_v4();
        let tracked = lifecycle_command(&body, message_id);
        if let Some(command) = &tracked {
            let mut tracker = session.commands.lock().await;
            prune_expired_commands(&mut tracker, Instant::now());
            if let Some(existing_id) = tracker.by_workload.get(&command.fence.workload_id).copied()
            {
                if tracker.by_id.contains_key(&existing_id) {
                    return Err(WorkerError::Busy);
                }
                tracker.by_id.remove(&existing_id);
                tracker.by_workload.remove(&command.fence.workload_id);
            }
            if tracker.by_id.len() >= self.config.max_in_flight_commands_per_worker {
                return Err(WorkerError::Busy);
            }
            tracker
                .by_workload
                .insert(command.fence.workload_id, command.command_id);
            tracker.by_id.insert(command.command_id, command.clone());
        }
        let envelope = ControlEnvelope {
            protocol_revision: PROTOCOL_REVISION,
            message_id,
            reply_to: None,
            session_epoch: session.context.fence.session_epoch,
            body,
        };
        if let Err(error) = session.control.try_send(envelope) {
            if let Some(command) = tracked {
                let mut tracker = session.commands.lock().await;
                remove_tracked_command(&mut tracker, &command);
            }
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => WorkerError::Busy,
                mpsc::error::TrySendError::Closed(_) => WorkerError::Offline,
            });
        }
        Ok(message_id)
    }

    /// Consume acknowledgements/results for lifecycle commands. A failed
    /// result becomes a fenced Failed observation so create callers fail fast
    /// with the runtime's real error rather than waiting for a generic timeout.
    pub(crate) async fn handle_command_feedback(
        &self,
        context: &SessionContext,
        envelope: &ControlEnvelope,
    ) -> WorkerResult<Option<WorkloadStatus>> {
        let (command_id, remove, failure) = match &envelope.body {
            ControlMessage::CommandAck(ack) => (
                ack.command_id,
                ack.disposition != AckDisposition::Accepted,
                None,
            ),
            ControlMessage::CommandResult(result) => (
                result.command_id,
                true,
                (!result.success).then_some(&result.error),
            ),
            _ => return Ok(None),
        };
        let session = self.current(context.worker_id, &context.fence).await?;
        let mut tracker = session.commands.lock().await;
        let Some(command) = tracker.by_id.get(&command_id).cloned() else {
            // Inventory requests also produce command feedback but are tracked
            // by snapshot ID rather than the lifecycle-command table.
            return Ok(None);
        };
        if envelope.reply_to != Some(command.message_id) {
            return Err(WorkerError::Protocol(
                "worker command feedback does not match its request",
            ));
        }
        if remove {
            remove_tracked_command(&mut tracker, &command);
        }
        let Some(error) = failure else {
            return Ok(None);
        };
        let detail = error
            .as_ref()
            .map(|error| error.message.as_str())
            .unwrap_or("worker command failed");
        Ok(Some(WorkloadStatus {
            fence: command.fence,
            spec_hash: command.spec_hash,
            state: ObservedWorkloadState::Failed,
            replicas: Vec::new(),
            detail: Some(detail.chars().take(4_096).collect()),
        }))
    }

    pub(crate) async fn observe_workload_status(
        &self,
        context: &SessionContext,
        status: &WorkloadStatus,
    ) -> WorkerResult<()> {
        let session = self.current(context.worker_id, &context.fence).await?;
        let mut tracker = session.commands.lock().await;
        let Some(command_id) = tracker.by_workload.get(&status.fence.workload_id).copied() else {
            return Ok(());
        };
        let matching = tracker.by_id.get(&command_id).is_some_and(|command| {
            command.fence == status.fence && command.spec_hash == status.spec_hash
        });
        if matching {
            if let Some(command) = tracker.by_id.get(&command_id).cloned() {
                remove_tracked_command(&mut tracker, &command);
            }
        }
        Ok(())
    }

    pub(crate) async fn register_lane(
        &self,
        worker_id: Uuid,
        fence: &SessionFence,
        lane_number: u16,
        lane: DataLane,
    ) -> WorkerResult<watch::Receiver<bool>> {
        let session = self.lane_session(worker_id, fence, lane_number).await?;
        let old = {
            let mut lanes = session.lanes.write().await;
            if !lanes.contains_key(&lane_number) && lanes.len() >= session.max_data_lanes {
                return Err(WorkerError::RegistryFull);
            }
            lanes.insert(lane_number, lane)
        };
        if let Some(old) = old {
            old.shutdown();
        }
        Ok(session.shutdown.subscribe())
    }

    async fn lane_session(
        &self,
        worker_id: Uuid,
        fence: &SessionFence,
        lane_number: u16,
    ) -> WorkerResult<Arc<SessionEntry>> {
        let session = self.current(worker_id, fence).await?;
        if usize::from(lane_number) >= session.max_data_lanes {
            return Err(WorkerError::Protocol("data lane number exceeds limit"));
        }
        Ok(session)
    }

    pub(crate) async fn remove_lane_if(
        &self,
        worker_id: Uuid,
        fence: &SessionFence,
        lane_number: u16,
        lane_id: Uuid,
    ) {
        let Ok(session) = self.current(worker_id, fence).await else {
            return;
        };
        let mut lanes = session.lanes.write().await;
        if lanes
            .get(&lane_number)
            .is_some_and(|lane| lane.id() == lane_id)
        {
            lanes.remove(&lane_number);
        }
    }

    pub(crate) async fn open_stream(
        &self,
        context: &SessionContext,
        request: DataStreamRequest,
    ) -> WorkerResult<WorkerDataStream> {
        let session = self.current(context.worker_id, &context.fence).await?;
        if !session.lease_is_current(self.config.heartbeat_lease) {
            return Err(WorkerError::Offline);
        }
        let lanes = session
            .lanes
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        if lanes.is_empty() {
            return Err(WorkerError::Offline);
        }
        let offset = session.next_lane.fetch_add(1, Ordering::Relaxed);
        let candidates = lanes
            .iter()
            .cycle()
            .skip(offset % lanes.len())
            .take(lanes.len());
        for lane in candidates {
            match lane.open(request.clone()).await {
                Ok(stream) => return Ok(stream),
                Err(WorkerError::Busy | WorkerError::Offline) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(WorkerError::Busy)
    }

    pub(crate) async fn remove_control_if(&self, worker_id: Uuid, fence: &SessionFence) -> bool {
        let removed = {
            let mut sessions = self.sessions.write().await;
            if sessions
                .get(&worker_id)
                .is_some_and(|entry| entry.context.fence == *fence)
            {
                sessions.remove(&worker_id)
            } else {
                None
            }
        };
        if let Some(removed) = removed {
            let _ = removed.shutdown.send(true);
            true
        } else {
            false
        }
    }

    pub async fn disconnect(&self, worker_id: Uuid) -> bool {
        let removed = self.sessions.write().await.remove(&worker_id);
        if let Some(removed) = removed {
            let _ = removed.shutdown.send(true);
            true
        } else {
            false
        }
    }

    pub async fn shutdown(&self) {
        let sessions = {
            let mut sessions = self.sessions.write().await;
            sessions.drain().map(|(_, value)| value).collect::<Vec<_>>()
        };
        for session in sessions {
            let _ = session.shutdown.send(true);
        }
    }

    async fn current(
        &self,
        worker_id: Uuid,
        fence: &SessionFence,
    ) -> WorkerResult<Arc<SessionEntry>> {
        let session = self
            .sessions
            .read()
            .await
            .get(&worker_id)
            .cloned()
            .ok_or(WorkerError::Offline)?;
        if session.context.fence != *fence {
            return Err(WorkerError::StaleSession);
        }
        Ok(session)
    }
}

fn now_millis() -> u64 {
    static STARTED_AT: OnceLock<Instant> = OnceLock::new();
    STARTED_AT
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsctf_worker_protocol::{
        InventoryPage, InventoryRequest, ObservedWorkloadState, WorkloadFence,
    };

    fn fence(epoch: u64) -> SessionFence {
        SessionFence {
            session_id: Uuid::new_v4(),
            session_epoch: epoch,
        }
    }

    #[tokio::test]
    async fn newer_control_session_supersedes_old_one() {
        let worker = Uuid::new_v4();
        let registry = WorkerRegistry::new(RegistryConfig::default());
        let mut first = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let second_fence = fence(2);
        let _second = registry
            .register_control(worker, Uuid::new_v4(), second_fence)
            .await
            .unwrap();

        first.shutdown.changed().await.unwrap();
        assert!(*first.shutdown.borrow());
        assert_eq!(
            registry.session_context(worker).await.unwrap().fence,
            second_fence
        );
    }

    #[tokio::test]
    async fn stale_teardown_cannot_remove_replacement() {
        let worker = Uuid::new_v4();
        let registry = WorkerRegistry::new(RegistryConfig::default());
        let first = fence(1);
        registry
            .register_control(worker, Uuid::new_v4(), first)
            .await
            .unwrap();
        let second = fence(2);
        registry
            .register_control(worker, Uuid::new_v4(), second)
            .await
            .unwrap();

        assert!(!registry.remove_control_if(worker, &first).await);
        assert_eq!(
            registry.session_context(worker).await.unwrap().fence,
            second
        );
    }

    #[tokio::test]
    async fn registry_and_control_queue_are_bounded() {
        let config = RegistryConfig {
            max_workers: 1,
            control_queue_capacity: 1,
            ..RegistryConfig::default()
        };
        let registry = WorkerRegistry::new(config);
        let worker = Uuid::new_v4();
        let _registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        assert!(matches!(
            registry
                .register_control(Uuid::new_v4(), Uuid::new_v4(), fence(1))
                .await,
            Err(WorkerError::RegistryFull)
        ));

        let message = || {
            ControlMessage::InventoryRequest(InventoryRequest {
                command_id: Uuid::new_v4(),
                snapshot_id: Uuid::new_v4(),
            })
        };
        registry.send(worker, message()).await.unwrap();
        assert!(matches!(
            registry.send(worker, message()).await,
            Err(WorkerError::Busy)
        ));
    }

    #[tokio::test]
    async fn heartbeat_lease_expires() {
        let config = RegistryConfig {
            heartbeat_lease: Duration::from_millis(1),
            ..RegistryConfig::default()
        };
        let registry = WorkerRegistry::new(config);
        let worker = Uuid::new_v4();
        registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(!registry.is_online(worker).await);
    }

    #[tokio::test]
    async fn inventory_is_delivered_only_when_complete_and_ordered() {
        let registry = WorkerRegistry::new(RegistryConfig::default());
        let worker = Uuid::new_v4();
        let mut registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let snapshot = request_snapshot(&registry, worker, &mut registration).await;
        let first = InventoryPage {
            snapshot_id: snapshot,
            page: 0,
            final_page: false,
            items: vec![inventory_item()],
        };
        assert!(registry
            .collect_inventory(&registration.context, first)
            .await
            .unwrap()
            .is_none());
        let last = InventoryPage {
            snapshot_id: snapshot,
            page: 1,
            final_page: true,
            items: vec![inventory_item()],
        };
        let (completed_snapshot, items) = registry
            .collect_inventory(&registration.context, last)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed_snapshot, snapshot);
        assert_eq!(items.len(), 2);
        registry
            .complete_inventory(&registration.context, snapshot)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn inventory_rejects_out_of_order_pages() {
        let registry = WorkerRegistry::new(RegistryConfig::default());
        let worker = Uuid::new_v4();
        let mut registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let snapshot = request_snapshot(&registry, worker, &mut registration).await;
        let result = registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 1,
                    final_page: true,
                    items: Vec::new(),
                },
            )
            .await;
        assert!(matches!(result, Err(WorkerError::Protocol(_))));
    }

    #[tokio::test]
    async fn inventory_snapshot_has_cumulative_page_byte_and_replica_limits() {
        let one_item_bytes = serde_json::to_vec(&vec![inventory_item()]).unwrap().len();
        let config = RegistryConfig {
            max_inventory_pages: 2,
            max_inventory_bytes: one_item_bytes + 1,
            max_inventory_replicas: 1,
            ..RegistryConfig::default()
        };
        let registry = WorkerRegistry::new(config);
        let worker = Uuid::new_v4();
        let mut registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let snapshot = request_snapshot(&registry, worker, &mut registration).await;
        registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 0,
                    final_page: false,
                    items: vec![inventory_item()],
                },
            )
            .await
            .unwrap();
        let too_many_bytes = registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 1,
                    final_page: true,
                    items: vec![inventory_item()],
                },
            )
            .await;
        assert!(matches!(too_many_bytes, Err(WorkerError::Protocol(_))));

        let snapshot = request_snapshot(&registry, worker, &mut registration).await;
        let mut item = inventory_item();
        item.replicas = (0..2)
            .map(|replica| rsctf_worker_protocol::ReplicaStatus {
                service: "challenge".into(),
                replica,
                ready: true,
                runtime_id: None,
                detail: None,
            })
            .collect();
        let too_many_replicas = registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 0,
                    final_page: true,
                    items: vec![item],
                },
            )
            .await;
        assert!(matches!(too_many_replicas, Err(WorkerError::Protocol(_))));
    }

    #[tokio::test]
    async fn inventory_rejects_empty_non_final_pages_and_page_floods() {
        let config = RegistryConfig {
            max_inventory_pages: 1,
            ..RegistryConfig::default()
        };
        let registry = WorkerRegistry::new(config);
        let worker = Uuid::new_v4();
        let mut registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let snapshot = request_snapshot(&registry, worker, &mut registration).await;
        let empty = registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 0,
                    final_page: false,
                    items: Vec::new(),
                },
            )
            .await;
        assert!(matches!(empty, Err(WorkerError::Protocol(_))));

        let snapshot = request_snapshot(&registry, worker, &mut registration).await;
        registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 0,
                    final_page: false,
                    items: vec![inventory_item()],
                },
            )
            .await
            .unwrap();
        let page_flood = registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 1,
                    final_page: true,
                    items: vec![inventory_item()],
                },
            )
            .await;
        assert!(matches!(page_flood, Err(WorkerError::Protocol(_))));
    }

    #[tokio::test]
    async fn inventory_request_is_suppressed_until_apply_completes() {
        let registry = WorkerRegistry::new(RegistryConfig::default());
        let worker = Uuid::new_v4();
        let mut registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let snapshot = request_snapshot(&registry, worker, &mut registration).await;
        assert!(!registry
            .request_inventory(worker, Duration::from_secs(90))
            .await
            .unwrap());
        registry
            .collect_inventory(
                &registration.context,
                InventoryPage {
                    snapshot_id: snapshot,
                    page: 0,
                    final_page: true,
                    items: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert!(!registry
            .request_inventory(worker, Duration::from_secs(90))
            .await
            .unwrap());
        registry
            .complete_inventory(&registration.context, snapshot)
            .await
            .unwrap();
        assert!(registry
            .request_inventory(worker, Duration::from_secs(90))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn lifecycle_command_is_suppressed_until_result() {
        let registry = WorkerRegistry::new(RegistryConfig::default());
        let worker = Uuid::new_v4();
        let mut registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let workload = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 1,
        };
        let command_id = Uuid::new_v4();
        let command = || {
            ControlMessage::EnsureAbsent(rsctf_worker_protocol::EnsureAbsent {
                command_id,
                fence: workload,
                spec_hash: "a".repeat(64),
                timeout_ms: 60_000,
            })
        };
        registry.send(worker, command()).await.unwrap();
        assert!(matches!(
            registry.send(worker, command()).await,
            Err(WorkerError::Busy)
        ));
        let sent = registration.outbound.recv().await.unwrap();
        let failed = ControlEnvelope {
            reply_to: Some(sent.message_id),
            ..ControlEnvelope::new(
                1,
                ControlMessage::CommandResult(rsctf_worker_protocol::CommandResult {
                    command_id,
                    success: false,
                    error: Some(rsctf_worker_protocol::CommandError {
                        code: rsctf_worker_protocol::CommandErrorCode::NotFound,
                        message: "image is unavailable".into(),
                        failed_replicas: Vec::new(),
                    }),
                }),
            )
        };
        let status = registry
            .handle_command_feedback(&registration.context, &failed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.state, ObservedWorkloadState::Failed);
        assert_eq!(status.fence, workload);
        assert!(registry.send(worker, command()).await.is_ok());
    }

    #[tokio::test]
    async fn lifecycle_command_tracking_is_bounded_per_worker() {
        let registry = WorkerRegistry::new(RegistryConfig {
            max_in_flight_commands_per_worker: 1,
            ..RegistryConfig::default()
        });
        let worker = Uuid::new_v4();
        let _registration = registry
            .register_control(worker, Uuid::new_v4(), fence(1))
            .await
            .unwrap();
        let command = |workload_id| {
            ControlMessage::EnsureAbsent(rsctf_worker_protocol::EnsureAbsent {
                command_id: Uuid::new_v4(),
                fence: WorkloadFence {
                    workload_id,
                    assignment_id: Uuid::new_v4(),
                    generation: 1,
                },
                spec_hash: "a".repeat(64),
                timeout_ms: 60_000,
            })
        };

        registry
            .send(worker, command(Uuid::new_v4()))
            .await
            .unwrap();
        assert!(matches!(
            registry.send(worker, command(Uuid::new_v4())).await,
            Err(WorkerError::Busy)
        ));
    }

    #[test]
    fn expired_commands_are_removed_from_both_indexes() {
        let workload_id = Uuid::new_v4();
        let command_id = Uuid::new_v4();
        let command = InFlightCommand {
            command_id,
            message_id: Uuid::new_v4(),
            fence: WorkloadFence {
                workload_id,
                assignment_id: Uuid::new_v4(),
                generation: 1,
            },
            spec_hash: "b".repeat(64),
            deadline: Instant::now().checked_sub(Duration::from_secs(1)).unwrap(),
        };
        let mut tracker = CommandTracker::default();
        tracker.by_id.insert(command_id, command);
        tracker.by_workload.insert(workload_id, command_id);

        prune_expired_commands(&mut tracker, Instant::now());

        assert!(tracker.by_id.is_empty());
        assert!(tracker.by_workload.is_empty());
    }

    async fn request_snapshot(
        registry: &WorkerRegistry,
        worker: Uuid,
        registration: &mut SessionRegistration,
    ) -> Uuid {
        assert!(registry
            .request_inventory(worker, Duration::from_secs(90))
            .await
            .unwrap());
        let envelope = registration.outbound.recv().await.unwrap();
        let ControlMessage::InventoryRequest(request) = envelope.body else {
            panic!("expected inventory request");
        };
        request.snapshot_id
    }

    fn inventory_item() -> InventoryItem {
        InventoryItem {
            fence: WorkloadFence {
                workload_id: Uuid::new_v4(),
                assignment_id: Uuid::new_v4(),
                generation: 1,
            },
            spec_hash: "0".repeat(64),
            state: ObservedWorkloadState::Ready,
            replicas: Vec::new(),
        }
    }
}
