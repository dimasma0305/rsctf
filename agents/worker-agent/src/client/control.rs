use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use rsctf_worker_protocol::{
    read_json_frame, write_json_frame, AckDisposition, CommandAck, CommandResult, ControlEnvelope,
    ControlMessage, Heartbeat, InventoryPage, InventoryRequest, ServerWelcome, WorkerHello,
    WorkloadStatus, MAX_CONTROL_FRAME,
};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{data, validate_revision, ClientError, SESSION_NEGOTIATION_TIMEOUT};
use crate::readiness::ReadinessFile;
use crate::runtime::{RuntimeError, SharedRuntime};
use crate::tls::MtlsConnector;

const OUTBOUND_QUEUE: usize = 256;
const INVENTORY_PAGE_SIZE: usize = 100;
const INVENTORY_PAYLOAD_BUDGET: usize = MAX_CONTROL_FRAME - 4 * 1024;

#[derive(Clone)]
pub struct OperationDispatcher {
    runtime: SharedRuntime,
    semaphore: Arc<Semaphore>,
    configured_limit: usize,
    active_workloads: Arc<DashMap<Uuid, Uuid>>,
}

impl OperationDispatcher {
    pub fn new(runtime: SharedRuntime, concurrency: usize) -> Self {
        Self {
            runtime,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            configured_limit: concurrency,
            active_workloads: Arc::new(DashMap::new()),
        }
    }

    pub fn dispatch(
        &self,
        envelope: &ControlEnvelope,
        outbound: mpsc::Sender<ControlEnvelope>,
        negotiated_limit: u16,
    ) -> Result<(), ClientError> {
        let command_id = envelope.body.command_id().ok_or_else(|| {
            ClientError::Protocol("received message is not a server command".to_string())
        })?;
        let reply_to = envelope.message_id;
        let epoch = envelope.session_epoch;
        let message = envelope.body.clone();
        let workload_id = message.workload_fence().map(|fence| fence.workload_id);
        if let Some(workload_id) = workload_id {
            match self.active_workloads.entry(workload_id) {
                Entry::Vacant(entry) => {
                    entry.insert(command_id);
                }
                Entry::Occupied(_) => {
                    send_busy(
                        &outbound,
                        epoch,
                        reply_to,
                        command_id,
                        "another operation for this workload is still running",
                    );
                    return Ok(());
                }
            }
        }
        let effective_limit = self.configured_limit.min(usize::from(negotiated_limit));
        let in_flight = self
            .configured_limit
            .saturating_sub(self.semaphore.available_permits());
        let permit = match (in_flight < effective_limit)
            .then(|| self.semaphore.clone().try_acquire_owned())
            .transpose()
        {
            Ok(Some(permit)) => permit,
            Ok(None) | Err(_) => {
                // Do not create an unbounded set of tasks waiting for permits.
                // The server keeps desired state durable and will retry a busy
                // command, so failing fast is both cheaper and safer.
                self.finish_workload(workload_id, command_id);
                send_busy(
                    &outbound,
                    epoch,
                    reply_to,
                    command_id,
                    "worker operation capacity is exhausted",
                );
                return Ok(());
            }
        };
        let dispatcher = self.clone();
        tokio::spawn(async move {
            let accepted = ControlEnvelope {
                reply_to: Some(reply_to),
                ..ControlEnvelope::new(
                    epoch,
                    ControlMessage::CommandAck(CommandAck {
                        command_id,
                        disposition: AckDisposition::Accepted,
                        detail: None,
                    }),
                )
            };
            if outbound.send(accepted).await.is_err() {
                dispatcher.finish_workload(workload_id, command_id);
                return;
            }
            dispatcher
                .execute(message, epoch, reply_to, outbound, permit, workload_id)
                .await;
        });
        Ok(())
    }

