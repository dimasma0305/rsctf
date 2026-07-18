use std::sync::Arc;
use std::time::Duration;

use rsctf_worker_protocol::{
    read_json_frame, read_json_frame_counted, write_json_frame, ControlEnvelope, ControlMessage,
    DataHello, ServerWelcome, SessionLimits, WorkerHello, CONTROL_ALPN, DATA_ALPN,
    MAX_CONTROL_FRAME, MAX_WORKER_SLOTS, MAX_WORKLOAD_REPLICAS, PROTOCOL_REVISION,
};
use tokio::io::AsyncWrite;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};
use tokio_rustls::{server::TlsStream, TlsAcceptor};
use uuid::Uuid;

use super::authority::{AuthenticatedPeer, PeerCertificates, WorkerAuthority};
use super::control_admission::ControlAdmission;
use super::data::{drive_data_lane, DataConfig};
use super::registry::{RegistryConfig, SessionRegistration, WorkerRegistry};
use super::{SessionContext, WorkerError, WorkerResult};

mod admission;
use admission::{HandshakeAdmission, PeerHandshakePermit};

#[derive(Clone, Debug)]
pub struct WorkerServerConfig {
    pub registry: RegistryConfig,
    pub data: DataConfig,
    pub handshake_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub inventory_interval: Duration,
    pub max_concurrent_handshakes: usize,
    pub max_concurrent_handshakes_per_ip: usize,
    pub max_inbound_control_messages_per_second: usize,
    pub inbound_control_message_burst: usize,
    pub max_inbound_control_bytes_per_second: usize,
    pub inbound_control_byte_burst: usize,
    pub shutdown_timeout: Duration,
}

