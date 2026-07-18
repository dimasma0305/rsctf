use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};
use rsctf_worker_protocol::{
    ControlMessage, DataStreamRequest, Heartbeat, Platform, ResourceUsage, RuntimeDescriptor,
    RuntimeEndpointKind, RuntimeKind, TcpProxyRequest, WorkerCapabilities, WorkerCapacity,
    WorkerHello, WorkloadFence, PROTOCOL_REVISION,
};
use serde_json::json;
use uuid::Uuid;

use super::tests::{spec, AuthorityFixture};
use super::*;
use crate::services::worker_store::{
    PlaceWorkload, PlacementOutcome, PlatformOs, ResourceReservation, WorkloadDefinition,
};

pub(super) fn v1_capabilities() -> WorkerCapabilities {
    WorkerCapabilities {
        ensure_workload: true,
        write_flag: true,
        tcp_proxy: true,
        interactive_exec: false,
        inventory: true,
        local_image_build: false,
        max_data_lanes: 4,
        max_workload_replicas: 512,
    }
}

fn hello(worker_id: Uuid, capabilities: WorkerCapabilities) -> WorkerHello {
    WorkerHello {
        protocol_revision: PROTOCOL_REVISION,
        worker_id,
        boot_id: Uuid::new_v4(),
        agent_version: "test-agent".into(),
        platform: Platform {
            operating_system: OperatingSystem::Linux,
            architecture: "amd64".into(),
            windows_build: None,
        },
        runtime: RuntimeDescriptor {
            kind: RuntimeKind::Docker,
            version: Some("test".into()),
            endpoint_kind: Some(RuntimeEndpointKind::UnixSocket),
        },
        capabilities,
        capacity: WorkerCapacity {
            cpu_millis: 4_000,
            memory_bytes: 8_388_608,
            slots: 8,
        },
        labels: BTreeMap::new(),
    }
}

fn heartbeat(runtime_healthy: bool) -> ControlMessage {
    ControlMessage::Heartbeat(Heartbeat {
        sent_at_unix_ms: Utc::now().timestamp_millis(),
        usage: ResourceUsage {
            reserved_cpu_millis: 0,
            reserved_memory_bytes: 0,
            running_workloads: 0,
        },
        runtime_healthy,
        runtime_error: (!runtime_healthy).then(|| "Docker probe failed".into()),
    })
}

fn placement(worker_id: Uuid, owner_key: String) -> PlaceWorkload {
    PlaceWorkload {
        id: Uuid::new_v4(),
        owner_kind: "test".into(),
        owner_key,
        assignment_id: Uuid::new_v4(),
        definition: WorkloadDefinition {
            spec: serde_json::to_value(spec()).unwrap(),
            spec_hash_sha256: [8; 32],
            required_os: PlatformOs::Linux,
            required_architecture: "amd64".into(),
            required_runtime: "docker".into(),
            reservation: ResourceReservation {
                cpu_millis: 100,
                memory_bytes: 1_048_576,
                slots: 1,
            },
        },
        exact_worker_id: Some(worker_id),
        required_labels: json!({}),
    }
}