    async fn execute(
        &self,
        message: ControlMessage,
        epoch: u64,
        reply_to: Uuid,
        outbound: mpsc::Sender<ControlEnvelope>,
        _permit: tokio::sync::OwnedSemaphorePermit,
        workload_id: Option<Uuid>,
    ) {
        let command_id = match message.command_id() {
            Some(command_id) => command_id,
            None => return,
        };
        let outcome = match message {
            ControlMessage::InventoryRequest(request) => {
                match tokio::time::timeout(
                    Duration::from_secs(60),
                    self.send_inventory(request, epoch, reply_to, &outbound),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(RuntimeError::new(
                        rsctf_worker_protocol::CommandErrorCode::Timeout,
                        "runtime inventory timed out",
                    )),
                }
            }
            ControlMessage::EnsureWorkload(command) => {
                let timeout_ms = command.timeout_ms;
                let runtime = self.runtime.clone();
                run_timed(
                    timeout_ms,
                    async move { runtime.ensure_workload(command).await },
                )
                .await
            }
            ControlMessage::EnsureAbsent(command) => {
                let timeout_ms = command.timeout_ms;
                let runtime = self.runtime.clone();
                run_timed(
                    timeout_ms,
                    async move { runtime.ensure_absent(command).await },
                )
                .await
            }
            ControlMessage::WriteFlag(command) => {
                let timeout_ms = command.timeout_ms;
                let runtime = self.runtime.clone();
                run_timed(timeout_ms, async move { runtime.write_flag(command).await }).await
            }
            _ => Err(RuntimeError::unsupported("message is not a worker command")),
        };

        let (result, status) = match outcome {
            Ok(status) => (
                CommandResult {
                    command_id,
                    success: true,
                    error: None,
                },
                status,
            ),
            Err(error) => (
                CommandResult {
                    command_id,
                    success: false,
                    error: Some(error.as_command_error()),
                },
                None,
            ),
        };
        let result = ControlEnvelope {
            reply_to: Some(reply_to),
            ..ControlEnvelope::new(epoch, ControlMessage::CommandResult(result))
        };
        if outbound.send(result).await.is_ok() {
            if let Some(status) = status {
                let _ = outbound
                    .send(ControlEnvelope::new(
                        epoch,
                        ControlMessage::WorkloadStatus(status),
                    ))
                    .await;
            }
        }
        self.finish_workload(workload_id, command_id);
    }

    fn finish_workload(&self, workload_id: Option<Uuid>, command_id: Uuid) {
        let Some(workload_id) = workload_id else {
            return;
        };
        if let Entry::Occupied(entry) = self.active_workloads.entry(workload_id) {
            if *entry.get() == command_id {
                entry.remove();
            }
        }
    }

