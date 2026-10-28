use std::collections::BTreeMap;

use chrono::{Duration as ChronoDuration, Utc};
use rsctf_worker_protocol::{
    EndpointRef, GameKind, Heartbeat, ImageIdentity, ObservedWorkloadState, Platform, PortProtocol,
    ReplicaStatus, ResourceLimits, ResourceUsage, ServicePort, ServiceSpec, TcpProxyRequest,
    WorkloadFence, WorkloadSpec, WorkloadStatus,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

use super::health_tests::complete_capabilities;
use super::*;

pub(crate) struct AuthorityFixture {
    admin: PgPool,
    pub(crate) pool: PgPool,
    schema: String,
}

impl AuthorityFixture {
    pub(crate) async fn create() -> Self {
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
                enrollment_token_hash BYTEA NULL,
                enrollment_token_expires_at TIMESTAMPTZ NULL,
                enrollment_token_used_at TIMESTAMPTZ NULL,
                certificate_serial TEXT NULL,
                certificate_fingerprint_sha256 BYTEA NULL,
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
                reconcile_quarantine_generation BIGINT NULL,
                reconcile_quarantined_at TIMESTAMPTZ NULL,
                reconcile_quarantine_message TEXT NULL,
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
        sqlx::raw_sql(crate::migrations::m0361_worker_certificate_rotation::UP_SQL)
            .execute(&pool)
            .await
            .expect("create certificate recovery table");

        Self {
            admin,
            pool,
            schema,
        }
    }

    pub(crate) async fn insert_current_session(&self) -> SessionContext {
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
        .bind(serde_json::to_value(complete_capabilities()).unwrap())
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

    pub(crate) async fn destroy(self) {
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

pub(crate) fn spec() -> ValidatedWorkloadSpec {
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
                storage_bytes: rsctf_worker_protocol::DEFAULT_STORAGE_BYTES,
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
    let (generation, observed_state): (i64, String) =
        sqlx::query_as(r#"SELECT generation, observed_state FROM "WorkerWorkloads" WHERE id = $1"#)
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
