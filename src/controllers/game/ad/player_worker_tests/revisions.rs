//! Managed worker image transitions and durable team data.
use super::*;
use crate::app_state::SharedState;
use crate::services::ad_engine::{AdvancedRound, ProvisionFlag};
use crate::services::container::{ContainerLiveness, ContainerManager};
use crate::services::worker::WorkerContainerManager;
use crate::services::worker_store::{DefinitionUpdateOutcome, UpdateWorkload};

async fn replace_image(
    store: &WorkerStore,
    current: WorkerHandle,
    worker: Uuid,
    participant: i32,
    challenge: i32,
    image: &str,
) -> (WorkerHandle, UpdateWorkload) {
    let manager = WorkerContainerManager::new(store.clone());
    let reference = format!("worker://{worker}/{image}");
    let operation = Uuid::new_v4().to_string();
    let next = manager
        .revise_team_service(current, participant, challenge, &reference, &operation)
        .await
        .unwrap();
    let retry = manager
        .revise_team_service(current, participant, challenge, &reference, &operation)
        .await
        .unwrap();
    assert_eq!(next.id, retry.id);
    let handle = crate::services::worker::parse_worker_handle(&next.id).unwrap();
    assert_eq!(handle.generation, current.generation + 1);
    let definition = store
        .get_workload(handle.workload_id)
        .await
        .unwrap()
        .unwrap()
        .definition;
    (
        handle,
        UpdateWorkload {
            id: handle.workload_id,
            assignment_id: handle.assignment_id,
            expected_generation: current.generation,
            definition,
        },
    )
}

pub(super) async fn scored_receipts(pool: &PgPool, round: i32) -> serde_json::Value {
    sqlx::query_scalar(
        r#"SELECT jsonb_agg(to_jsonb(r) ORDER BY team_service_id)
        FROM "AdFlagDeliveryResults" r WHERE round_id=$1"#,
    )
    .bind(round)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub(super) async fn exercise(
    state: &SharedState,
    worker: Uuid,
    teams: [WorkerHandle; 2],
    round: &AdvancedRound,
    starter: &str,
) -> WorkerHandle {
    let patch = std::env::var("RSCTF_WORKER_TEST_PATCH_IMAGE_ID").unwrap();
    let second = std::env::var("RSCTF_WORKER_TEST_SECOND_PATCH_IMAGE_ID").unwrap();
    assert_ne!(starter, patch);
    assert_ne!(starter, second);
    assert_ne!(patch, second);
    let [mut current, peer] = teams;
    let plane = state.workers.as_ref().unwrap();
    let backend = WorkerContainerManager::new(state.worker_store.clone());
    let flag = round
        .flags
        .iter()
        .find(|item| item.container_id == Some(current.encode()))
        .unwrap();
    let peer_flag = round
        .flags
        .iter()
        .find(|item| item.container_id == Some(peer.encode()))
        .unwrap();
    let official =
        ProvisionFlag::read(state.pg(), 900001, flag.participation_id, flag.challenge_id)
            .await
            .unwrap();
    let receipts = scored_receipts(state.pg(), round.id).await;
    assert_eq!(receipts.as_array().unwrap().len(), 2);
    let id = team_container(worker, current).await;
    let peer_id = team_container(worker, peer).await;
    let save = "printf %s \"$1\" > /data/team-marker";
    docker(&["exec", &id, "sh", "-ec", save, "sh", "initial"]).await;
    docker(&["exec", &peer_id, "sh", "-ec", save, "sh", "peer"]).await;
    let mut previous_marker = "initial";
    let mut first_patch = None;
    for (image, marker) in [
        (patch.as_str(), "first"),
        (second.as_str(), "second"),
        (patch.as_str(), "previous"),
        (starter, "starter"),
    ] {
        let retained = current.encode();
        crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
            &state.db, &retained,
        )
        .await
        .unwrap();
        let (next, request) = replace_image(
            &state.worker_store,
            current,
            worker,
            flag.participation_id,
            flag.challenge_id,
            image,
        )
        .await;
        if let (Some(first), "previous") = (&first_patch, marker) {
            // Both requests name the same immutable image, but the earlier
            // activation cannot claim the receipt of this later rollback.
            assert_eq!(
                state
                    .worker_store
                    .update_workload_definition(first)
                    .await
                    .unwrap(),
                DefinitionUpdateOutcome::Stale
            );
        } else if marker == "first" {
            first_patch = Some(request);
        }
        assert_eq!(
            backend.inspect_liveness(&current.encode()).await.unwrap(),
            ContainerLiveness::Unknown
        );
        assert_eq!(
            backend.inspect_liveness(&next.encode()).await.unwrap(),
            ContainerLiveness::Running
        );
        assert!(plane
            .open_data_stream(worker, health_request(current))
            .await
            .is_err());
        assert!(plane
            .open_data_stream(worker, health_request(next))
            .await
            .is_err());
        assert_health(plane, worker, peer).await;
        official
            .initialize(
                state,
                flag.participation_id,
                flag.challenge_id,
                &next.encode(),
                Some(&retained),
            )
            .await
            .unwrap();
        verify_team_flag(worker, next, &flag.flag).await;
        let reference = format!("worker://{worker}/{image}");
        publish_fixture_endpoint(state.pg(), flag.participation_id, next, &reference).await;
        assert_health(plane, worker, next).await;
        let id = team_container(worker, next).await;
        let actual = docker(&["inspect", "--format", "{{.Image}}", &id]).await;
        assert_eq!(std::str::from_utf8(&actual).unwrap().trim(), image);
        assert_eq!(
            docker(&["exec", &id, "cat", "/data/team-marker"]).await,
            previous_marker.as_bytes()
        );
        docker(&["exec", &id, "sh", "-ec", save, "sh", marker]).await;
        previous_marker = marker;
        assert_eq!(team_container(worker, peer).await, peer_id);
        assert_eq!(
            docker(&["exec", &peer_id, "cat", "/data/team-marker"]).await,
            b"peer"
        );
        verify_team_flag(worker, peer, &peer_flag.flag).await;
        assert_eq!(scored_receipts(state.pg(), round.id).await, receipts);
        current = next;
    }
    current
}