impl Default for WorkerServerConfig {
    fn default() -> Self {
        Self {
            registry: RegistryConfig::default(),
            data: DataConfig::default(),
            handshake_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(10),
            inventory_interval: Duration::from_secs(30),
            max_concurrent_handshakes: 128,
            max_concurrent_handshakes_per_ip: 16,
            max_inbound_control_messages_per_second: 128,
            inbound_control_message_burst: 512,
            max_inbound_control_bytes_per_second: 2 * 1024 * 1024,
            inbound_control_byte_burst: 20 * 1024 * 1024,
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

/// Build a TLS-1.3-only server configuration that requires a worker certificate.
///
/// `worker_roots` must contain only the dedicated worker CA, not the public web
/// PKI roots. The caller supplies the server chain and private key from its
/// normal secret/configuration layer.
pub fn build_mtls_server_config(
    server_chain: Vec<CertificateDer<'static>>,
    server_key: PrivateKeyDer<'static>,
    worker_roots: RootCertStore,
) -> WorkerResult<ServerConfig> {
    let verifier = WebPkiClientVerifier::builder(Arc::new(worker_roots))
        .build()
        .map_err(|error| WorkerError::ProtocolOwned(error.to_string()))?;
    let mut config =
        ServerConfig::builder_with_protocol_versions(&[&tokio_rustls::rustls::version::TLS13])
            .with_client_cert_verifier(verifier)
            .with_single_cert(server_chain, server_key)?;
    config.alpn_protocols = vec![CONTROL_ALPN.to_vec(), DATA_ALPN.to_vec()];
    config.max_early_data_size = 0;
    config.send_half_rtt_data = false;
    Ok(config)
}

/// Live worker-plane service. Clone this into the network owner and the local
/// proxy/exec adapters; database behavior stays behind `WorkerAuthority`.
#[derive(Clone)]
pub struct WorkerService {
    config: WorkerServerConfig,
    registry: WorkerRegistry,
    authority: Arc<dyn WorkerAuthority>,
}

impl WorkerService {
    pub fn new(
        config: WorkerServerConfig,
        authority: Arc<dyn WorkerAuthority>,
    ) -> WorkerResult<Self> {
        validate_server_config(&config)?;
        let registry = WorkerRegistry::new(config.registry.clone());
        Ok(Self {
            config,
            registry,
            authority,
        })
    }

    pub fn registry(&self) -> &WorkerRegistry {
        &self.registry
    }

    pub async fn send(&self, worker_id: Uuid, message: ControlMessage) -> WorkerResult<Uuid> {
        self.registry.send(worker_id, message).await
    }

    pub async fn open_data_stream(
        &self,
        worker_id: Uuid,
        request: rsctf_worker_protocol::DataStreamRequest,
    ) -> WorkerResult<super::WorkerDataStream> {
        let session = self
            .registry
            .session_context(worker_id)
            .await
            .ok_or(WorkerError::Offline)?;
        self.authority
            .authorize_data_stream(&session, &request)
            .await?;
        // Re-resolution above can race with reconnect. Registry selection is
        // deliberately fenced again against its current live session.
        self.registry.open_stream(&session, request).await
    }

    pub async fn serve(
        &self,
        listener: TcpListener,
        tls_config: Arc<ServerConfig>,
        mut shutdown: watch::Receiver<bool>,
    ) -> std::io::Result<()> {
        let acceptor = TlsAcceptor::from(tls_config);
        let handshakes = Arc::new(Semaphore::new(self.config.max_concurrent_handshakes));
        let peer_handshakes = HandshakeAdmission::new(self.config.max_concurrent_handshakes_per_ip);
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => {
                    let (socket, peer) = accepted?;
                    let Some(peer_permit) = peer_handshakes.try_admit(peer.ip()) else {
                        tracing::warn!(%peer, "worker: peer handshake capacity exhausted");
                        continue;
                    };
                    let Ok(permit) = handshakes.clone().try_acquire_owned() else {
                        tracing::warn!(%peer, "worker: handshake capacity exhausted");
                        continue;
                    };
                    let service = self.clone();
                    let acceptor = acceptor.clone();
                    connections.spawn(async move {
                        if let Err(error) = service.accept(socket, acceptor, permit, peer_permit).await {
                            tracing::warn!(%peer, %error, "worker: connection rejected");
                        }
                    });
                }
                joined = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = joined {
                        tracing::warn!(%error, "worker: connection task failed");
                    }
                }
            }
        }
        self.registry.shutdown().await;
        let drain = async {
            while let Some(result) = connections.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(%error, "worker: connection task failed during shutdown");
                }
            }
        };
        if tokio::time::timeout(self.config.shutdown_timeout, drain)
            .await
            .is_err()
        {
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        }
        Ok(())
    }

    async fn accept(
        &self,
        socket: TcpStream,
        acceptor: TlsAcceptor,
        handshake_permit: OwnedSemaphorePermit,
        peer_permit: PeerHandshakePermit,
    ) -> WorkerResult<()> {
        let tls = tokio::time::timeout(self.config.handshake_timeout, acceptor.accept(socket))
            .await
            .map_err(|_| WorkerError::Protocol("TLS handshake timed out"))??;
        let (alpn, peer) = {
            let connection = tls.get_ref().1;
            let alpn = connection
                .alpn_protocol()
                .ok_or(WorkerError::Protocol("missing worker ALPN"))?
                .to_vec();
            let chain_der = connection
                .peer_certificates()
                .ok_or(WorkerError::Authentication)?
                .iter()
                .map(|certificate| certificate.as_ref().to_vec())
                .collect();
            (alpn, PeerCertificates { chain_der })
        };
        if peer.chain_der.is_empty() {
            return Err(WorkerError::Authentication);
        }
        let authenticated_peer = tokio::time::timeout(
            self.config.handshake_timeout,
            self.authority.authenticate_peer(&peer),
        )
        .await
        .map_err(|_| WorkerError::Protocol("worker authentication timed out"))??;
        // Keep both admission permits through the application hello and
        // durable session setup. A valid but compromised certificate must not
        // be able to pipeline slow hellos from one address and occupy every
        // global handshake slot.
        if alpn == CONTROL_ALPN {
            self.serve_control(authenticated_peer, tls, handshake_permit, peer_permit)
                .await
        } else if alpn == DATA_ALPN {
            self.serve_data(authenticated_peer, tls, handshake_permit, peer_permit)
                .await
        } else {
            Err(WorkerError::Protocol("unsupported worker ALPN"))
        }
    }

    async fn serve_control(
        &self,
        authenticated_peer: AuthenticatedPeer,
        mut tls: TlsStream<TcpStream>,
        handshake_permit: OwnedSemaphorePermit,
        peer_permit: PeerHandshakePermit,
    ) -> WorkerResult<()> {
        let hello: WorkerHello =
            tokio::time::timeout(self.config.handshake_timeout, read_json_frame(&mut tls))
                .await
                .map_err(|_| WorkerError::Protocol("control hello timed out"))??;
        if hello.protocol_revision != PROTOCOL_REVISION {
            return Err(WorkerError::Protocol("unsupported protocol revision"));
        }
        if hello.worker_id != authenticated_peer.worker_id {
            return Err(WorkerError::Authentication);
        }
        if hello.capabilities.tcp_proxy && hello.capabilities.max_data_lanes == 0 {
            return Err(WorkerError::Protocol(
                "TCP proxy capability requires at least one data lane",
            ));
        }
        if hello.capacity.cpu_millis == 0
            || hello.capacity.memory_bytes == 0
            || hello.capacity.slots == 0
            || hello.capacity.slots > MAX_WORKER_SLOTS
        {
            return Err(WorkerError::Protocol(
                "worker capacity is zero or exceeds protocol limits",
            ));
        }
        let session = tokio::time::timeout(
            self.config.handshake_timeout,
            self.authority.begin_session(&authenticated_peer, &hello),
        )
        .await
        .map_err(|_| WorkerError::Protocol("begin worker session timed out"))??;
        let context = SessionContext {
            worker_id: hello.worker_id,
            boot_id: hello.boot_id,
            certificate_fingerprint_sha256: authenticated_peer.fingerprint_sha256,
            fence: session,
        };
        // From the instant begin_session commits, exactly one owner must close
        // its durable fence. This guard stays armed through registration and
        // handshake preparation, then moves into the live control loop.
        let cleanup = SessionCleanup::new(
            self.registry.clone(),
            self.authority.clone(),
            context.clone(),
        );
        let welcome = ServerWelcome {
            protocol_revision: PROTOCOL_REVISION,
            session,
            heartbeat_interval_ms: duration_millis(self.config.heartbeat_interval),
            lease_timeout_ms: duration_millis(self.config.registry.heartbeat_lease),
            limits: SessionLimits {
                max_control_frame_bytes: MAX_CONTROL_FRAME as u32,
                max_in_flight_commands: usize_to_u16(
                    self.config.registry.max_in_flight_commands_per_worker,
                ),
                max_data_lanes: negotiated_data_lane_limit(
                    self.config.registry.max_data_lanes_per_worker,
                    hello.capabilities.max_data_lanes,
                ),
                max_streams_per_lane: usize_to_u16(self.config.data.max_streams_per_lane),
            },
        };
        let (registration, cleanup) = self
            .prepare_control_session(
                &context,
                &welcome,
                hello.capabilities.inventory,
                &mut tls,
                cleanup,
            )
            .await?;
        drop((handshake_permit, peer_permit));
        self.run_control(tls, registration, hello.capabilities.inventory, cleanup)
            .await
    }

    async fn prepare_control_session<W>(
        &self,
        context: &SessionContext,
        welcome: &ServerWelcome,
        inventory_enabled: bool,
        writer: &mut W,
        cleanup: SessionCleanup,
    ) -> WorkerResult<(SessionRegistration, SessionCleanup)>
    where
        W: AsyncWrite + Unpin,
    {
        let prepared = async {
            let registration = self
                .registry
                .register_authenticated_control(
                    context.clone(),
                    usize::from(welcome.limits.max_data_lanes),
                )
                .await?;
            tokio::time::timeout(
                self.config.handshake_timeout,
                write_json_frame(writer, welcome),
            )
            .await
            .map_err(|_| WorkerError::Protocol("control welcome timed out"))??;
            if inventory_enabled {
                self.registry
                    .request_inventory(
                        context.worker_id,
                        self.config.inventory_interval.saturating_mul(3),
                    )
                    .await?;
            }
            Ok(registration)
        }
        .await;

        match prepared {
            Ok(registration) => Ok((registration, cleanup)),
            Err(error) => {
                cleanup.close().await;
                Err(error)
            }
        }
    }

    async fn run_control(
        &self,
        tls: TlsStream<TcpStream>,
        registration: SessionRegistration,
        inventory_enabled: bool,
        cleanup: SessionCleanup,
    ) -> WorkerResult<()> {
        let SessionRegistration {
            context,
            mut outbound,
            mut shutdown,
        } = registration;
        let (mut reader, mut writer) = tokio::io::split(tls);
        let registry = self.registry.clone();
        let authority = self.authority.clone();
        let reader_context = context.clone();
        let mut admission = ControlAdmission::new(
            self.config.max_inbound_control_messages_per_second,
            self.config.inbound_control_message_burst,
            self.config.max_inbound_control_bytes_per_second,
            self.config.inbound_control_byte_burst,
        );
        let read_loop = async move {
            loop {
                let (envelope, payload_bytes): (ControlEnvelope, usize) =
                    read_json_frame_counted(&mut reader).await?;
                if !admission.try_admit(payload_bytes) {
                    return Err(WorkerError::Protocol(
                        "inbound worker control quota exceeded",
                    ));
                }
                validate_envelope(&reader_context, &envelope)?;
                validate_inbound_shape(&envelope.body)?;
                if matches!(&envelope.body, ControlMessage::Heartbeat(_)) {
                    registry
                        .touch(reader_context.worker_id, &reader_context.fence)
                        .await?;
                }
                authority
                    .validate_inbound(&reader_context, &envelope.body)
                    .await?;
                let failed_status = registry
                    .handle_command_feedback(&reader_context, &envelope)
                    .await?;
                match envelope.body {
                    ControlMessage::InventoryPage(page) => {
                        if let Some((snapshot_id, items)) =
                            registry.collect_inventory(&reader_context, page).await?
                        {
                            let cleanup = authority
                                .handle_inventory_snapshot(&reader_context, snapshot_id, items)
                                .await?;
                            for command in cleanup {
                                match registry.send(reader_context.worker_id, command).await {
                                    Ok(_) => {}
                                    Err(WorkerError::Busy) => {
                                        tracing::warn!(
                                            worker_id = %reader_context.worker_id,
                                            "worker cleanup command queue is full"
                                        );
                                        break;
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                            registry
                                .complete_inventory(&reader_context, snapshot_id)
                                .await?;
                        }
                    }
                    ControlMessage::WorkloadStatus(status) => {
                        registry
                            .observe_workload_status(&reader_context, &status)
                            .await?;
                        authority
                            .handle_inbound(&reader_context, ControlMessage::WorkloadStatus(status))
                            .await?;
                    }
                    message => {
                        authority.handle_inbound(&reader_context, message).await?;
                    }
                }
                if let Some(status) = failed_status {
                    authority
                        .handle_inbound(&reader_context, ControlMessage::WorkloadStatus(status))
                        .await?;
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), WorkerError>(())
        };
        let write_loop = async move {
            while let Some(envelope) = outbound.recv().await {
                write_json_frame(&mut writer, &envelope).await?;
            }
            Ok::<(), WorkerError>(())
        };
        let lease_loop = async {
            let check_every =
                (self.config.registry.heartbeat_lease / 3).max(Duration::from_millis(250));
            loop {
                tokio::time::sleep(check_every).await;
                if !self.registry.is_online(context.worker_id).await {
                    return Err(WorkerError::Offline);
                }
            }
        };
        let inventory_loop = async {
            if !inventory_enabled {
                std::future::pending::<()>().await;
            }
            loop {
                tokio::time::sleep(self.config.inventory_interval).await;
                match self
                    .registry
                    .request_inventory(
                        context.worker_id,
                        self.config.inventory_interval.saturating_mul(3),
                    )
                    .await
                {
                    Ok(_) | Err(WorkerError::Busy) => {}
                    Err(error) => return Err(error),
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), WorkerError>(())
        };
        let result = tokio::select! {
            result = read_loop => result,
            result = write_loop => result,
            result = lease_loop => result,
            result = inventory_loop => result,
            changed = shutdown.changed() => {
                let _ = changed;
                Ok(())
            }
        };
        cleanup.close().await;
        result
    }

    async fn serve_data(
        &self,
        authenticated_peer: AuthenticatedPeer,
        mut tls: TlsStream<TcpStream>,
        handshake_permit: OwnedSemaphorePermit,
        peer_permit: PeerHandshakePermit,
    ) -> WorkerResult<()> {
        let hello: DataHello =
            tokio::time::timeout(self.config.handshake_timeout, read_json_frame(&mut tls))
                .await
                .map_err(|_| WorkerError::Protocol("data hello timed out"))??;
        if hello.protocol_revision != PROTOCOL_REVISION {
            return Err(WorkerError::Protocol("unsupported protocol revision"));
        }
        if hello.worker_id != authenticated_peer.worker_id {
            return Err(WorkerError::Authentication);
        }
        let session = self
            .registry
            .session_context(hello.worker_id)
            .await
            .ok_or(WorkerError::Offline)?;
        if session.fence != hello.session {
            return Err(WorkerError::StaleSession);
        }
        if session.certificate_fingerprint_sha256 != authenticated_peer.fingerprint_sha256 {
            return Err(WorkerError::Authentication);
        }
        tokio::time::timeout(
            self.config.handshake_timeout,
            self.authority.validate_peer(&authenticated_peer),
        )
        .await
        .map_err(|_| WorkerError::Protocol("worker certificate revalidation timed out"))??;
        drop((handshake_permit, peer_permit));
        drive_data_lane(
            self.registry.clone(),
            session,
            hello.lane,
            tls,
            self.config.data.clone(),
        )
        .await
    }
}

/// Owns cleanup for one exact durable session fence. Explicit failure paths
/// await `close`; cancellation still schedules the same fenced cleanup on drop.
struct SessionCleanup {
    registry: WorkerRegistry,
    authority: Arc<dyn WorkerAuthority>,
    context: Option<SessionContext>,
}

impl SessionCleanup {
    fn new(
        registry: WorkerRegistry,
        authority: Arc<dyn WorkerAuthority>,
        context: SessionContext,
    ) -> Self {
        Self {
            registry,
            authority,
            context: Some(context),
        }
    }

    async fn close(mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        close_session(&self.registry, &self.authority, &context).await;
    }
}

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        let Some(context) = self.context.take() else {
            return;
        };
        let registry = self.registry.clone();
        let authority = self.authority.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                close_session(&registry, &authority, &context).await;
            });
        } else {
            tracing::error!(
                worker_id = %context.worker_id,
                session_id = %context.fence.session_id,
                "worker session cleanup dropped without an async runtime"
            );
        }
    }
}

