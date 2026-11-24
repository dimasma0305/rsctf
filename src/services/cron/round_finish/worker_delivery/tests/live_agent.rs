use super::*;
use crate::services::worker::{build_mtls_server_config, start_reconciler};
use crate::services::worker_store::WorkloadObservedState;
use rcgen::{CertificateParams, KeyPair};
use rsctf_worker_protocol::WorkloadFence;
use serde_json::json;
use std::{
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use tokio::{net::TcpListener, process::Command, sync::watch};
use tokio_rustls::rustls::{pki_types::PrivatePkcs8KeyDer, RootCertStore};
#[path = "live_agent/revisions.rs"]
mod revisions;

struct IdentityDir(PathBuf);
impl Drop for IdentityDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct LiveWorker {
    service: Arc<WorkerService>,
    agent: tokio::process::Child,
    listener: tokio::task::JoinHandle<()>,
    reconciler: tokio::task::JoinHandle<()>,
}

fn private_file(dir: &Path, name: &str, contents: &[u8]) {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join(name))
        .unwrap()
        .write_all(contents)
        .unwrap();
}

async fn place_team(
    store: &WorkerStore,
    worker: Uuid,
    image: &str,
    participant: i32,
) -> WorkerHandle {
    use crate::services::container::{ContainerResourceLimits, ContainerSpec};
    let spec = ContainerSpec::ad_service(
        format!("worker://{worker}/{image}"),
        ContainerResourceLimits {
            cpu_count: 1,
            memory_limit: 64,
            storage_limit: 128,
        },
        8080,
        participant,
        false,
        String::new(),
    );
    let placed = crate::services::worker::WorkerContainerManager::new(store.clone())
        .place_team_service(&spec, participant, 900031)
        .await
        .unwrap();
    crate::services::worker::parse_worker_handle(&placed.id).unwrap()
}

