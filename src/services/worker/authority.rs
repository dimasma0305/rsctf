use async_trait::async_trait;
use rsctf_worker_protocol::{
    ControlMessage, DataStreamRequest, InventoryItem, SessionFence, WorkerHello,
};
use uuid::Uuid;

use super::{SessionContext, WorkerResult};

/// Exact TLS client identity established for one accepted connection.
///
/// Keeping the leaf fingerprint alongside the durable worker ID prevents an
/// authenticated connection from being upgraded to a newly enrolled identity
/// if certificate rotation races with the application hello.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedPeer {
    pub worker_id: Uuid,
    pub fingerprint_sha256: [u8; 32],
}

/// The certificate chain that rustls already validated against the worker CA.
///
/// The authority still maps the leaf certificate to a durable worker identity
/// (normally from its URI SAN). Keeping DER parsing behind this interface lets
/// the transport remain independent of the database and certificate issuer.
#[derive(Clone, Debug)]
pub struct PeerCertificates {
    pub chain_der: Vec<Vec<u8>>,
}

impl PeerCertificates {
    /// The end-entity certificate, suitable for matching the enrolled SHA-256
    /// fingerprint. Rustls presents the peer chain leaf-first.
    pub fn leaf_der(&self) -> Option<&[u8]> {
        self.chain_der.first().map(Vec::as_slice)
    }
}

/// Durable authorization and event hooks supplied by the application layer.
///
/// Implementations must validate assignment and workload generation fences in
/// `validate_inbound` before accepting inventory, status, or command results.
/// The transport always validates the session epoch itself first.
#[async_trait]
pub trait WorkerAuthority: Send + Sync + 'static {
    /// Resolve a rustls-verified client certificate to its immutable worker ID.
    async fn authenticate_peer(&self, peer: &PeerCertificates) -> WorkerResult<AuthenticatedPeer>;

    /// Revalidate that this exact certificate is still enrolled. Data lanes
    /// call this after their hello so rotation/revocation cannot race the TLS
    /// handshake and attach a stale connection to a live session.
    async fn validate_peer(&self, peer: &AuthenticatedPeer) -> WorkerResult<()>;

    /// Atomically begin a new durable session and return its fencing token.
    /// The epoch must strictly increase for every accepted control reconnect.
    async fn begin_session(
        &self,
        peer: &AuthenticatedPeer,
        hello: &WorkerHello,
    ) -> WorkerResult<SessionFence>;

    /// Revalidate worker state and any assignment/generation carried by an
    /// inbound message. Revoked workers and stale workload incarnations fail
    /// closed here.
    async fn validate_inbound(
        &self,
        session: &SessionContext,
        message: &ControlMessage,
    ) -> WorkerResult<()>;

    /// Persist or otherwise consume an already validated inbound message.
    async fn handle_inbound(
        &self,
        session: &SessionContext,
        message: ControlMessage,
    ) -> WorkerResult<()>;

    /// Consume one complete, strictly ordered inventory snapshot. Partial
    /// snapshots are kept in the live session and discarded on disconnect.
    async fn handle_inventory_snapshot(
        &self,
        session: &SessionContext,
        snapshot_id: Uuid,
        items: Vec<InventoryItem>,
    ) -> WorkerResult<Vec<ControlMessage>>;

    /// Authorize one named workload stream immediately before it is opened.
    /// Wire requests contain no arbitrary IP address or hostname.
    async fn authorize_data_stream(
        &self,
        session: &SessionContext,
        request: &DataStreamRequest,
    ) -> WorkerResult<()>;

    /// Best-effort notification after the current control session disappears.
    async fn session_closed(&self, _session: &SessionContext) {}
}