async fn close_session(
    registry: &WorkerRegistry,
    authority: &Arc<dyn WorkerAuthority>,
    context: &SessionContext,
) {
    // Registry removal and durable close both carry the exact session fence;
    // neither operation can evict a newer replacement connection.
    registry
        .remove_control_if(context.worker_id, &context.fence)
        .await;
    authority.session_closed(context).await;
}

fn validate_envelope(context: &SessionContext, envelope: &ControlEnvelope) -> WorkerResult<()> {
    if envelope.protocol_revision != PROTOCOL_REVISION {
        return Err(WorkerError::Protocol("unsupported protocol revision"));
    }
    if envelope.session_epoch != context.fence.session_epoch {
        return Err(WorkerError::StaleSession);
    }
    Ok(())
}

fn validate_inbound_shape(message: &ControlMessage) -> WorkerResult<()> {
    const MAX_INVENTORY_ITEMS: usize = 512;
    match message {
        ControlMessage::Heartbeat(_)
        | ControlMessage::CommandAck(_)
        | ControlMessage::CommandResult(_) => Ok(()),
        ControlMessage::InventoryPage(page) => {
            if page.items.len() > MAX_INVENTORY_ITEMS
                || page
                    .items
                    .iter()
                    .any(|item| item.replicas.len() > MAX_WORKLOAD_REPLICAS)
            {
                Err(WorkerError::Protocol("inventory page exceeds limits"))
            } else {
                Ok(())
            }
        }
        ControlMessage::WorkloadStatus(status) => {
            if status.replicas.len() > MAX_WORKLOAD_REPLICAS {
                Err(WorkerError::Protocol("workload status exceeds limits"))
            } else {
                Ok(())
            }
        }
        ControlMessage::InventoryRequest(_)
        | ControlMessage::EnsureWorkload(_)
        | ControlMessage::EnsureAbsent(_)
        | ControlMessage::WriteFlag(_) => Err(WorkerError::Protocol(
            "worker sent a server-only control message",
        )),
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn usize_to_u16(value: usize) -> u16 {
    value.min(usize::from(u16::MAX)) as u16
}

fn negotiated_data_lane_limit(configured: usize, advertised: u16) -> u16 {
    usize_to_u16(configured).min(advertised)
}

fn validate_server_config(config: &WorkerServerConfig) -> WorkerResult<()> {
    if config.registry.max_workers == 0
        || config.registry.max_data_lanes_per_worker == 0
        || config.registry.max_data_lanes_per_worker > usize::from(u16::MAX)
        || config.registry.control_queue_capacity == 0
        || config.registry.max_in_flight_commands_per_worker == 0
        || config.registry.max_in_flight_commands_per_worker > usize::from(u16::MAX)
        || config.registry.max_inventory_items == 0
        || config.registry.max_inventory_pages == 0
        || config.registry.max_inventory_bytes == 0
        || config.registry.max_total_inventory_bytes == 0
        || config.registry.max_inventory_bytes > config.registry.max_total_inventory_bytes
        || config.registry.max_inventory_replicas == 0
        || config.max_concurrent_handshakes == 0
        || config.max_concurrent_handshakes_per_ip == 0
        || config.max_concurrent_handshakes_per_ip > config.max_concurrent_handshakes
        || config.shutdown_timeout.is_zero()
        || config.max_inbound_control_messages_per_second == 0
        || config.inbound_control_message_burst
            < config
                .registry
                .max_in_flight_commands_per_worker
                .saturating_mul(3)
                .saturating_add(config.registry.max_inventory_pages)
        || config.max_inbound_control_bytes_per_second == 0
        || config.inbound_control_byte_burst < config.registry.max_inventory_bytes
        || config.handshake_timeout.is_zero()
        || config.heartbeat_interval.is_zero()
        || config.inventory_interval.is_zero()
        || config.registry.heartbeat_lease <= config.heartbeat_interval
        || config.data.max_streams_per_lane == 0
        || config.data.max_streams_per_lane > usize::from(u16::MAX)
        || config.data.pending_open_capacity == 0
        || config.data.open_timeout.is_zero()
        || config.data.status_timeout.is_zero()
        || config.data.max_connection_receive_window < 256 * 1024 * config.data.max_streams_per_lane
    {
        return Err(WorkerError::Protocol("invalid worker server limits"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        async fn authenticate_peer(
            &self,
            _peer: &PeerCertificates,
        ) -> WorkerResult<AuthenticatedPeer> {
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
        let cleanup =
            SessionCleanup::new(service.registry.clone(), authority.clone(), context.clone());

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
}