async fn test_agent(
    pool: &PgPool,
    worker: Uuid,
    dir: &Path,
    store: WorkerStore,
    shutdown: watch::Receiver<bool>,
) -> LiveWorker {
    let issuer = crate::services::worker_pki::tests::issuer().0;
    let key = KeyPair::generate().unwrap();
    let csr = CertificateParams::new(Vec::<String>::new())
        .unwrap()
        .serialize_request(&key)
        .unwrap()
        .pem()
        .unwrap();
    let identity = issuer.issue(worker, &csr).unwrap();
    sqlx::query(r#"INSERT INTO "WorkerNodes" (id,name,certificate_fingerprint_sha256,certificate_serial,certificate_expires_at)
         VALUES ($1,'round-worker',$2,$3,$4)"#)
        .bind(worker).bind(identity.fingerprint_sha256.as_slice()).bind(identity.serial).bind(identity.expires_at).execute(pool).await.unwrap();
    let server_key = KeyPair::generate().unwrap();
    let server = CertificateParams::new(vec!["localhost".into()])
        .unwrap()
        .self_signed(&server_key)
        .unwrap();
    let mut roots = RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut std::io::Cursor::new(&identity.ca_certificate_pem)) {
        roots.add(cert.unwrap()).unwrap();
    }
    let tls = build_mtls_server_config(
        vec![server.der().clone()],
        PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
        roots,
    )
    .unwrap();
    let service = Arc::new(
        WorkerService::new(
            WorkerServerConfig::default(),
            Arc::new(
                PostgresWorkerAuthority::new(store.clone(), Duration::from_secs(30))
                    .with_issuer(issuer),
            ),
        )
        .unwrap(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let live = service.clone();
    let stop = shutdown.clone();
    let listener =
        tokio::spawn(async move { live.serve(listener, Arc::new(tls), stop).await.unwrap() });
    let reconciler = start_reconciler(store, service.clone(), shutdown);
    private_file(dir, "server.pem", server.pem().as_bytes());
    private_file(dir, "worker.pem", identity.certificate_pem.as_bytes());
    private_file(dir, "worker.key", key.serialize_pem().as_bytes());
    let config = json!({"workerId":worker,"controlAddress":addr,"dataAddress":addr,"serverName":"localhost",
        "certificatePath":"worker.pem","privateKeyPath":"worker.key","caPath":"server.pem",
        "capacity":{"cpuMillis":2000,"memoryBytes":536870912,"slots":2}});
    private_file(dir, "worker.json", &serde_json::to_vec(&config).unwrap());
    let binary = std::env::var_os("RSCTF_TEST_LIVE_AD_WORKER_BIN")
        .expect("set the same-build trusted worker binary");
    let mut agent = Command::new(binary)
        .kill_on_drop(true)
        .args([
            "run",
            "--accept-host-network-boundary",
            "--minimum-free-bytes",
            "1073741824",
            "--config",
        ])
        .arg(dir.join("worker.json"))
        .env("RUST_LOG", "warn")
        .spawn()
        .unwrap();
    while !service.registry().is_online(worker).await {
        assert!(
            agent.try_wait().unwrap().is_none(),
            "worker exited before enrollment became ready"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    LiveWorker {
        service,
        agent,
        listener,
        reconciler,
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL, a dedicated Docker daemon, fixture image and same-build Linux worker binary"]
async fn live_remote_agent_publishes_two_team_round_receipts() {
    tokio::time::timeout(Duration::from_secs(240), scenario())
        .await
        .expect("timed out waiting for worker or Docker");
}

async fn docker(args: &[&str]) -> Vec<u8> {
    let output = Command::new("docker").args(args).output().await.unwrap();
    assert!(output.status.success(), "Docker {} failed", args[0]);
    output.stdout
}

fn health_request(handle: WorkerHandle) -> rsctf_worker_protocol::DataStreamRequest {
    use rsctf_worker_protocol::{DataStreamRequest, TcpProxyRequest};
    DataStreamRequest::TcpProxy(TcpProxyRequest {
        fence: WorkloadFence {
            workload_id: handle.workload_id,
            assignment_id: handle.assignment_id,
            generation: handle.generation.try_into().unwrap(),
        },
        service: "challenge".into(),
        port: "service".into(),
        replica: None,
    })
}

async fn assert_health(service: &WorkerService, worker: Uuid, handle: WorkerHandle) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = service
        .open_data_stream(worker, health_request(handle))
        .await
        .unwrap();
    stream
        .write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    stream.flush().await.unwrap();
    let mut response = String::new();
    stream
        .take(4096)
        .read_to_string(&mut response)
        .await
        .unwrap();
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .expect("HTTP health response");
    assert!(headers.lines().next().unwrap().contains(" 200 "));
    assert_eq!(body.trim(), "ok");
}

async fn verify_team_health_stream(
    service: &WorkerService,
    worker: Uuid,
    handle: WorkerHandle,
    pool: &PgPool,
    team_service: i32,
) {
    assert_health(service, worker, handle).await;
    sqlx::query(r#"UPDATE "AdTeamServices" SET host='',port=0 WHERE id=$1 AND container_id=$2"#)
        .bind(team_service)
        .bind(handle.encode())
        .execute(pool)
        .await
        .unwrap();
    assert!(matches!(
        service
            .open_data_stream(worker, health_request(handle))
            .await,
        Err(crate::services::worker::WorkerError::Authorization)
    ));
}

async fn scenario() {
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let image = std::env::var("RSCTF_WORKER_TEST_IMAGE_ID")
        .expect("set a local disposable fixture image ID");
    assert!(
        docker(&[
            "volume",
            "ls",
            "-q",
            "--filter",
            "name=^rsctf-worker-owner$"
        ])
        .await
        .is_empty(),
        "the test requires a fresh dedicated Docker daemon"
    );
    let fixture = ad_scoring_fixture().await;
    sqlx::raw_sql(
        r#"UPDATE "Games" SET end_time_utc=now()+interval '1 hour', ad_tick_seconds=120;
       UPDATE "GameChallenges" SET ad_self_hosted=false WHERE id=900031"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let worker = Uuid::new_v4();
    let path = std::env::temp_dir().join(format!("rsctf-ad-worker-{worker}"));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    let dir = IdentityDir(path);
    let store = WorkerStore::new(fixture.pool.clone());
    let (stop, shutdown) = watch::channel(false);
    let mut live = test_agent(
        &fixture.pool,
        worker,
        &dir.0,
        store.clone(),
        shutdown.clone(),
    )
    .await;
    let mut state = runtime_state(
        &fixture.pool,
        Arc::new(crate::services::worker::WorkerContainerManager::new(
            store.clone(),
        )),
    );
    Arc::get_mut(&mut state).unwrap().workers = Some(live.service.clone());
    crate::services::ad_vpn::acquire_instance_lease(&state.db)
        .await
        .unwrap();
    let lease = crate::services::ad_vpn::start_instance_lease_monitor();
    let relays = crate::services::worker::start_team_relays(
        state.clone(),
        live.service.as_ref().clone(),
        shutdown,
    );
    sqlx::query(
        r#"INSERT INTO "AdPatchStarters"(challenge_id,tree_id,archive,data_mount)
        VALUES(900031,repeat('a',64),'fixture'::bytea,'/data')"#,
    )
    .execute(&fixture.pool)
    .await
    .unwrap();
    let mut targets = Vec::new();
    for (service, participant) in [(900041, 900021), (900042, 900022)] {
        let handle = place_team(&store, worker, &image, participant).await;
        while store
            .ready_workload_session(handle.workload_id)
            .await
            .unwrap()
            .is_none()
        {
            let workload = store
                .get_workload(handle.workload_id)
                .await
                .unwrap()
                .unwrap();
            assert_ne!(
                workload.observed_state,
                WorkloadObservedState::Failed,
                "worker could not start the team fixture: {}",
                workload.observed_message.as_deref().unwrap_or("no detail")
            );
            assert!(
                live.agent.try_wait().unwrap().is_none(),
                "worker exited before Docker became ready"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        publish_fixture_endpoint(&fixture.pool, participant, handle, &image).await;
        targets.push((service, handle));
    }
    let round = prepare_round(
        &state.db,
        fixture.game_id,
        Some(RoundCursor {
            id: 900108,
            number: 8,
        }),
        Some(true),
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(round.flags.len(), 2);
    assert!(round.flags[0].flag != round.flags[1].flag);
    let (service, handle) = targets[0];
    let first = round
        .flags
        .iter()
        .find(|flag| flag.team_service_id == service)
        .unwrap();
    let retained = handle.encode();
    crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
        &state.db, &retained,
    )
    .await
    .unwrap();
    let flag = crate::services::ad_engine::ProvisionFlag::read(
        &fixture.pool,
        fixture.game_id,
        first.participation_id,
        first.challenge_id,
    )
    .await
    .unwrap();
    assert!(flag.official.as_deref() == Some(first.flag.as_str()));
    flag.initialize(
        &state,
        first.participation_id,
        first.challenge_id,
        &retained,
        Some(&retained),
    )
    .await
    .unwrap();
    verify_team_flag(worker, handle, &first.flag).await;
    let unpublished: (String, i32, i64) = sqlx::query_as(
        r#"SELECT host, port, (SELECT COUNT(*) FROM "AdFlagDeliveryResults" WHERE round_id=$2 AND delivered)
           FROM "AdTeamServices" WHERE id=$1"#)
        .bind(service).bind(round.id).fetch_one(&fixture.pool).await.unwrap();
    assert_eq!(unpublished, (String::new(), 0, 0));
    publish_fixture_endpoint(&fixture.pool, first.participation_id, handle, &image).await;
    for _ in 0..2 {
        let (sender, _receipts) = tokio::sync::mpsc::unbounded_channel();
        let publication = super::super::super::publish_round_flags(
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
        .unwrap();
        assert_eq!(publication.failure_count, 0);
    }
    let updated =
        revisions::exercise(&state, worker, [targets[0].1, targets[1].1], &round, &image).await;
    for (service, original) in targets {
        let handle = if service == first.team_service_id {
            updated
        } else {
            original
        };
        let recorded: (String, i16) = sqlx::query_as(
            r#"SELECT container_id,attempts FROM "AdFlagDeliveryResults"
               WHERE round_id=$1 AND team_service_id=$2 AND delivered AND delivery_kind='Managed'"#,
        )
        .bind(round.id)
        .bind(service)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
        assert_eq!(recorded, (original.encode(), 1));
        let flag = round
            .flags
            .iter()
            .find(|flag| flag.team_service_id == service)
            .unwrap();
        verify_team_flag(worker, handle, &flag.flag).await;
        verify_team_health_stream(
            state.workers.as_ref().unwrap(),
            worker,
            handle,
            &fixture.pool,
            service,
        )
        .await;
        store
            .mark_desired_absent(handle.workload_id, handle.assignment_id, handle.generation)
            .await
            .unwrap();
        while store
            .get_workload(handle.workload_id)
            .await
            .unwrap()
            .unwrap()
            .observed_state
            != WorkloadObservedState::Absent
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    stop.send(true).unwrap();
    live.agent.kill().await.unwrap();
    live.listener.await.unwrap();
    live.reconciler.await.unwrap();
    relays.await.unwrap();
    crate::services::ad_vpn::release_instance_lease()
        .await
        .unwrap();
    lease.await.unwrap();
    fixture.cleanup().await;
}

async fn verify_team_flag(worker: Uuid, handle: WorkerHandle, value: &str) {
    let id = team_container(worker, handle).await;
    let read = docker(&["exec", &id, "cat", "/flag"]).await;
    assert!(read == value.as_bytes());
}

async fn team_container(worker: Uuid, handle: WorkerHandle) -> String {
    let ids = docker(&[
        "ps",
        "-q",
        "--no-trunc",
        "--filter",
        &format!("label=io.rsctf.worker.id={worker}"),
        "--filter",
        &format!("label=io.rsctf.workload.id={}", handle.workload_id),
    ])
    .await;
    let id = std::str::from_utf8(&ids).unwrap().trim();
    assert!(id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    id.to_owned()
}
