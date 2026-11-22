use super::*;
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
            let mut replacement = mounted.definition.clone();
            replacement.spec["services"][0]["environment"] = json!({"REVISION":"next"});
            let next: ValidatedWorkloadSpec =
                serde_json::from_value(replacement.spec.clone()).unwrap();
            replacement.spec_hash_sha256 = hex::decode(next.spec_hash().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            assert_eq!(
                store
                    .update_workload_definition(&UpdateWorkload {
                        id: mounted.id,
                        assignment_id: mounted.assignment_id,
                        expected_generation: 1,
                        definition: replacement
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
