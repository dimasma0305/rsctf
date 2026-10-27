use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_trait::async_trait;
use rsctf_worker_protocol::{
    ControlMessage, DataStreamRequest, EnsureAbsent, InventoryItem, OperatingSystem, RuntimeKind,
    SessionFence, ValidatedWorkloadSpec, WorkerHello,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    AuthenticatedPeer, PeerCertificates, SessionContext, WorkerAuthority, WorkerError, WorkerResult,
};
use crate::services::worker_store::{
    PlatformOs, ResourceReservation, SessionFence as StoreSessionFence, StatusUpdateOutcome,
    WorkerInventory, WorkerStore, WorkerStoreError, WorkloadObservedState,
    WorkloadStatus as StoreWorkloadStatus,
};

mod heartbeat;
mod validation;

use heartbeat::HeartbeatWrites;
use validation::*;

/// PostgreSQL-backed identity, session and workload authority for the live
/// worker transport. The transport owns sockets; this type owns every durable
/// authorization decision and fencing check.
#[derive(Clone)]
pub struct PostgresWorkerAuthority {
    store: WorkerStore,
    lease: Duration,
    heartbeat_writes: HeartbeatWrites,
    issuer: Option<std::sync::Arc<crate::services::worker_pki::WorkerIssuer>>,
}

impl PostgresWorkerAuthority {
    pub fn new(store: WorkerStore, lease: Duration) -> Self {
        Self {
            store,
            lease,
            heartbeat_writes: HeartbeatWrites::new((lease / 3).max(Duration::from_secs(1))),
            issuer: None,
        }
    }

    pub fn with_issuer(
        mut self,
        issuer: std::sync::Arc<crate::services::worker_pki::WorkerIssuer>,
    ) -> Self {
        self.issuer = Some(issuer);
        self
    }

    fn store_session(context: &SessionContext) -> WorkerResult<StoreSessionFence> {
        Ok(StoreSessionFence {
            worker_id: context.worker_id,
            session_id: context.fence.session_id,
            session_epoch: i64::try_from(context.fence.session_epoch)
                .map_err(|_| WorkerError::StaleSession)?,
        })
    }

    async fn session_is_current(&self, context: &SessionContext) -> WorkerResult<()> {
        let worker = self
            .store
            .get_worker(context.worker_id)
            .await
            .map_err(store_error)?
            .ok_or(WorkerError::Authentication)?;
        let epoch =
            i64::try_from(context.fence.session_epoch).map_err(|_| WorkerError::StaleSession)?;
        if worker.session_id != Some(context.fence.session_id)
            || worker.session_epoch != epoch
            || worker
                .lease_expires_at
                .is_none_or(|expiry| expiry <= chrono::Utc::now())
        {
            return Err(WorkerError::StaleSession);
        }
        Ok(())
    }

    async fn record_item(
        &self,
        context: &SessionContext,
        item: &InventoryItem,
        detail: Option<String>,
    ) -> WorkerResult<StatusUpdateOutcome> {
        // A worker reports observed containers, but the durable desired spec is
        // authoritative. In particular, one surviving replica must never make
        // a partially lost workload Ready after reconnect.
        if let Some(workload) = self
            .store
            .get_workload(item.fence.workload_id)
            .await
            .map_err(store_error)?
        {
            let generation =
                i64::try_from(item.fence.generation).map_err(|_| WorkerError::Authorization)?;
            if workload.worker_id == context.worker_id
                && workload.assignment_id == item.fence.assignment_id
                && workload.generation == generation
                && workload.definition.spec_hash_sha256 == decode_hash(&item.spec_hash)?
            {
                let spec: ValidatedWorkloadSpec = serde_json::from_value(workload.definition.spec)
                    .map_err(|error| {
                        WorkerError::Authority(format!(
                            "invalid stored workload spec while checking status: {error}"
                        ))
                    })?;
                validate_replica_observation(&spec, item)?;
            }
        }
        let status = StoreWorkloadStatus {
            session: Self::store_session(context)?,
            workload_id: item.fence.workload_id,
            assignment_id: item.fence.assignment_id,
            generation: i64::try_from(item.fence.generation)
                .map_err(|_| WorkerError::Authorization)?,
            spec_hash_sha256: decode_hash(&item.spec_hash)?,
            state: observed_state(item.state),
            message: detail,
        };
        self.store
            .record_workload_status(&status)
            .await
            .map_err(store_error)
    }
}

