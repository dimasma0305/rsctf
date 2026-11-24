use super::*;
use crate::services::ad::service_lifecycle::service_lock_key;
use crate::services::ad::service_lifecycle::{
    publish_managed_backend_if_eligible, retain_created_backend_identity, ManagedBackendPublication,
};
use crate::services::{
    ad::{reset_fixture::runtime_state, scoring::test_fixture::ad_scoring_fixture},
    ad_engine::{prepare_round, RoundCursor},
    container::NoopContainerManager,
    worker::{PostgresWorkerAuthority, SessionContext, WorkerServerConfig, WorkerService},
    worker_store::WorkerStore,
};
use rsctf_worker_protocol::SessionFence;
use rsctf_worker_protocol::{CommandResult, ControlEnvelope, ControlMessage};
use sqlx::PgPool;
use std::{collections::HashSet, sync::Arc};
use uuid::Uuid;

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
        VALUES ($1,'scoring-fixture','{"writeFlag":true,"verifiedFlagDelivery":true,"ensureWorkload":true,"tcpProxy":true,"inventory":true,"maxDataLanes":1,"maxWorkloadReplicas":1}',
                $2,'fixture',now()+interval '1 hour',$3,1,$4,now(),now(),now()+interval '1 hour')"#)
        .bind(context.worker_id).bind(context.certificate_fingerprint_sha256.as_slice())
        .bind(context.fence.session_id).bind(context.boot_id).execute(pool).await.unwrap();
    let mut targets = Vec::new();
    for team in [900021, 900022] {
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
        publish_fixture_endpoint(pool, team, handle, "fixture-image").await;
        targets.push(handle);
    }
    targets
}

