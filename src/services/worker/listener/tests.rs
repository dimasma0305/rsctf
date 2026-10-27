use super::*;
use async_trait::async_trait;
use rsctf_worker_protocol::{
    DataStreamRequest, EnsureAbsent, Heartbeat, InventoryItem, ResourceUsage, SessionFence,
    WorkloadFence,
};
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

#[derive(Default)]
struct RecordingAuthority {
    closed: Mutex<Vec<SessionContext>>,
}

#[async_trait]
impl WorkerAuthority for RecordingAuthority {
    async fn authenticate_peer(&self, _peer: &PeerCertificates) -> WorkerResult<AuthenticatedPeer> {
        Err(WorkerError::Authentication)
    }

    async fn validate_peer(&self, _peer: &AuthenticatedPeer) -> WorkerResult<()> {
        Err(WorkerError::Authentication)
    }

    async fn begin_session(
        &self,
        _peer: &AuthenticatedPeer,
        _hello: &WorkerHello,
    ) -> WorkerResult<SessionFence> {
        Err(WorkerError::Authentication)
    }

    async fn validate_inbound(
        &self,
        _session: &SessionContext,
        _message: &ControlMessage,
    ) -> WorkerResult<()> {
        Ok(())
    }

    async fn handle_inbound(
        &self,
        _session: &SessionContext,
        _message: ControlMessage,
    ) -> WorkerResult<()> {
        Ok(())
    }

    async fn handle_inventory_snapshot(
        &self,
        _session: &SessionContext,
        _snapshot_id: Uuid,
        _items: Vec<InventoryItem>,
    ) -> WorkerResult<Vec<ControlMessage>> {
        Ok(Vec::new())
    }

    async fn authorize_data_stream(
        &self,
        _session: &SessionContext,
        _request: &DataStreamRequest,
    ) -> WorkerResult<()> {
        Ok(())
    }

    async fn session_closed(&self, session: &SessionContext) {
        self.closed.lock().unwrap().push(session.clone());
    }
}

struct FailingWriter;

impl AsyncWrite for FailingWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "intentional test failure",
        )))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[test]
fn stale_session_epoch_is_rejected() {
    let context = SessionContext {
        worker_id: Uuid::new_v4(),
        boot_id: Uuid::new_v4(),
        certificate_fingerprint_sha256: [7; 32],
        fence: SessionFence {
            session_id: Uuid::new_v4(),
            session_epoch: 3,
        },
    };
    let envelope = ControlEnvelope::new(
        2,
        ControlMessage::Heartbeat(Heartbeat {
            sent_at_unix_ms: 0,
            usage: ResourceUsage {
                reserved_cpu_millis: 0,
                reserved_memory_bytes: 0,
                running_workloads: 0,
            },
            runtime_healthy: true,
            runtime_error: None,
        }),
    );
    assert!(matches!(
        validate_envelope(&context, &envelope),
        Err(WorkerError::StaleSession)
    ));
}

#[test]
fn worker_cannot_send_server_commands() {
    let command = ControlMessage::EnsureAbsent(EnsureAbsent {
        command_id: Uuid::new_v4(),
        fence: WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 1,
        },
        spec_hash: "0".repeat(64),
        timeout_ms: 1_000,
    });
    assert!(matches!(
        validate_inbound_shape(&command),
        Err(WorkerError::Protocol(_))
    ));
}

#[test]
fn invalid_zero_capacity_is_rejected() {
    let mut config = WorkerServerConfig::default();
    config.registry.control_queue_capacity = 0;
    assert!(matches!(
        validate_server_config(&config),
        Err(WorkerError::Protocol(_))
    ));
}

#[tokio::test]
async fn failed_welcome_immediately_closes_the_registered_session() {
    let authority = Arc::new(RecordingAuthority::default());
    let service = WorkerService::new(WorkerServerConfig::default(), authority.clone()).unwrap();
    let context = test_context(1);
    let cleanup = SessionCleanup::new(service.registry.clone(), authority.clone(), context.clone());

    let result = service
        .prepare_control_session(
            &context,
            &test_welcome(context.fence),
            false,
            &mut FailingWriter,
            cleanup,
        )
        .await;

    assert!(matches!(result, Err(WorkerError::Frame(_))));
    assert!(service
        .registry
        .session_context(context.worker_id)
        .await
        .is_none());
    assert_closed_once(&authority, &context);
}

#[tokio::test]
async fn failed_old_registration_does_not_close_a_newer_replacement() {
    let authority = Arc::new(RecordingAuthority::default());
    let service = WorkerService::new(WorkerServerConfig::default(), authority.clone()).unwrap();
    let old = test_context(1);
    let newer = SessionContext {
        worker_id: old.worker_id,
        boot_id: Uuid::new_v4(),
        certificate_fingerprint_sha256: old.certificate_fingerprint_sha256,
        fence: SessionFence {
            session_id: Uuid::new_v4(),
            session_epoch: 2,
        },
    };
    let _registration = service
        .registry
        .register_authenticated_control(
            newer.clone(),
            service.config.registry.max_data_lanes_per_worker,
        )
        .await
        .unwrap();
    let cleanup = SessionCleanup::new(service.registry.clone(), authority.clone(), old.clone());

    let result = service
        .prepare_control_session(
            &old,
            &test_welcome(old.fence),
            false,
            &mut tokio::io::sink(),
            cleanup,
        )
        .await;

    assert!(matches!(result, Err(WorkerError::StaleSession)));
    let current = service
        .registry
        .session_context(old.worker_id)
        .await
        .unwrap();
    assert_eq!(current.fence, newer.fence);
    assert_eq!(current.boot_id, newer.boot_id);
    assert_closed_once(&authority, &old);
}

fn test_context(session_epoch: u64) -> SessionContext {
    SessionContext {
        worker_id: Uuid::new_v4(),
        boot_id: Uuid::new_v4(),
        certificate_fingerprint_sha256: [7; 32],
        fence: SessionFence {
            session_id: Uuid::new_v4(),
            session_epoch,
        },
    }
}

fn test_welcome(session: SessionFence) -> ServerWelcome {
    ServerWelcome {
        certificate_renewal: false,
        protocol_revision: PROTOCOL_REVISION,
        session,
        heartbeat_interval_ms: 1_000,
        lease_timeout_ms: 3_000,
        limits: SessionLimits {
            max_control_frame_bytes: MAX_CONTROL_FRAME as u32,
            max_in_flight_commands: 1,
            max_data_lanes: 1,
            max_streams_per_lane: 1,
        },
    }
}

#[test]
fn worker_data_lane_advertisement_caps_the_session_limit() {
    assert_eq!(negotiated_data_lane_limit(4, 1), 1);
    assert_eq!(negotiated_data_lane_limit(4, 8), 4);
}

fn assert_closed_once(authority: &RecordingAuthority, expected: &SessionContext) {
    let closed = authority.closed.lock().unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].worker_id, expected.worker_id);
    assert_eq!(closed[0].boot_id, expected.boot_id);
    assert_eq!(closed[0].fence, expected.fence);
}
