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
}

impl PostgresWorkerAuthority {
    pub fn new(store: WorkerStore, lease: Duration) -> Self {
        Self {
            store,
            lease,
            heartbeat_writes: HeartbeatWrites::new((lease / 3).max(Duration::from_secs(1))),
        }
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
        validate_v1_capabilities(&hello.capabilities)?;
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
mod tests {
    use std::collections::BTreeMap;

    use chrono::{Duration as ChronoDuration, Utc};
    use rsctf_worker_protocol::{
        EndpointRef, GameKind, Heartbeat, ImageIdentity, ObservedWorkloadState, Platform,
        PortProtocol, ReplicaStatus, ResourceLimits, ResourceUsage, ServicePort, ServiceSpec,
        TcpProxyRequest, WorkloadFence, WorkloadSpec, WorkloadStatus,
    };
    use sqlx::{postgres::PgPoolOptions, PgPool};

    use super::health_tests::v1_capabilities;
    use super::*;

    pub(super) struct AuthorityFixture {
        admin: PgPool,
        pub(super) pool: PgPool,
        schema: String,
    }

    impl AuthorityFixture {
        pub(super) async fn create() -> Self {
            let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
                .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
            let admin = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .expect("connect test database");
            let schema = format!("worker_authority_{}", Uuid::new_v4().simple());
            sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
                .execute(&admin)
                .await
                .expect("create isolated worker-authority schema");

            let search_path_schema = schema.clone();
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .after_connect(move |connection, _metadata| {
                    let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                    Box::pin(async move {
                        sqlx::query(&statement).execute(connection).await?;
                        Ok(())
                    })
                })
                .connect(&database_url)
                .await
                .expect("connect isolated worker-authority schema");
            sqlx::raw_sql(
                r#"
                CREATE TABLE "WorkerNodes" (
                    id UUID PRIMARY KEY,
                    name TEXT NOT NULL,
                    administrative_state TEXT NOT NULL DEFAULT 'Enabled',
                    platform_os TEXT NULL,
                    platform_architecture TEXT NULL,
                    runtime_kind TEXT NULL,
                    runtime_version TEXT NULL,
                    labels JSONB NOT NULL DEFAULT '{}'::jsonb,
                    capabilities JSONB NOT NULL DEFAULT '{}'::jsonb,
                    capacity_cpu_millis BIGINT NOT NULL DEFAULT 0,
                    capacity_memory_bytes BIGINT NOT NULL DEFAULT 0,
                    capacity_slots INTEGER NOT NULL DEFAULT 0,
                    certificate_serial TEXT NULL,
                    certificate_expires_at TIMESTAMPTZ NULL,
                    session_id UUID NULL,
                    session_epoch BIGINT NOT NULL DEFAULT 0,
                    boot_id UUID NULL,
                    connected_at TIMESTAMPTZ NULL,
                    heartbeat_at TIMESTAMPTZ NULL,
                    lease_expires_at TIMESTAMPTZ NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
                );
                CREATE TABLE "WorkerWorkloads" (
                    id UUID PRIMARY KEY,
                    owner_kind TEXT NOT NULL DEFAULT 'test',
                    owner_key TEXT NOT NULL DEFAULT 'test',
                    worker_id UUID NOT NULL,
                    assignment_id UUID NOT NULL,
                    generation BIGINT NOT NULL,
                    spec_hash_sha256 BYTEA NOT NULL,
                    spec JSONB NOT NULL DEFAULT '{}'::jsonb,
                    required_os TEXT NOT NULL DEFAULT 'linux',
                    required_architecture TEXT NOT NULL DEFAULT 'amd64',
                    required_runtime TEXT NOT NULL DEFAULT 'docker',
                    required_labels JSONB NOT NULL DEFAULT '{}'::jsonb,
                    reserved_cpu_millis BIGINT NOT NULL DEFAULT 0,
                    reserved_memory_bytes BIGINT NOT NULL DEFAULT 0,
                    reserved_slots INTEGER NOT NULL DEFAULT 1,
                    required_replicas INTEGER NOT NULL DEFAULT 1,
                    desired_state TEXT NOT NULL DEFAULT 'Present',
                    observed_state TEXT NOT NULL DEFAULT 'Unknown',
                    observed_session_epoch BIGINT NULL,
                    observed_message TEXT NULL,
                    observed_at TIMESTAMPTZ NULL,
                    ready_at TIMESTAMPTZ NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
                );
                CREATE UNIQUE INDEX ux_workerworkloads_owner
                    ON "WorkerWorkloads" (owner_kind, owner_key);
                CREATE UNIQUE INDEX ux_workerworkloads_assignment
                    ON "WorkerWorkloads" (assignment_id);
                "#,
            )
            .execute(&pool)
            .await
            .expect("create worker-authority tables");

            Self {
                admin,
                pool,
                schema,
            }
        }

        pub(super) async fn insert_current_session(&self) -> SessionContext {
            let worker_id = Uuid::new_v4();
            let session_id = Uuid::new_v4();
            let boot_id = Uuid::new_v4();
            sqlx::query(
                r#"INSERT INTO "WorkerNodes" (
                       id, name, platform_os, platform_architecture, runtime_kind,
                       capabilities, capacity_cpu_millis, capacity_memory_bytes,
                       capacity_slots, certificate_fingerprint_sha256,
                       certificate_expires_at, session_id,
                       session_epoch, boot_id, connected_at, heartbeat_at,
                       lease_expires_at
                   ) VALUES (
                       $1, $2, 'linux', 'amd64', 'docker', $3, 4000, 8388608,
                       8, $4, $5, $6, 1, $7, clock_timestamp(), clock_timestamp(), $8
                   )"#,
            )
            .bind(worker_id)
            .bind(format!("worker-{worker_id}"))
            .bind(serde_json::to_value(v1_capabilities()).unwrap())
            .bind([7_u8; 32].as_slice())
            .bind(Utc::now() + ChronoDuration::hours(1))
            .bind(session_id)
            .bind(boot_id)
            .bind(Utc::now() + ChronoDuration::minutes(5))
            .execute(&self.pool)
            .await
            .expect("insert current worker session");
            SessionContext {
                worker_id,
                boot_id,
                certificate_fingerprint_sha256: [7; 32],
                fence: SessionFence {
                    session_id,
                    session_epoch: 1,
                },
            }
        }

        pub(super) async fn destroy(self) {
            self.pool.close().await;
            sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
                .execute(&self.admin)
                .await
                .expect("drop isolated worker-authority schema");
            self.admin.close().await;
        }
    }

    fn heartbeat() -> ControlMessage {
        ControlMessage::Heartbeat(Heartbeat {
            sent_at_unix_ms: Utc::now().timestamp_millis(),
            usage: ResourceUsage {
                reserved_cpu_millis: 0,
                reserved_memory_bytes: 0,
                running_workloads: 0,
            },
            runtime_healthy: true,
            runtime_error: None,
        })
    }

    fn workload_status(
        fence: WorkloadFence,
        spec_hash: [u8; 32],
        state: ObservedWorkloadState,
    ) -> ControlMessage {
        ControlMessage::WorkloadStatus(WorkloadStatus {
            fence,
            spec_hash: hex::encode(spec_hash),
            state,
            replicas: Vec::new(),
            detail: None,
        })
    }

    pub(super) fn spec() -> ValidatedWorkloadSpec {
        WorkloadSpec {
            game_kind: GameKind::Jeopardy,
            platform: Platform {
                operating_system: OperatingSystem::Linux,
                architecture: "amd64".into(),
                windows_build: None,
            },
            services: vec![ServiceSpec {
                name: "challenge".into(),
                image: ImageIdentity::RegistryDigest {
                    repository: "example.invalid/challenge".into(),
                    digest: format!("sha256:{}", "a".repeat(64)),
                },
                resources: ResourceLimits {
                    cpu_millis: 100,
                    memory_bytes: 1_048_576,
                },
                replicas: 2,
                stateless: true,
                environment: BTreeMap::new(),
                ports: vec![ServicePort {
                    name: "service".into(),
                    container_port: 8080,
                    protocol: PortProtocol::Tcp,
                }],
            }],
            primary_endpoint: EndpointRef {
                service: "challenge".into(),
                port: "service".into(),
            },
            flag_target: None,
        }
        .try_into()
        .unwrap()
    }

    #[test]
    fn stream_authorization_is_named_and_replica_bounded() {
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 1,
        };
        let request = |replica| {
            DataStreamRequest::TcpProxy(TcpProxyRequest {
                fence,
                service: "challenge".into(),
                port: "service".into(),
                replica,
            })
        };
        assert!(stream_exists(&spec(), &request(None)));
        assert!(stream_exists(&spec(), &request(Some(1))));
        assert!(!stream_exists(&spec(), &request(Some(2))));
    }