#[test]
fn revision_one_requires_every_lifecycle_and_route_capability() {
    let complete = v1_capabilities();
    assert!(validate_v1_capabilities(&complete).is_ok());

    for missing in [
        "ensureWorkload",
        "writeFlag",
        "tcpProxy",
        "inventory",
        "maxDataLanes",
        "maxWorkloadReplicas",
    ] {
        let mut capabilities = complete.clone();
        match missing {
            "ensureWorkload" => capabilities.ensure_workload = false,
            "writeFlag" => capabilities.write_flag = false,
            "tcpProxy" => capabilities.tcp_proxy = false,
            "inventory" => capabilities.inventory = false,
            "maxDataLanes" => capabilities.max_data_lanes = 0,
            "maxWorkloadReplicas" => capabilities.max_workload_replicas = 0,
            _ => unreachable!(),
        }
        assert!(matches!(
            validate_v1_capabilities(&capabilities),
            Err(WorkerError::Protocol(
                "worker is missing required revision 1 capabilities"
            ))
        ));
    }

    let mut oversized = complete;
    oversized.max_workload_replicas = 513;
    assert!(validate_v1_capabilities(&oversized).is_err());
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn missing_capability_cannot_replace_or_schedule_a_durable_session() {
    let fixture = AuthorityFixture::create().await;
    let session = fixture.insert_current_session().await;
    let store = WorkerStore::new(fixture.pool.clone());
    let authority = PostgresWorkerAuthority::new(store.clone(), Duration::from_secs(30));
    let mut incomplete = v1_capabilities();
    incomplete.inventory = false;

    assert!(matches!(
        authority
            .begin_session(
                &AuthenticatedPeer {
                    worker_id: session.worker_id,
                    fingerprint_sha256: session.certificate_fingerprint_sha256,
                },
                &hello(session.worker_id, incomplete),
            )
            .await,
        Err(WorkerError::Protocol(
            "worker is missing required revision 1 capabilities"
        ))
    ));
    let durable: (Option<Uuid>, i64) =
        sqlx::query_as(r#"SELECT session_id, session_epoch FROM "WorkerNodes" WHERE id = $1"#)
            .bind(session.worker_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(durable, (Some(session.fence.session_id), 1));

    let mut stored_incomplete = v1_capabilities();
    stored_incomplete.write_flag = false;
    sqlx::query(r#"UPDATE "WorkerNodes" SET capabilities = $2 WHERE id = $1"#)
        .bind(session.worker_id)
        .bind(serde_json::to_value(stored_incomplete).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(matches!(
        store
            .place_workload(&placement(
                session.worker_id,
                format!("missing-cap-{}", Uuid::new_v4())
            ))
            .await
            .unwrap(),
        PlacementOutcome::NoCompatibleCapacity
    ));

    fixture.destroy().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn unhealthy_heartbeat_withdraws_exact_session_routes_and_placement() {
    let fixture = AuthorityFixture::create().await;
    let session = fixture.insert_current_session().await;
    let store = WorkerStore::new(fixture.pool.clone());
    let authority = PostgresWorkerAuthority::new(store.clone(), Duration::from_secs(30));
    let workload_id = Uuid::new_v4();
    let assignment_id = Uuid::new_v4();
    let fence = WorkloadFence {
        workload_id,
        assignment_id,
        generation: 1,
    };
    sqlx::query(
        r#"INSERT INTO "WorkerWorkloads" (
               id, owner_kind, owner_key, worker_id, assignment_id, generation,
               spec_hash_sha256, spec, observed_state, observed_session_epoch,
               observed_at, ready_at, required_replicas
           ) VALUES (
               $1, 'test', $2, $3, $4, 1, $5, $6, 'Ready', 1,
               clock_timestamp(), clock_timestamp(), 2
           )"#,
    )
    .bind(workload_id)
    .bind(format!("ready-{workload_id}"))
    .bind(session.worker_id)
    .bind(assignment_id)
    .bind([7_u8; 32].as_slice())
    .bind(serde_json::to_value(spec()).unwrap())
    .execute(&fixture.pool)
    .await
    .unwrap();
    let request = DataStreamRequest::TcpProxy(TcpProxyRequest {
        fence,
        service: "challenge".into(),
        port: "service".into(),
        replica: Some(0),
    });

    assert!(store
        .ready_workload_session(workload_id)
        .await
        .unwrap()
        .is_some());
    authority
        .authorize_data_stream(&session, &request)
        .await
        .unwrap();
    assert!(matches!(
        store
            .place_workload(&placement(
                session.worker_id,
                format!("before-unhealthy-{}", Uuid::new_v4()),
            ))
            .await
            .unwrap(),
        PlacementOutcome::Placed(_)
    ));

    assert!(matches!(
        authority.handle_inbound(&session, heartbeat(false)).await,
        Err(WorkerError::StaleSession)
    ));
    let durable: (Option<Uuid>, i64) =
        sqlx::query_as(r#"SELECT session_id, session_epoch FROM "WorkerNodes" WHERE id = $1"#)
            .bind(session.worker_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(durable, (None, 1));
    let observed: String =
        sqlx::query_scalar(r#"SELECT observed_state FROM "WorkerWorkloads" WHERE id = $1"#)
            .bind(workload_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(observed, "Lost");
    assert!(store
        .ready_workload_session(workload_id)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        authority.authorize_data_stream(&session, &request).await,
        Err(WorkerError::Authorization)
    ));
    assert!(matches!(
        store
            .place_workload(&placement(
                session.worker_id,
                format!("after-unhealthy-{}", Uuid::new_v4()),
            ))
            .await
            .unwrap(),
        PlacementOutcome::NoCompatibleCapacity
    ));

    let replacement_session = Uuid::new_v4();
    sqlx::query(
        r#"UPDATE "WorkerNodes"
              SET session_id = $2, session_epoch = 2, boot_id = $3,
                  connected_at = clock_timestamp(), heartbeat_at = clock_timestamp(),
                  lease_expires_at = $4
            WHERE id = $1"#,
    )
    .bind(session.worker_id)
    .bind(replacement_session)
    .bind(Uuid::new_v4())
    .bind(Utc::now() + ChronoDuration::minutes(5))
    .execute(&fixture.pool)
    .await
    .unwrap();
    assert!(matches!(
        authority.handle_inbound(&session, heartbeat(false)).await,
        Err(WorkerError::StaleSession)
    ));
    let current: (Option<Uuid>, i64) =
        sqlx::query_as(r#"SELECT session_id, session_epoch FROM "WorkerNodes" WHERE id = $1"#)
            .bind(session.worker_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(current, (Some(replacement_session), 2));

    fixture.destroy().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn concurrent_placements_retry_a_busy_single_worker_row() {
    const PLACEMENTS: usize = 16;

    let fixture = AuthorityFixture::create().await;
    let session = fixture.insert_current_session().await;
    sqlx::query(
        r#"UPDATE "WorkerNodes"
              SET capacity_cpu_millis = 64000,
                  capacity_memory_bytes = 67108864,
                  capacity_slots = 64
            WHERE id = $1"#,
    )
    .bind(session.worker_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    let store = WorkerStore::new(fixture.pool.clone());
    let mut placements = tokio::task::JoinSet::new();
    for index in 0..PLACEMENTS {
        let store = store.clone();
        let request = placement(
            session.worker_id,
            format!("burst-{index}-{}", Uuid::new_v4()),
        );
        placements.spawn(async move { store.place_workload(&request).await });
    }

    let mut placed = 0;
    while let Some(result) = placements.join_next().await {
        if matches!(result.unwrap().unwrap(), PlacementOutcome::Placed(_)) {
            placed += 1;
        }
    }
    assert_eq!(placed, PLACEMENTS);

    let stored: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "WorkerWorkloads""#)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(stored, PLACEMENTS as i64);
    fixture.destroy().await;
}