#[async_trait]
impl WorkerAuthority for PostgresWorkerAuthority {
    fn supports_certificate_renewal(&self) -> bool {
        self.issuer.is_some()
    }

    async fn renew_certificate(
        &self,
        session: &SessionContext,
        request: rsctf_worker_protocol::RenewCertificate,
    ) -> WorkerResult<rsctf_worker_protocol::CertificateRenewed> {
        self.validate_peer(&AuthenticatedPeer {
            worker_id: session.worker_id,
            fingerprint_sha256: session.certificate_fingerprint_sha256,
        })
        .await?;
        let issuer = self
            .issuer
            .clone()
            .ok_or(WorkerError::Protocol("certificate renewal unavailable"))?;
        self.store
            .renew_certificate(Self::store_session(session)?, &request, issuer)
            .await
            .map_err(store_error)
    }
    async fn authenticate_peer(&self, peer: &PeerCertificates) -> WorkerResult<AuthenticatedPeer> {
        let leaf = peer.leaf_der().ok_or(WorkerError::Authentication)?;
        let fingerprint: [u8; 32] = Sha256::digest(leaf).into();
        self.store
            .authenticate_certificate(fingerprint)
            .await
            .map_err(store_error)?
            .map(|worker| AuthenticatedPeer {
                worker_id: worker.id,
                fingerprint_sha256: fingerprint,
            })
            .ok_or(WorkerError::Authentication)
    }

    async fn validate_peer(&self, peer: &AuthenticatedPeer) -> WorkerResult<()> {
        let current = self
            .store
            .authenticate_certificate(peer.fingerprint_sha256)
            .await
            .map_err(store_error)?;
        if current.is_some_and(|worker| worker.id == peer.worker_id) {
            Ok(())
        } else {
            Err(WorkerError::Authentication)
        }
    }

    async fn begin_session(
        &self,
        peer: &AuthenticatedPeer,
        hello: &WorkerHello,
    ) -> WorkerResult<SessionFence> {
        validate_hello_metadata(hello)?;
        validate_required_capabilities(&hello.capabilities)?;
        validate_labels(hello)?;
        let capacity = ResourceReservation {
            cpu_millis: i64::try_from(hello.capacity.cpu_millis)
                .map_err(|_| WorkerError::Protocol("worker CPU capacity exceeds limits"))?,
            memory_bytes: i64::try_from(hello.capacity.memory_bytes)
                .map_err(|_| WorkerError::Protocol("worker memory capacity exceeds limits"))?,
            slots: i32::try_from(hello.capacity.slots)
                .map_err(|_| WorkerError::Protocol("worker slot capacity exceeds limits"))?,
        };
        let inventory = WorkerInventory {
            platform_os: match hello.platform.operating_system {
                OperatingSystem::Linux => PlatformOs::Linux,
                OperatingSystem::Windows => PlatformOs::Windows,
            },
            architecture: hello.platform.architecture.clone(),
            runtime_kind: match hello.runtime.kind {
                RuntimeKind::Docker => "docker",
                RuntimeKind::Kubernetes => "kubernetes",
                RuntimeKind::Unavailable => "unavailable",
            }
            .to_string(),
            runtime_version: hello
                .runtime
                .version
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            labels: serde_json::to_value(&hello.labels)
                .map_err(|error| WorkerError::ProtocolOwned(error.to_string()))?,
            capabilities: serde_json::to_value(&hello.capabilities)
                .map_err(|error| WorkerError::ProtocolOwned(error.to_string()))?,
            capacity,
        };
        let session_id = Uuid::new_v4();
        let session = self
            .store
            .open_session(
                peer.worker_id,
                peer.fingerprint_sha256,
                session_id,
                hello.boot_id,
                &inventory,
                self.lease,
            )
            .await
            .map_err(store_error)?
            .ok_or(WorkerError::Authentication)?;
        let fence = SessionFence {
            session_id: session.fence.session_id,
            session_epoch: u64::try_from(session.fence.session_epoch)
                .map_err(|_| WorkerError::Authority("negative worker session epoch".into()))?,
        };
        self.heartbeat_writes.opened(peer.worker_id, fence);
        Ok(fence)
    }

