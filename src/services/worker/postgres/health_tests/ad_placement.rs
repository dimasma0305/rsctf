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
    rehash(next)
}

fn rehash(mut next: WorkloadDefinition) -> WorkloadDefinition {
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
    for mount in [None, Some("/data")] {
        let session = connect(&authority, &initial, true, true).await;
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
        // A full inventory taken just before the revision switch arrives
        // after the new generation commits but before EnsureWorkload runs.
        let donor = InventoryItem {
            fence: WorkloadFence {
                workload_id: request.id,
                assignment_id: request.assignment_id,
                generation: 1,
            },
            spec_hash: hex::encode(request.definition.spec_hash_sha256),
            state: ObservedWorkloadState::Ready,
            replicas: vec![ReplicaStatus {
                service: "challenge".into(),
                replica: 0,
                ready: true,
                runtime_id: Some("previous".into()),
                detail: None,
            }],
        };
        let inventory =
            |items| authority.handle_inventory_snapshot(&session, Uuid::new_v4(), items);
        assert!(inventory(vec![donor.clone()]).await.unwrap().is_empty());
        assert_eq!(
            store
                .get_workload(request.id)
                .await
                .unwrap()
                .unwrap()
                .observed_state,
            WorkloadObservedState::Unknown
        );
        // A different assignment is still an orphan, even if it reports the
        // same workload UUID as the donor.
        let mut orphan = donor.clone();
        orphan.fence.assignment_id = Uuid::new_v4();
        assert!(
            matches!(inventory(vec![orphan.clone()]).await.unwrap().as_slice(),
            [ControlMessage::EnsureAbsent(command)] if command.fence == orphan.fence)
        );
        sqlx::query(
            r#"UPDATE "WorkerWorkloads" SET observed_state='Failed' WHERE id=$1 AND generation=2"#,
        )
        .bind(request.id)
        .execute(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(backend.query(&current).await.unwrap().status, "exited");
        assert_eq!(
            backend.query(&previous.encode()).await.unwrap().status,
            "unknown"
        );
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
        assert!(inventory(vec![donor.clone()]).await.unwrap().is_empty());
        assert!(inventory(Vec::new()).await.unwrap().is_empty());
        let deleted = store.get_workload(request.id).await.unwrap().unwrap();
        assert_eq!(deleted.generation, 3);
        assert_eq!(
            deleted.desired_state,
            crate::services::worker_store::WorkloadDesiredState::Absent
        );
        assert_ne!(deleted.observed_state, WorkloadObservedState::Absent);
        assert!(store
            .list_due_workloads(Utc::now(), 100)
            .await
            .unwrap()
            .iter()
            .any(|due| due.workload.id == request.id));
        sqlx::query(
            r#"UPDATE "WorkerNodes" SET lease_expires_at=now()-interval '1 second' WHERE id=$1"#,
        )
        .bind(initial.worker_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
        assert!(matches!(
            inventory(vec![donor]).await,
            Err(WorkerError::StaleSession)
        ));
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
    let fixture = crate::services::ad::scoring::test_fixture::ad_scoring_fixture().await;
    let store = WorkerStore::new(fixture.pool.clone());
    let authority = PostgresWorkerAuthority::new(store.clone(), Duration::from_secs(30));
    let initial = AuthorityFixture::insert_current_session_on(&fixture.pool).await;
    let id = initial.worker_id;
    let bare = ad_request(id, None);
    let mut mounted = ad_request(id, Some("/data"));
    mounted.owner_key = crate::services::ad::service_lifecycle::service_lock_key(900021, 900031);

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
    sqlx::raw_sql(
        r#"UPDATE "Games" SET end_time_utc=now()+interval '1 hour';
        UPDATE "GameChallenges" SET ad_self_hosted=false WHERE id=900031"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let backend = WorkerHandle {
        workload_id: mounted.id,
        assignment_id: mounted.assignment_id,
        generation: 1,
    }
    .encode();
    // An exact inventory remains owned during a downgrade. Its Ready report alone
    // must never publish a route or authorize a player/checker data stream.
    let mut published = false;
    for (readback, storage) in [(true, true), (true, false), (false, true), (true, true)] {
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
        if !published && compatible {
            // Swapping the handle into the other team's published service must not grant a route.
            sqlx::query(
                r#"UPDATE "AdTeamServices" SET container_id=$1,published_image_backend=$1,host='192.0.2.1',port=8080 WHERE id=900042"#,
            )
                .bind(&backend)
                .execute(&fixture.pool)
                .await
                .unwrap();
            assert!(matches!(
                authority.authorize_data_stream(&session, &stream).await,
                Err(WorkerError::Authorization)
            ));
            sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=NULL,published_image_backend=NULL WHERE id=900042"#)
                .execute(&fixture.pool)
                .await
                .unwrap();
            use crate::services::ad::service_lifecycle::{
                publish_managed_backend_if_eligible, ManagedBackendPublication,
            };
            assert!(publish_managed_backend_if_eligible(
                &mut fixture.pool.acquire().await.unwrap(),
                ManagedBackendPublication {
                    game_id: fixture.game_id,
                    participation_id: 900021,
                    challenge_id: 900031,
                    host: "192.0.2.1",
                    port: 8080,
                    backend_id: &backend,
                    image_ref: "fixture-image"
                }
            )
            .await
            .unwrap());
            published = true;
        }
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
    // These definitions validate on their own, but the worker cannot change
    // the volume registration of an existing assignment. Keep the route live.
    let session = connect(&authority, &initial, true, true).await;
    authority
        .handle_inventory_snapshot(&session, Uuid::new_v4(), vec![item])
        .await
        .unwrap();
    for (request, kind, path) in [
        (&bare, "attackDefense", Some("/data")),
        (&mounted, "attackDefense", None),
        (&mounted, "attackDefense", Some("/var/lib/replacement")),
        (&mounted, "jeopardy", None),
    ] {
        let mut definition = next_definition(request);
        definition.spec["gameKind"] = json!(kind);
        definition.spec["teamDataMount"] = json!(path);
        let result = store
            .update_workload_definition(&UpdateWorkload {
                id: request.id,
                assignment_id: request.assignment_id,
                expected_generation: 1,
                definition: rehash(definition),
            })
            .await;
        assert!(matches!(result, Err(WorkerStoreError::Conflict(_))));
        let unchanged = store.get_workload(request.id).await.unwrap().unwrap();
        assert_eq!(unchanged.generation, 1);
        assert_eq!(unchanged.definition.spec, request.definition.spec);
        authority
            .authorize_data_stream(&session, &stream)
            .await
            .unwrap();
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
    fixture.cleanup().await;
}
