use super::*;
use crate::services::container::ContainerManager;
use crate::services::worker::{WorkerContainerManager, WorkerHandle};
use crate::services::worker_store::{DefinitionUpdateOutcome, UpdateWorkload};
use rsctf_worker_protocol::{FlagTarget, GameKind, ObservedWorkloadState, ReplicaStatus};

fn ad_request(worker: Uuid, mount: Option<&str>) -> PlaceWorkload {
    let mut request = placement(worker, Uuid::new_v4().to_string());
    request.owner_kind = "ad-team-service".into();
    let mut workload = spec().into_inner();
    workload.game_kind = GameKind::AttackDefense;
    workload.services[0].replicas = 1;
    workload.services[0].stateless = false;
    workload.team_data_mount = mount.map(str::to_owned);
    workload.flag_target = Some(FlagTarget {
        service: "challenge".into(),
        path: "/flag".into(),
    });
    let validated = ValidatedWorkloadSpec::try_from(workload).unwrap();
    request.definition.spec_hash_sha256 = hex::decode(validated.spec_hash().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    request.definition.spec = serde_json::to_value(validated).unwrap();
    request
}

fn next_definition(request: &PlaceWorkload) -> WorkloadDefinition {
    let mut next = request.definition.clone();
    next.spec["services"][0]["environment"] = json!({"REVISION":"next"});
    let validated: ValidatedWorkloadSpec = serde_json::from_value(next.spec.clone()).unwrap();
    next.spec_hash_sha256 = hex::decode(validated.spec_hash().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    next
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn stale_team_cleanup_preserves_the_next_revision_and_its_mount() {
    let fixture = AuthorityFixture::create().await;
    let store = WorkerStore::new(fixture.pool.clone());
    let backend = WorkerContainerManager::new(store.clone());
    let authority = PostgresWorkerAuthority::new(store.clone(), Duration::from_secs(30));
    let initial = fixture.insert_current_session().await;
    connect(&authority, &initial, true, true).await;
    for mount in [None, Some("/data")] {
        let request = ad_request(initial.worker_id, mount);
        assert!(matches!(
            store.place_workload(&request).await.unwrap(),
            PlacementOutcome::Placed(_)
        ));
        let previous = WorkerHandle {
            workload_id: request.id,
            assignment_id: request.assignment_id,
            generation: 1,
        };
        assert_eq!(
            backend
                .has_ad_data_volumes(&previous.encode())
                .await
                .unwrap(),
            mount.is_some()
        );
        assert!(matches!(
            store
                .update_workload_definition(&UpdateWorkload {
                    id: request.id,
                    assignment_id: request.assignment_id,
                    expected_generation: 1,
                    definition: next_definition(&request),
                })
                .await
                .unwrap(),
            DefinitionUpdateOutcome::Updated { generation: 2 }
        ));
        let current = WorkerHandle {
            generation: 2,
            ..previous
        }
        .encode();
        assert!(matches!(
            backend.destroy(&previous.encode()).await,
            Err(crate::utils::error::AppError::Conflict(_))
        ));
        assert!(backend
            .has_ad_data_volumes(&previous.encode())
            .await
            .is_err());
        assert_eq!(
            backend.has_ad_data_volumes(&current).await.unwrap(),
            mount.is_some()
        );
        backend.destroy(&current).await.unwrap();
        backend.destroy(&current).await.unwrap();
        let deleted = store.get_workload(request.id).await.unwrap().unwrap();
        assert_eq!(deleted.generation, 3);
        assert_eq!(
            deleted.desired_state,
            crate::services::worker_store::WorkloadDesiredState::Absent
        );
    }
    fixture.destroy().await;
}

async fn connect(
    authority: &PostgresWorkerAuthority,
    initial: &SessionContext,
    readback: bool,
    storage: bool,
) -> SessionContext {
    let mut caps = complete_capabilities();
    caps.verified_flag_delivery = readback;
    caps.team_data_mount = storage;
    let hello = hello(initial.worker_id, caps);
    let fence = authority
        .begin_session(
            &AuthenticatedPeer {
                worker_id: initial.worker_id,
                fingerprint_sha256: initial.certificate_fingerprint_sha256,
            },
            &hello,
        )
        .await
        .unwrap();
    SessionContext {
        fence,
        boot_id: hello.boot_id,
        ..initial.clone()
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn ad_placement_routes_and_reconnect_preserve_stateful_assignment() {
    let fixture = AuthorityFixture::create().await;
    let store = WorkerStore::new(fixture.pool.clone());
    let authority = PostgresWorkerAuthority::new(store.clone(), Duration::from_secs(30));
    let initial = fixture.insert_current_session().await;
    let id = initial.worker_id;
    let bare = ad_request(id, None);
    let mounted = ad_request(id, Some("/data"));

    connect(&authority, &initial, false, true).await;
    assert!(matches!(
        store.place_workload(&bare).await.unwrap(),
        PlacementOutcome::NoCompatibleCapacity
    ));
    assert!(matches!(
        store
            .place_workload(&placement(id, "ordinary".into()))
            .await
            .unwrap(),
        PlacementOutcome::Placed(_)
    ));
    connect(&authority, &initial, true, false).await;
    assert!(matches!(
        store.place_workload(&bare).await.unwrap(),
        PlacementOutcome::Placed(_)
    ));
    assert!(matches!(
        store.place_workload(&mounted).await.unwrap(),
        PlacementOutcome::NoCompatibleCapacity
    ));
    connect(&authority, &initial, true, true).await;
    assert!(matches!(
        store.place_workload(&mounted).await.unwrap(),
        PlacementOutcome::Placed(_)
    ));

    let item = InventoryItem {
        fence: WorkloadFence {
            workload_id: mounted.id,
            assignment_id: mounted.assignment_id,
            generation: 1,
        },
        spec_hash: hex::encode(mounted.definition.spec_hash_sha256),
        state: ObservedWorkloadState::Ready,
        replicas: vec![ReplicaStatus {
            service: "challenge".into(),
            replica: 0,
            ready: true,
            runtime_id: Some("fixture".into()),
            detail: None,
        }],
    };
    let stream = DataStreamRequest::TcpProxy(TcpProxyRequest {
        fence: item.fence,
        service: "challenge".into(),
        port: "service".into(),
        replica: Some(0),
    });
    // An exact inventory remains owned during a downgrade. Its Ready report alone
    // must never publish a route or authorize a player/checker data stream.
    for (readback, storage) in [(true, false), (false, true), (true, true)] {
        let session = connect(&authority, &initial, readback, storage).await;
        let compatible = readback && storage;
        let due = store.list_due_workloads(Utc::now(), 100).await.unwrap();
        assert_eq!(
            due.iter().any(|due| due.workload.id == mounted.id),
            compatible
        );
        let cleanup = authority
            .handle_inventory_snapshot(&session, Uuid::new_v4(), vec![item.clone()])
            .await
            .unwrap();
        assert!(cleanup.is_empty());
        let route = store.ready_workload_session(mounted.id).await.unwrap();
        assert_eq!(
            route.map(|route| route.fence.session_id),
            compatible.then_some(session.fence.session_id)
        );
        match authority.authorize_data_stream(&session, &stream).await {
            Ok(()) => assert!(compatible),
            Err(WorkerError::Authorization) => assert!(!compatible),
            Err(other) => panic!("unexpected stream failure: {other}"),
        }
        if !compatible {
            assert_eq!(
                store
                    .update_workload_definition(&UpdateWorkload {
                        id: mounted.id,
                        assignment_id: mounted.assignment_id,
                        expected_generation: 1,
                        definition: next_definition(&mounted),
                    })
                    .await
                    .unwrap(),
                DefinitionUpdateOutcome::WorkerNoLongerCompatible
            );
        }
    }
    for request in [&bare, &mounted] {
        store
            .mark_desired_absent(request.id, request.assignment_id, 1)
            .await
            .unwrap();
    }
    connect(&authority, &initial, false, false).await;
    let due = store.list_due_workloads(Utc::now(), 100).await.unwrap();
    assert!(due.iter().any(|due| due.workload.id == bare.id));
    assert!(!due.iter().any(|due| due.workload.id == mounted.id));
    connect(&authority, &initial, false, true).await;
    assert!(store
        .list_due_workloads(Utc::now(), 100)
        .await
        .unwrap()
        .iter()
        .any(|due| due.workload.id == mounted.id));
    fixture.destroy().await;
}