    async fn validate_inbound(
        &self,
        session: &SessionContext,
        message: &ControlMessage,
    ) -> WorkerResult<()> {
        match message {
            ControlMessage::RenewCertificate(_) => {
                self.session_is_current(session).await?;
            }
            ControlMessage::CertificateRenewed(_) => {
                return Err(WorkerError::Protocol(
                    "worker sent a server-only certificate response",
                ))
            }
            ControlMessage::WorkloadStatus(status) => {
                decode_hash(&status.spec_hash)?;
            }
            ControlMessage::InventoryPage(page) => {
                self.session_is_current(session).await?;
                let mut ids = HashSet::with_capacity(page.items.len());
                for item in &page.items {
                    if !ids.insert(item.fence.workload_id) {
                        return Err(WorkerError::Protocol(
                            "inventory page contains duplicate workloads",
                        ));
                    }
                    decode_hash(&item.spec_hash)?;
                }
            }
            ControlMessage::Heartbeat(_)
            | ControlMessage::CommandAck(_)
            | ControlMessage::CommandResult(_) => {}
            ControlMessage::InventoryRequest(_)
            | ControlMessage::EnsureWorkload(_)
            | ControlMessage::EnsureAbsent(_)
            | ControlMessage::WriteFlag(_) => {
                return Err(WorkerError::Protocol(
                    "worker sent a server-only control message",
                ));
            }
        }
        Ok(())
    }

    async fn handle_inbound(
        &self,
        session: &SessionContext,
        message: ControlMessage,
    ) -> WorkerResult<()> {
        match message {
            ControlMessage::RenewCertificate(_) | ControlMessage::CertificateRenewed(_) => {
                return Err(WorkerError::Protocol(
                    "renewal must use its authenticated response path",
                ));
            }
            ControlMessage::Heartbeat(heartbeat) => {
                if !heartbeat.runtime_healthy {
                    let closed = self
                        .store
                        .close_session(Self::store_session(session)?)
                        .await
                        .map_err(store_error)?;
                    if closed {
                        tracing::warn!(
                            worker_id = %session.worker_id,
                            "closed worker session after unhealthy runtime heartbeat"
                        );
                    }
                    // Returning an error tears down the matching live control
                    // connection and its data lanes. close_session is fenced,
                    // so a delayed unhealthy heartbeat cannot evict a newer
                    // replacement session.
                    return Err(WorkerError::StaleSession);
                }
                // The process-local lease is touched by every heartbeat in the
                // transport. Coalesce durable renewals so a compromised but
                // enrolled worker cannot turn heartbeat floods into DB writes.
                if !self.heartbeat_writes.is_due(session) {
                    return Ok(());
                }
                // A TLS connection may outlive the certificate overlap window.
                // Revalidate its exact fingerprint before extending its lease.
                self.validate_peer(&AuthenticatedPeer {
                    worker_id: session.worker_id,
                    fingerprint_sha256: session.certificate_fingerprint_sha256,
                })
                .await?;
                if self
                    .store
                    .heartbeat(Self::store_session(session)?, self.lease)
                    .await
                    .map_err(store_error)?
                    .is_none()
                {
                    return Err(WorkerError::StaleSession);
                }
            }
            ControlMessage::WorkloadStatus(status) => {
                let workload_id = status.fence.workload_id;
                let item = InventoryItem {
                    fence: status.fence,
                    spec_hash: status.spec_hash,
                    state: status.state,
                    replicas: status.replicas,
                };
                if self.record_item(session, &item, status.detail).await?
                    == StatusUpdateOutcome::Stale
                {
                    // A workload fence can legitimately lose a race with a
                    // generation replacement or orphan cleanup. That must not
                    // tear down an otherwise current worker session. Recheck
                    // the session after the failed conditional update so a
                    // superseded connection still fails closed.
                    self.session_is_current(session).await?;
                    tracing::debug!(
                        worker_id = %session.worker_id,
                        %workload_id,
                        "ignored stale workload status from current worker session"
                    );
                }
            }
            ControlMessage::CommandAck(_) | ControlMessage::CommandResult(_) => {
                // Results are advisory in revision 1. Desired/observed state is
                // advanced only by a fully fenced WorkloadStatus.
            }
            ControlMessage::InventoryPage(_)
            | ControlMessage::InventoryRequest(_)
            | ControlMessage::EnsureWorkload(_)
            | ControlMessage::EnsureAbsent(_)
            | ControlMessage::WriteFlag(_) => {
                return Err(WorkerError::Protocol("unexpected inbound message"));
            }
        }
        Ok(())
    }

