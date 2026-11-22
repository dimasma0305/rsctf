use super::*;
use crate::services::{
    ad::{reset_fixture::runtime_state, scoring::test_fixture::ad_scoring_fixture},
    ad_engine::{prepare_round, RoundCursor},
    container::NoopContainerManager,
    worker::{PostgresWorkerAuthority, SessionContext, WorkerServerConfig, WorkerService},
    worker_store::WorkerStore,
};
use rsctf_worker_protocol::{CommandResult, ControlEnvelope, ControlMessage};
use std::{collections::HashSet, sync::Arc};

async fn register_targets(pool: &PgPool, context: &SessionContext) -> Vec<WorkerHandle> {
    sqlx::raw_sql(
        r#"UPDATE "Games" SET end_time_utc=now()+interval '1 hour', ad_tick_seconds=120;
        UPDATE "GameChallenges" SET ad_self_hosted=FALSE WHERE id=900031"#,
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "WorkerNodes"
        (id,name,capabilities,certificate_fingerprint_sha256,certificate_serial,certificate_expires_at,
         session_id,session_epoch,boot_id,connected_at,heartbeat_at,lease_expires_at)
        VALUES ($1,'scoring-fixture','{"writeFlag":true,"verifiedFlagDelivery":true}',
                $2,'fixture',now()+interval '1 hour',$3,1,$4,now(),now(),now()+interval '1 hour')"#)
        .bind(context.worker_id).bind(context.certificate_fingerprint_sha256.as_slice())
        .bind(context.fence.session_id).bind(context.boot_id).execute(pool).await.unwrap();
    let mut targets = Vec::new();
    for (service, team) in [(900041, 900021), (900042, 900022)] {
        let handle = WorkerHandle {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 1,
        };
        sqlx::query(r#"INSERT INTO "WorkerWorkloads"
            (id,owner_kind,owner_key,worker_id,assignment_id,generation,spec_hash_sha256,spec,
             required_os,required_architecture,required_runtime,reserved_cpu_millis,
             reserved_memory_bytes,reserved_slots,required_replicas,observed_session_epoch,observed_state)
            VALUES ($1,'ad-team-service',$2,$3,$4,1,$5,
            '{"gameKind":"attackDefense","flagTarget":{"path":"/team/flag","service":"app"},
              "services":[{"name":"app","replicas":1}]}','linux','amd64','docker',0,0,1,1,1,'Ready')"#)
            .bind(handle.workload_id).bind(service_lock_key(team,900031)).bind(context.worker_id)
            .bind(handle.assignment_id).bind([1_u8;32].as_slice()).execute(pool).await.unwrap();
        sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=$1 WHERE id=$2"#)
            .bind(handle.encode())
            .bind(service)
            .execute(pool)
            .await
            .unwrap();
        targets.push(handle);
    }
    targets
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn remote_round_publication_binds_team_generation_round_and_readback() {
    let fixture = ad_scoring_fixture().await;
    let context = SessionContext {
        worker_id: Uuid::new_v4(),
        boot_id: Uuid::new_v4(),
        certificate_fingerprint_sha256: [7; 32],
        fence: SessionFence {
            session_id: Uuid::new_v4(),
            session_epoch: 1,
        },
    };
    let targets = register_targets(&fixture.pool, &context).await;
    let authority = PostgresWorkerAuthority::new(
        WorkerStore::new(fixture.pool.clone()),
        Duration::from_secs(30),
    );
    let workers =
        Arc::new(WorkerService::new(WorkerServerConfig::default(), Arc::new(authority)).unwrap());
    let mut peer = workers
        .registry()
        .register_authenticated_control(context.clone(), 1, true)
        .await
        .unwrap();
    let mut state = runtime_state(&fixture.pool, Arc::new(NoopContainerManager));
    Arc::get_mut(&mut state).unwrap().workers = Some(workers.clone());
    let previous = RoundCursor {
        id: 900108,
        number: 8,
    };
    assert!(prepare_round(
        &state.db,
        fixture.game_id,
        Some(previous),
        Some(false),
        chrono::Utc::now()
    )
    .await
    .is_err());
    let round = prepare_round(
        &state.db,
        fixture.game_id,
        Some(previous),
        Some(true),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(round.flags.len(), 2);
    let first = round
        .flags
        .iter()
        .find(|flag| flag.team_service_id == 900041)
        .unwrap();
    let second = round
        .flags
        .iter()
        .find(|flag| flag.team_service_id == 900042)
        .unwrap();
    let timeout = Duration::from_secs(2);
    // Even a stored endpoint copied from the other team must send no flag.
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=$1 WHERE id=900041"#)
        .bind(targets[1].encode())
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(!deliver(&state, round.id, first, targets[1], timeout).await);
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=$1 WHERE id=900041"#)
        .bind(targets[0].encode())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let mut wrong = first.clone();
    wrong.flag.clone_from(&second.flag);
    assert!(!deliver(&state, round.id, &wrong, targets[0], timeout).await);
    assert!(!deliver(&state, previous.id, first, targets[0], timeout).await);
    assert!(
        !deliver(
            &state,
            round.id,
            first,
            WorkerHandle {
                generation: 2,
                ..targets[0]
            },
            timeout
        )
        .await
    );
    assert!(peer.outbound.try_recv().is_err());

    // A successful terminal response from a generation that lost readiness
    // while handling the request must not become a scored delivery.
    let withdraw = async {
        let request = peer.outbound.recv().await.unwrap();
        sqlx::query(r#"UPDATE "WorkerWorkloads" SET observed_state='Failed' WHERE id=$1"#)
            .bind(targets[0].workload_id)
            .execute(&fixture.pool)
            .await
            .unwrap();
        acknowledge(&workers, &context, &request).await;
    };
    let (delivered, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            deliver(&state, round.id, first, targets[0], timeout),
            withdraw
        )
    })
    .await
    .unwrap();
    assert!(!delivered);
    sqlx::query(r#"UPDATE "WorkerWorkloads" SET observed_state='Ready' WHERE id=$1"#)
        .bind(targets[0].workload_id)
        .execute(&fixture.pool)
        .await
        .unwrap();

    let publish = || async {
        let (sender, _receipts) = tokio::sync::mpsc::unbounded_channel();
        super::super::publish_round_flags(
            &state,
            fixture.game_id,
            round.id,
            round.number,
            chrono::Utc::now(),
            3,
            round.flags.clone(),
            false,
            tokio::time::Instant::now() + Duration::from_secs(30),
            sender,
        )
        .await
        .unwrap()
    };
    let respond = async {
        let mut seen = HashSet::new();
        for _ in 0..2 {
            let request = peer.outbound.recv().await.unwrap();
            let ControlMessage::WriteFlag(write) = &request.body else {
                panic!("expected flag request")
            };
            let index = targets
                .iter()
                .position(|handle| handle.workload_id == write.fence.workload_id)
                .unwrap();
            assert!(seen.insert(index));
            assert_eq!(targets[index].assignment_id, write.fence.assignment_id);
            assert_eq!(write.flag_sequence, round.id as u64);
            assert_eq!(write.target.path, "/team/flag");
            assert_eq!(write.target.service, "app");
            assert!(write.value == [first, second][index].flag);
            acknowledge(&workers, &context, &request).await;
        }
    };
    let (publication, ()) = tokio::time::timeout(Duration::from_secs(12), async {
        tokio::join!(publish(), respond)
    })
    .await
    .unwrap();
    assert_eq!(publication.failure_count, 0);
    let recorded: Vec<(i32,String)> = sqlx::query_as(r#"SELECT team_service_id,container_id FROM "AdFlagDeliveryResults"
        WHERE round_id=$1 AND delivered AND delivery_kind='Managed' AND attempts=1 ORDER BY team_service_id"#)
        .bind(round.id).fetch_all(&fixture.pool).await.unwrap();
    assert_eq!(
        recorded,
        vec![(900041, targets[0].encode()), (900042, targets[1].encode())]
    );
    assert_eq!(publish().await.failure_count, 0);
    assert!(peer.outbound.try_recv().is_err());
    fixture.cleanup().await;
}

async fn acknowledge(workers: &WorkerService, context: &SessionContext, request: &ControlEnvelope) {
    let reply = ControlEnvelope {
        reply_to: Some(request.message_id),
        ..ControlEnvelope::new(
            request.session_epoch,
            ControlMessage::CommandResult(CommandResult {
                command_id: request.body.command_id().unwrap(),
                success: true,
                error: None,
            }),
        )
    };
    workers
        .registry()
        .handle_command_feedback(context, &reply)
        .await
        .unwrap();
}