async fn publish_fixture_endpoint(
    pool: &PgPool,
    participant: i32,
    handle: WorkerHandle,
    image: &str,
) {
    let backend = handle.encode();
    assert!(
        retain_created_backend_identity(pool, 900001, participant, 900031, &backend)
            .await
            .unwrap()
    );
    let endpoint = ManagedBackendPublication {
        game_id: 900001,
        participation_id: participant,
        challenge_id: 900031,
        backend_id: &backend,
        host: "192.0.2.1",
        port: 8080,
        image_ref: image,
    };
    assert!(
        publish_managed_backend_if_eligible(&mut pool.acquire().await.unwrap(), endpoint)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn worker_reset_checks_the_owner_before_retirement_checkpoint_and_publication() {
    use crate::services::ad_engine::{
        checkpoint_reset_replacement, prepare_service_reset, publish_service_reset,
    };
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let db = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
    let context = SessionContext {
        worker_id: Uuid::new_v4(),
        certificate_fingerprint_sha256: [3; 32],
        fence: SessionFence {
            session_id: Uuid::new_v4(),
            session_epoch: 1,
        },
        boot_id: Uuid::new_v4(),
    };
    let handles = register_targets(pool, &context).await;
    let own = handles[0].encode();
    let peer = handles[1].encode();
    let job = Uuid::new_v4();
    let lease = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "ControlPlaneJobs"
        (id,kind,scope_key,game_id,operation_id,fingerprint,status,lease_token,lease_expires_at_utc)
        VALUES ($1,'AdReset','reset-fixture',900001,$1,'fixture',1,$2,now()+interval '1 hour')"#,
    )
    .bind(job)
    .bind(lease)
    .execute(pool)
    .await
    .unwrap();
    sqlx::raw_sql(r#"UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108;
        UPDATE "AdCheckResults" SET sla_credit=NULL WHERE round_id=900108 AND team_service_id=900041"#)
        .execute(pool).await.unwrap();
    let set_backend = async |id: Option<&str>| {
        sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=$1 WHERE id=900041"#)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    };
    let evidence = || {
        sqlx::query_as::<_, (serde_json::Value, serde_json::Value, serde_json::Value)>(
            r#"SELECT to_jsonb(s), to_jsonb(c), j.input FROM "AdTeamServices" s
            JOIN "AdCheckResults" c ON c.team_service_id=s.id AND c.round_id=900108
            CROSS JOIN "ControlPlaneJobs" j WHERE s.id=900041 AND j.id=$1"#,
        )
        .bind(job)
        .fetch_one(pool)
    };
    let prepare = async |id: &str| {
        prepare_service_reset(
            &db,
            fixture.game_id,
            900041,
            Some(id),
            job,
            lease,
            "fixture-image",
            false,
            "fixture reset",
        )
        .await
    };
    set_backend(Some(&peer)).await;
    let before = evidence().await.unwrap();
    assert!(prepare(&peer).await.is_err());
    assert_eq!(evidence().await.unwrap(), before);

    set_backend(Some(&own)).await;
    let prepared = prepare(&own).await.unwrap();
    set_backend(None).await;
    let checkpoint = async |id: &str| {
        checkpoint_reset_replacement(pool, fixture.game_id, 900041, job, lease, None, id).await
    };
    let blank = evidence().await.unwrap();
    assert!(checkpoint(&peer).await.is_err());
    assert_eq!(evidence().await.unwrap(), blank);
    checkpoint(&own).await.unwrap();

    // A legacy receipt can already have a peer's backend. Even an exact
    // match on that receipt must not publish the wrong worker.
    let publish = async |id: &str| {
        publish_service_reset(
            &db,
            fixture.game_id,
            900041,
            "192.0.2.1",
            8080,
            id,
            prepared.prepared_round_id,
            true,
            Some("fixture-image"),
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap()
    };
    set_backend(Some(&peer)).await;
    let retained = evidence().await.unwrap();
    assert!(!publish(&peer).await);
    assert_eq!(evidence().await.unwrap(), retained);
    set_backend(Some(&own)).await;
    assert!(publish(&own).await);
    fixture.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn team_worker_endpoint_writes_and_cleanup_preserve_the_peer() {
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let worker = Uuid::new_v4();
    let context = SessionContext {
        worker_id: worker,
        certificate_fingerprint_sha256: [3; 32],
        fence: SessionFence {
            session_id: Uuid::new_v4(),
            session_epoch: 1,
        },
        boot_id: Uuid::new_v4(),
    };
    let targets = register_targets(pool, &context).await;
    let state = runtime_state(
        pool,
        Arc::new(crate::services::worker::WorkerContainerManager::new(
            WorkerStore::new(pool.clone()),
        )),
    );
    let image = "fixture-image";
    let own = targets[0].encode();
    let endpoint = ManagedBackendPublication {
        game_id: 900001,
        participation_id: 900021,
        challenge_id: 900031,
        backend_id: &own,
        host: "192.0.2.99",
        port: 8080,
        image_ref: image,
    };
    for invalid in [
        targets[1],
        WorkerHandle {
            generation: 2,
            ..targets[0]
        },
        WorkerHandle {
            assignment_id: Uuid::new_v4(),
            ..targets[0]
        },
    ] {
        let backend = invalid.encode();
        assert!(
            !retain_created_backend_identity(pool, 900001, 900021, 900031, &backend)
                .await
                .unwrap()
        );
        assert!(!publish_managed_backend_if_eligible(
            &mut pool.acquire().await.unwrap(),
            ManagedBackendPublication {
                backend_id: &backend,
                ..endpoint
            }
        )
        .await
        .unwrap());
        assert!(matches!(
            crate::services::ad::service_lifecycle::destroy_team_backend(
                &state, 900021, 900031, &backend
            )
            .await,
            Err(crate::utils::error::AppError::Conflict(_))
        ));
    }
    assert!(
        retain_created_backend_identity(pool, 900001, 900021, 900031, "rsctf-worker:broken")
            .await
            .is_err()
    );
    // The publication must protect its workload generation until the caller commits.
    let mut publishing = pool.begin().await.unwrap();
    assert!(publish_managed_backend_if_eligible(
        &mut publishing,
        ManagedBackendPublication {
            host: "192.0.2.1",
            ..endpoint
        }
    )
    .await
    .unwrap());
    let competing = || {
        sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM "WorkerWorkloads" WHERE id=$1 FOR UPDATE NOWAIT"#,
        )
        .bind(targets[0].workload_id)
    };
    let err = competing().fetch_one(pool).await.unwrap_err();
    assert_eq!(
        err.as_database_error().unwrap().code().as_deref(),
        Some("55P03")
    );
    publishing.commit().await.unwrap();
    assert_eq!(
        competing().fetch_one(pool).await.unwrap(),
        targets[0].workload_id
    );
    sqlx::query(
        r#"UPDATE "WorkerNodes" SET lease_expires_at=now()-interval '1 second' WHERE id=$1"#,
    )
    .bind(worker)
    .execute(pool)
    .await
    .unwrap();
    assert!(
        !publish_managed_backend_if_eligible(&mut pool.acquire().await.unwrap(), endpoint)
            .await
            .unwrap()
    );
    let hosts: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT container_id,host FROM "AdTeamServices" WHERE id IN(900041,900042) ORDER BY id"#,
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        hosts,
        [
            (own.clone(), "192.0.2.1".to_owned()),
            (targets[1].encode(), "192.0.2.1".to_owned())
        ]
    );
    // Offline assignments can still be retained for cleanup; deletion stays idempotent.
    assert!(
        retain_created_backend_identity(pool, 900001, 900021, 900031, &own)
            .await
            .unwrap()
    );
    for _ in 0..2 {
        crate::services::ad::service_lifecycle::destroy_team_backend(&state, 900021, 900031, &own)
            .await
            .unwrap();
    }
    let desired:Vec<(String,i64)> = sqlx::query_as(r#"SELECT desired_state,generation FROM "WorkerWorkloads" WHERE worker_id=$1 ORDER BY owner_key"#)
        .bind(worker).fetch_all(pool).await.unwrap();
    assert_eq!(
        desired,
        [("Absent".to_owned(), 2), ("Present".to_owned(), 1)]
    );
    fixture.cleanup().await;
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

    let own = targets[0].encode();
    let other = targets[1].encode();
    crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
        &state.db, &own,
    )
    .await
    .unwrap();
    assert!(!deliver(&state, round.id, first, targets[0], timeout).await);
    let flag = worker_flag::TeamFlag {
        round_id: round.id,
        participation_id: first.participation_id,
        challenge_id: first.challenge_id,
        value: &first.flag,
    };
    let prepare = |retained_backend, handle| {
        worker_flag::deliver(
            &state,
            flag,
            worker_flag::DeliveryTarget::BeforePublication { retained_backend },
            handle,
            timeout,
        )
    };
    assert!(!prepare(Some(&other), targets[0]).await);
    assert!(!prepare(None, targets[0]).await);
    assert!(!prepare(Some(&own), targets[1]).await);
    assert!(peer.outbound.try_recv().is_err());
    // A successful stateless teardown clears the donor pointer. A stale job
    // still expecting that donor must not write, and the blank pointer alone
    // must never authorize a different team's assignment.
    sqlx::query(r#"UPDATE "AdTeamServices" SET container_id=NULL WHERE id=$1"#)
        .bind(first.team_service_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert!(!prepare(Some(&own), targets[0]).await);
    assert!(!prepare(None, targets[1]).await);
    assert!(peer.outbound.try_recv().is_err());
    let replace = async {
        let request = peer.outbound.recv().await.unwrap();
        publish_fixture_endpoint(
            &fixture.pool,
            first.participation_id,
            targets[0],
            "fixture-image",
        )
        .await;
        acknowledge(&workers, &context, &request).await;
    };
    let (confirmed, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(prepare(None, targets[0]), replace)
    })
    .await
    .unwrap();
    assert!(
        !confirmed,
        "a late response cannot initialize a published endpoint"
    );
    assert!(!prepare(Some(&own), targets[0]).await);
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