    async fn handle_inventory_snapshot(
        &self,
        session: &SessionContext,
        snapshot_id: Uuid,
        items: Vec<InventoryItem>,
    ) -> WorkerResult<Vec<ControlMessage>> {
        const PAGE_SIZE: i64 = 1_000;

        let store_session = Self::store_session(session)?;
        let mut assigned = HashMap::new();
        let mut after_id = None;
        loop {
            let workloads = self
                .store
                .list_session_workloads(store_session, after_id, PAGE_SIZE)
                .await
                .map_err(store_error)?;
            for workload in &workloads {
                assigned.insert(workload.id, workload.clone());
            }
            if workloads.len() < PAGE_SIZE as usize {
                break;
            }
            after_id = workloads.last().map(|workload| workload.id);
        }

        let mut cleanup = Vec::new();
        let mut statuses = Vec::with_capacity(items.len().saturating_add(assigned.len()));
        let mut reported_exact = HashSet::with_capacity(items.len());
        for item in items {
            let generation =
                i64::try_from(item.fence.generation).map_err(|_| WorkerError::Authorization)?;
            let spec_hash = decode_hash(&item.spec_hash)?;
            let current = assigned.get(&item.fence.workload_id);
            let exact = current.is_some_and(|workload| {
                workload.assignment_id == item.fence.assignment_id
                    && workload.generation == generation
                    && workload.definition.spec_hash_sha256 == spec_hash
            });
            if !exact {
                tracing::warn!(worker_id = %session.worker_id, workload_id = %item.fence.workload_id, "worker reported an unassigned or stale workload");
                cleanup.push(ControlMessage::EnsureAbsent(EnsureAbsent {
                    command_id: Uuid::new_v4(),
                    fence: item.fence,
                    spec_hash: item.spec_hash,
                    timeout_ms: 60_000,
                }));
                continue;
            }
            if !reported_exact.insert(item.fence.workload_id) {
                return Err(WorkerError::Protocol(
                    "inventory snapshot contains the current workload fence more than once",
                ));
            }

            let workload = assigned
                .get(&item.fence.workload_id)
                .cloned()
                .ok_or_else(|| WorkerError::Authority("inventory assignment disappeared".into()))?;
            let spec: ValidatedWorkloadSpec = serde_json::from_value(workload.definition.spec)
                .map_err(|error| {
                    WorkerError::Authority(format!(
                        "invalid stored workload spec while checking inventory: {error}"
                    ))
                })?;
            validate_replica_observation(&spec, &item)?;
            statuses.push(StoreWorkloadStatus {
                session: store_session,
                workload_id: item.fence.workload_id,
                assignment_id: item.fence.assignment_id,
                generation,
                spec_hash_sha256: spec_hash,
                state: observed_state(item.state),
                message: Some(format!("adopted from inventory snapshot {snapshot_id}")),
            });
        }

        for workload_id in reported_exact {
            assigned.remove(&workload_id);
        }

        // A completed inventory is an authoritative full snapshot, not just a
        // list of things the worker happened to find. Mark each remaining
        // assignment absent; the reconciler recreates desired-Present rows.
        for workload in assigned.into_values() {
            statuses.push(StoreWorkloadStatus {
                session: store_session,
                workload_id: workload.id,
                assignment_id: workload.assignment_id,
                generation: workload.generation,
                spec_hash_sha256: workload.definition.spec_hash_sha256,
                state: WorkloadObservedState::Absent,
                message: Some(format!("missing from inventory snapshot {snapshot_id}")),
            });
        }

        let mut applied = 0;
        for batch in statuses.chunks(PAGE_SIZE as usize) {
            applied += self
                .store
                .record_workload_status_batch(store_session, batch)
                .await
                .map_err(store_error)?
                .len();
        }
        if applied != statuses.len() {
            self.session_is_current(session).await?;
            tracing::debug!(
                worker_id = %session.worker_id,
                expected = statuses.len(),
                applied,
                "inventory raced with a newer workload generation"
            );
        }
        Ok(cleanup)
    }