    async fn send_inventory(
        &self,
        request: InventoryRequest,
        epoch: u64,
        reply_to: Uuid,
        outbound: &mpsc::Sender<ControlEnvelope>,
    ) -> Result<Option<WorkloadStatus>, RuntimeError> {
        let inventory = self.runtime.inventory().await?;
        let pages = inventory_pages(inventory)?;
        if pages.is_empty() {
            let page = InventoryPage {
                snapshot_id: request.snapshot_id,
                page: 0,
                final_page: true,
                items: Vec::new(),
            };
            let _ = outbound
                .send(ControlEnvelope {
                    reply_to: Some(reply_to),
                    ..ControlEnvelope::new(epoch, ControlMessage::InventoryPage(page))
                })
                .await;
        } else {
            let page_count = pages.len();
            for (index, items) in pages.into_iter().enumerate() {
                let page = InventoryPage {
                    snapshot_id: request.snapshot_id,
                    page: index as u32,
                    final_page: index + 1 == page_count,
                    items,
                };
                if outbound
                    .send(ControlEnvelope {
                        reply_to: Some(reply_to),
                        ..ControlEnvelope::new(epoch, ControlMessage::InventoryPage(page))
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
        Ok(None)
    }
}

fn send_busy(
    outbound: &mpsc::Sender<ControlEnvelope>,
    epoch: u64,
    reply_to: Uuid,
    command_id: Uuid,
    detail: &str,
) {
    let _ = outbound.try_send(ControlEnvelope {
        reply_to: Some(reply_to),
        ..ControlEnvelope::new(
            epoch,
            ControlMessage::CommandAck(CommandAck {
                command_id,
                disposition: AckDisposition::Busy,
                detail: Some(detail.to_string()),
            }),
        )
    });
}

fn inventory_pages(
    inventory: Vec<rsctf_worker_protocol::InventoryItem>,
) -> Result<Vec<Vec<rsctf_worker_protocol::InventoryItem>>, RuntimeError> {
    let mut pages = Vec::new();
    let mut current = Vec::new();
    for item in inventory {
        if current.len() >= INVENTORY_PAGE_SIZE {
            pages.push(std::mem::take(&mut current));
        }
        current.push(item);
        if encoded_inventory_items(&current)? > INVENTORY_PAYLOAD_BUDGET {
            let item = current.pop().expect("an inventory item was just pushed");
            if current.is_empty() {
                return Err(RuntimeError::new(
                    rsctf_worker_protocol::CommandErrorCode::Internal,
                    "one workload inventory item exceeds the control frame limit",
                ));
            }
            pages.push(std::mem::take(&mut current));
            current.push(item);
            if encoded_inventory_items(&current)? > INVENTORY_PAYLOAD_BUDGET {
                return Err(RuntimeError::new(
                    rsctf_worker_protocol::CommandErrorCode::Internal,
                    "one workload inventory item exceeds the control frame limit",
                ));
            }
        }
    }
    if !current.is_empty() {
        pages.push(current);
    }
    Ok(pages)
}

fn encoded_inventory_items(
    items: &[rsctf_worker_protocol::InventoryItem],
) -> Result<usize, RuntimeError> {
    serde_json::to_vec(items)
        .map(|value| value.len())
        .map_err(|error| {
            RuntimeError::new(
                rsctf_worker_protocol::CommandErrorCode::Internal,
                format!("encode workload inventory: {error}"),
            )
        })
}

pub async fn run_session(
    connector: &MtlsConnector,
    hello: &WorkerHello,
    runtime: SharedRuntime,
    dispatcher: OperationDispatcher,
    readiness: &ReadinessFile,
) -> Result<(), ClientError> {
    let mut stream = connector.connect_control().await?;
    let welcome: ServerWelcome = tokio::time::timeout(SESSION_NEGOTIATION_TIMEOUT, async {
        write_json_frame(&mut stream, hello).await?;
        read_json_frame(&mut stream).await
    })
    .await
    .map_err(|_| ClientError::Transport("control session negotiation timed out".to_string()))??;
    validate_revision(welcome.protocol_revision)?;
    if welcome.heartbeat_interval_ms == 0
        || welcome.lease_timeout_ms <= welcome.heartbeat_interval_ms
        || welcome.limits.max_control_frame_bytes == 0
        || welcome.limits.max_control_frame_bytes as usize > MAX_CONTROL_FRAME
        || welcome.limits.max_in_flight_commands == 0
        || welcome.limits.max_data_lanes == 0
        || welcome.limits.max_streams_per_lane == 0
    {
        return Err(ClientError::Protocol(
            "server returned invalid heartbeat timing".to_string(),
        ));
    }
    readiness.mark_connected().await?;
    tracing::info!(
        session_id = %welcome.session.session_id,
        session_epoch = welcome.session.session_epoch,
        "worker control session established"
    );

    let mut data_tasks = Vec::with_capacity(usize::from(welcome.limits.max_data_lanes));
    for lane_number in 0..welcome.limits.max_data_lanes {
        data_tasks.push(tokio::spawn(data::run_reconnecting(
            connector.clone(),
            hello.worker_id,
            welcome.session,
            runtime.clone(),
            lane_number,
        )));
    }
    let result = control_loop(stream, welcome, runtime, dispatcher).await;
    for task in &data_tasks {
        task.abort();
    }
    for task in data_tasks {
        let _ = task.await;
    }
    result
}

async fn control_loop(
    stream: tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    welcome: ServerWelcome,
    runtime: SharedRuntime,
    dispatcher: OperationDispatcher,
) -> Result<(), ClientError> {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<ControlEnvelope>(OUTBOUND_QUEUE);
    let writer_task: JoinHandle<Result<(), ClientError>> = tokio::spawn(async move {
        while let Some(envelope) = outbound_rx.recv().await {
            write_json_frame(&mut writer, &envelope).await?;
        }
        Ok(())
    });
    let heartbeat_task = spawn_heartbeats(
        welcome.session.session_epoch,
        welcome.heartbeat_interval_ms,
        runtime,
        outbound_tx.clone(),
    );
    tokio::pin!(writer_task);

    let result = loop {
        tokio::select! {
            message = read_json_frame::<_, ControlEnvelope>(&mut reader) => {
                let envelope = message?;
                validate_revision(envelope.protocol_revision)?;
                if envelope.session_epoch != welcome.session.session_epoch {
                    break Err(ClientError::Protocol("received stale session epoch".to_string()));
                }
                match envelope.body {
                    ControlMessage::InventoryRequest(_)
                    | ControlMessage::EnsureWorkload(_)
                    | ControlMessage::EnsureAbsent(_)
                    | ControlMessage::WriteFlag(_) => dispatcher.dispatch(
                        &envelope,
                        outbound_tx.clone(),
                        welcome.limits.max_in_flight_commands,
                    )?,
                    _ => break Err(ClientError::Protocol("server sent a worker-only message".to_string())),
                }
            }
            writer_result = &mut writer_task => {
                match writer_result {
                    Ok(result) => break result,
                    Err(error) => break Err(ClientError::Transport(format!("control writer task failed: {error}"))),
                }
            }
        }
    };
    heartbeat_task.abort();
    result
}

fn spawn_heartbeats(
    epoch: u64,
    interval_ms: u64,
    runtime: SharedRuntime,
    outbound: mpsc::Sender<ControlEnvelope>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut usage = rsctf_worker_protocol::ResourceUsage {
            reserved_cpu_millis: 0,
            reserved_memory_bytes: 0,
            running_workloads: 0,
        };
        let mut heartbeat_count = 0_u64;
        let mut consecutive_probe_failures = 0_u8;
        loop {
            interval.tick().await;
            let probe_error = match runtime.probe().await {
                Ok(()) => {
                    consecutive_probe_failures = 0;
                    None
                }
                Err(error) => {
                    consecutive_probe_failures = consecutive_probe_failures.saturating_add(1);
                    Some(error.to_string())
                }
            };
            // Usage is informational in protocol revision 1. Sample it at one
            // sixth the heartbeat rate, cache the last value, and never fence
            // routes because one O(containers) inventory call failed.
            if heartbeat_count.is_multiple_of(6) {
                match runtime.usage().await {
                    Ok(sample) => usage = sample,
                    Err(error) => tracing::warn!(%error, "Docker usage sample failed"),
                }
            }
            heartbeat_count = heartbeat_count.saturating_add(1);
            let runtime_healthy = consecutive_probe_failures < 3;
            let runtime_error = (!runtime_healthy).then(|| {
                probe_error
                    .clone()
                    .unwrap_or_else(|| "Docker health probe failed".to_string())
            });
            let heartbeat = Heartbeat {
                sent_at_unix_ms: unix_millis(),
                usage,
                runtime_healthy,
                runtime_error,
            };
            if outbound
                .send(ControlEnvelope::new(
                    epoch,
                    ControlMessage::Heartbeat(heartbeat),
                ))
                .await
                .is_err()
            {
                return;
            }
        }
    })
}

async fn run_timed<F>(timeout_ms: u64, operation: F) -> Result<Option<WorkloadStatus>, RuntimeError>
where
    F: std::future::Future<Output = Result<WorkloadStatus, RuntimeError>> + Send + 'static,
{
    if timeout_ms == 0 {
        return Err(RuntimeError::new(
            rsctf_worker_protocol::CommandErrorCode::Timeout,
            "command timeout must be greater than zero",
        ));
    }
    let mut operation = tokio::spawn(operation);
    match tokio::time::timeout(Duration::from_millis(timeout_ms), &mut operation).await {
        Ok(result) => map_runtime_task_result(result),
        Err(_) => {
            // Do not cancel a Docker mutation: the daemon can finish an HTTP
            // request after its client future is dropped. Awaiting the owned
            // task keeps this workload's active slot until the exact operation
            // has quiesced, preventing a later generation from racing a late
            // container create/delete.
            tracing::warn!(
                timeout_ms,
                "runtime command exceeded its nominal deadline; waiting for it to quiesce"
            );
            map_runtime_task_result(operation.await)
        }
    }
}

fn map_runtime_task_result(
    result: Result<Result<WorkloadStatus, RuntimeError>, tokio::task::JoinError>,
) -> Result<Option<WorkloadStatus>, RuntimeError> {
    result
        .map_err(|error| {
            RuntimeError::new(
                rsctf_worker_protocol::CommandErrorCode::Internal,
                format!("runtime command task failed: {error}"),
            )
        })?
        .map(Some)
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use rsctf_worker_protocol::{
        CommandErrorCode, InventoryItem, ObservedWorkloadState, ReplicaStatus, WorkloadFence,
    };
    use tokio::sync::oneshot;

    use super::*;

    fn item() -> InventoryItem {
        InventoryItem {
            fence: WorkloadFence {
                workload_id: Uuid::new_v4(),
                assignment_id: Uuid::new_v4(),
                generation: 1,
            },
            spec_hash: "a".repeat(64),
            state: ObservedWorkloadState::Ready,
            replicas: Vec::new(),
        }
    }

    fn status() -> WorkloadStatus {
        WorkloadStatus {
            fence: WorkloadFence {
                workload_id: Uuid::new_v4(),
                assignment_id: Uuid::new_v4(),
                generation: 2,
            },
            spec_hash: "b".repeat(64),
            state: ObservedWorkloadState::Ready,
            replicas: Vec::new(),
            detail: Some("quiesced".to_string()),
        }
    }

    #[test]
    fn inventory_pages_are_count_and_byte_bounded() {
        let pages = inventory_pages((0..101).map(|_| item()).collect()).unwrap();
        assert_eq!(pages.len(), 2);
        assert!(pages.iter().all(|page| {
            page.len() <= INVENTORY_PAGE_SIZE
                && encoded_inventory_items(page).unwrap() <= INVENTORY_PAYLOAD_BUDGET
        }));

        let mut oversized = item();
        oversized.replicas.push(ReplicaStatus {
            service: "challenge".into(),
            replica: 0,
            ready: false,
            runtime_id: None,
            detail: Some("x".repeat(MAX_CONTROL_FRAME)),
        });
        assert!(inventory_pages(vec![oversized]).is_err());
    }

    #[tokio::test]
    async fn run_timed_preserves_success_after_nominal_deadline() {
        let expected = status();
        let operation_result = expected.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn(run_timed(1, async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Ok(operation_result)
        }));

        started_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!task.is_finished());
        release_tx.send(()).unwrap();

        assert_eq!(task.await.unwrap().unwrap(), Some(expected));
    }

    #[tokio::test]
    async fn run_timed_preserves_runtime_error_after_nominal_deadline() {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let task = tokio::spawn(run_timed(1, async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Err(RuntimeError::new(
                CommandErrorCode::RuntimeUnavailable,
                "Docker daemon rejected the operation",
            )
            .with_failed_replicas(vec!["challenge-1".to_string()]))
        }));

        started_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!task.is_finished());
        release_tx.send(()).unwrap();

        let error = task.await.unwrap().unwrap_err();
        assert_eq!(error.code, CommandErrorCode::RuntimeUnavailable);
        assert_eq!(error.message, "Docker daemon rejected the operation");
        assert_eq!(error.failed_replicas, ["challenge-1"]);
    }
}