    #[test]
    fn ready_observation_requires_the_complete_replica_topology() {
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 1,
        };
        let replica = |replica| ReplicaStatus {
            service: "challenge".into(),
            replica,
            ready: true,
            runtime_id: Some(format!("runtime-{replica}")),
            detail: None,
        };
        let complete = InventoryItem {
            fence,
            spec_hash: "0".repeat(64),
            state: ObservedWorkloadState::Ready,
            replicas: vec![replica(0), replica(1)],
        };
        assert!(validate_replica_observation(&spec(), &complete).is_ok());

        let mut partial = complete.clone();
        partial.replicas.pop();
        assert!(validate_replica_observation(&spec(), &partial).is_err());

        let mut duplicate = complete;
        duplicate.replicas[1].replica = 0;
        assert!(validate_replica_observation(&spec(), &duplicate).is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn generation_replacement_race_keeps_current_session_but_rejects_stale_session() {
        let fixture = AuthorityFixture::create().await;
        let session = fixture.insert_current_session().await;
        let authority = PostgresWorkerAuthority::new(
            WorkerStore::new(fixture.pool.clone()),
            Duration::from_secs(30),
        );
        let workload_id = Uuid::new_v4();
        let assignment_id = Uuid::new_v4();
        let replacement_hash = [2_u8; 32];
        sqlx::query(
            r#"INSERT INTO "WorkerWorkloads" (
                   id, worker_id, assignment_id, generation, spec_hash_sha256,
                   spec, required_replicas
               ) VALUES ($1, $2, $3, 2, $4, $5, 2)"#,
        )
        .bind(workload_id)
        .bind(session.worker_id)
        .bind(assignment_id)
        .bind(replacement_hash.as_slice())
        .bind(serde_json::to_value(spec()).unwrap())
        .execute(&fixture.pool)
        .await
        .expect("insert replacement workload generation");
        let old_status = workload_status(
            WorkloadFence {
                workload_id,
                assignment_id,
                generation: 1,
            },
            [1; 32],
            ObservedWorkloadState::Reconciling,
        );

        authority
            .handle_inbound(&session, old_status.clone())
            .await
            .expect("old generation from current session is a harmless fence miss");
        authority
            .handle_inbound(&session, heartbeat())
            .await
            .expect("current session remains usable after generation fence miss");
        let (generation, observed_state): (i64, String) = sqlx::query_as(
            r#"SELECT generation, observed_state FROM "WorkerWorkloads" WHERE id = $1"#,
        )
        .bind(workload_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(generation, 2);
        assert_eq!(observed_state, "Unknown");

        sqlx::query(
            r#"UPDATE "WorkerNodes"
                  SET session_id = $2, session_epoch = 2, boot_id = $3
                WHERE id = $1"#,
        )
        .bind(session.worker_id)
        .bind(Uuid::new_v4())
        .bind(Uuid::new_v4())
        .execute(&fixture.pool)
        .await
        .expect("supersede worker session");
        assert!(matches!(
            authority.handle_inbound(&session, old_status).await,
            Err(WorkerError::StaleSession)
        ));

        fixture.destroy().await;
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn orphan_cleanup_status_does_not_disconnect_current_session() {
        let fixture = AuthorityFixture::create().await;
        let session = fixture.insert_current_session().await;
        let authority = PostgresWorkerAuthority::new(
            WorkerStore::new(fixture.pool.clone()),
            Duration::from_secs(30),
        );
        let orphan_status = workload_status(
            WorkloadFence {
                workload_id: Uuid::new_v4(),
                assignment_id: Uuid::new_v4(),
                generation: 1,
            },
            [9; 32],
            ObservedWorkloadState::Absent,
        );

        authority
            .handle_inbound(&session, orphan_status)
            .await
            .expect("orphan cleanup from current session is a harmless fence miss");
        authority
            .handle_inbound(&session, heartbeat())
            .await
            .expect("current session remains usable after orphan cleanup");

        fixture.destroy().await;
    }
}