    async fn authorize_data_stream(
        &self,
        session: &SessionContext,
        request: &DataStreamRequest,
    ) -> WorkerResult<()> {
        let fence = request.workload_fence();
        let generation = i64::try_from(fence.generation).map_err(|_| WorkerError::Authorization)?;
        let session_epoch =
            i64::try_from(session.fence.session_epoch).map_err(|_| WorkerError::StaleSession)?;
        // One indexed query revalidates the live session, route publication,
        // assignment and generation together. This is the user-facing stream
        // open path, so do not repeat separate node/workload/ready round trips.
        let spec = sqlx::query_scalar::<_, serde_json::Value>(
            r#"SELECT workload.spec
                 FROM "WorkerWorkloads" workload
                 JOIN "WorkerNodes" node ON node.id = workload.worker_id
                WHERE workload.id = $1
                  AND workload.worker_id = $2
                  AND workload.assignment_id = $3
                  AND workload.generation = $4
                  AND workload.desired_state = 'Present'
                  AND workload.observed_state = 'Ready'
                  AND workload.observed_session_epoch = node.session_epoch
                  AND node.session_id = $5
                  AND node.session_epoch = $6
                  AND node.lease_expires_at > clock_timestamp()
                  AND node.certificate_expires_at > clock_timestamp()
                  AND node.administrative_state <> 'Disabled'
                  AND node.capabilities @> '{
                      "ensureWorkload": true,
                      "writeFlag": true,
                      "tcpProxy": true,
                      "inventory": true
                  }'::jsonb
                  AND CASE
                      WHEN jsonb_typeof(node.capabilities -> 'maxDataLanes') = 'number'
                      THEN (node.capabilities ->> 'maxDataLanes')::NUMERIC
                           BETWEEN 1 AND 65535
                      ELSE FALSE
                  END
                  AND CASE
                      WHEN jsonb_typeof(
                          node.capabilities -> 'maxWorkloadReplicas'
                      ) = 'number'
                      THEN (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC =
                               TRUNC((node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC)
                           AND (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC
                               BETWEEN workload.required_replicas AND 512
                      ELSE FALSE
                  END"#,
        )
        .bind(fence.workload_id)
        .bind(session.worker_id)
        .bind(fence.assignment_id)
        .bind(generation)
        .bind(session.fence.session_id)
        .bind(session_epoch)
        .fetch_optional(self.store.pool())
        .await
        .map_err(|error| WorkerError::Authority(error.to_string()))?
        .ok_or(WorkerError::Authorization)?;
        let spec: ValidatedWorkloadSpec = serde_json::from_value(spec).map_err(|error| {
            WorkerError::Authority(format!("invalid stored workload spec: {error}"))
        })?;
        if !stream_exists(&spec, request) {
            return Err(WorkerError::Authorization);
        }
        Ok(())
    }

    async fn session_closed(&self, session: &SessionContext) {
        self.heartbeat_writes.closed(session);
        match Self::store_session(session) {
            Ok(fence) => {
                if let Err(error) = self.store.close_session(fence).await {
                    tracing::warn!(worker_id = %session.worker_id, %error, "failed to close worker session");
                }
            }
            Err(error) => {
                tracing::warn!(worker_id = %session.worker_id, %error, "invalid worker session fence during close");
            }
        }
    }
}

fn store_error(error: WorkerStoreError) -> WorkerError {
    WorkerError::Authority(error.to_string())
}

#[cfg(test)]
#[path = "postgres/health_tests.rs"]
mod health_tests;

#[cfg(test)]
#[path = "postgres/tests.rs"]
pub(crate) mod tests;
