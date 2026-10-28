use std::sync::Arc;
use std::time::Duration;

use rsctf_worker_protocol::{
    read_json_frame, read_json_frame_counted, write_json_frame, ControlEnvelope, ControlMessage,
    DataHello, ServerWelcome, SessionLimits, WorkerHello, CONTROL_ALPN, DATA_ALPN,
    MAX_CONTROL_FRAME, MAX_WORKER_SLOTS, MAX_WORKLOAD_REPLICAS, PROTOCOL_REVISION,
};
use tokio::io::AsyncWrite;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, OwnedSemaphorePermit};
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
use admission::{HandshakeAdmission, HandshakeSlots, PeerHandshakePermit};

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
        let handshakes = HandshakeSlots::new(self.config.max_concurrent_handshakes);
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
                        if let Some(rejections) = admission::sample_peer_rejection() {
                            tracing::warn!(%peer, rejections, "worker: peer handshake capacity exhausted");
                        }
                        continue;
                    };
                    let Some(permit) = handshakes.try_acquire(peer_handshakes.is_known_source(peer.ip())) else {
                        if let Some(rejections) = admission::sample_global_rejection() {
                            tracing::warn!(%peer, rejections, "worker: handshake capacity exhausted");
                        }
                        continue;
                    };
                    let service = self.clone();
                    let acceptor = acceptor.clone();
                    let peer_handshakes = peer_handshakes.clone();
                    connections.spawn(async move {
                        if let Err(error) = service.accept(socket, acceptor, permit, peer_permit, peer_handshakes).await {
                            if let Some(rejections) = admission::sample_connection_rejection() {
                                tracing::warn!(%peer, %error, rejections, "worker: connection rejected");
                            }
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
        handshake_admission: HandshakeAdmission,
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
        if !handshake_admission.admit_worker(authenticated_peer.worker_id) {
            return Err(WorkerError::Busy);
        }
        peer_permit.mark_authenticated();
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
            certificate_renewal: self.authority.supports_certificate_renewal(),
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
                    ControlMessage::RenewCertificate(request) => {
                        let response = tokio::time::timeout(
                            Duration::from_secs(10),
                            authority.renew_certificate(&reader_context, request),
                        )
                        .await
                        .map_err(|_| WorkerError::Protocol("certificate renewal timed out"))??;
                        registry
                            .send_certificate_response(&reader_context, response)
                            .await?;
                    }
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
        ControlMessage::RenewCertificate(request) => {
            if request.operation_id.is_nil()
                || request.csr_pem.is_empty()
                || request.csr_pem.len() > 64 * 1024
            {
                Err(WorkerError::Protocol("invalid certificate renewal request"))
            } else {
                Ok(())
            }
        }
        ControlMessage::CertificateRenewed(_) => Err(WorkerError::Protocol(
            "worker sent a server-only certificate response",
        )),
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
mod tests;
