//! Run teardown against real PostgreSQL, local storage, Docker, and the capture
//! reconciler. Inject backend failures at the adapter boundary, then recover.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

use futures::FutureExt;
use sea_orm::SqlxPostgresConnector;
use sea_orm_migration::MigratorTrait;

use super::*;
use crate::services::container::{
    ContainerBackendKind, ContainerInfo, ContainerManager, ContainerSpec, ContainerStatus,
    DockerContainerManager,
};
use crate::utils::error::{AppError, AppResult};

struct Faults {
    docker: DockerContainerManager,
    fail_export: AtomicBool,
    fail_destroy: AtomicBool,
    exports: AtomicUsize,
    destroys: AtomicUsize,
}

#[async_trait::async_trait]
impl ContainerManager for Faults {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::Docker
    }
    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        self.docker.create(spec).await
    }
    async fn query(&self, id: &str) -> AppResult<ContainerStatus> {
        self.docker.query(id).await
    }
    async fn export(&self, id: &str) -> AppResult<Vec<u8>> {
        self.exports.fetch_add(1, Ordering::SeqCst);
        if self.fail_export.swap(false, Ordering::SeqCst) {
            return Err(AppError::unavailable("injected export failure"));
        }
        self.docker.export(id).await
    }
    async fn destroy(&self, id: &str) -> AppResult<()> {
        self.destroys.fetch_add(1, Ordering::SeqCst);
        if self.fail_destroy.swap(false, Ordering::SeqCst) {
            return Err(AppError::unavailable("injected destroy failure"));
        }
        self.docker.destroy(id).await
    }
}

async fn retry(pool: &sqlx::PgPool, id: i32) -> Uuid {
    sqlx::query(
        r#"UPDATE "AdTeamServices" SET cleanup_next_at = '-infinity',
        cleanup_lease_until = clock_timestamp() - interval '1 second' WHERE id = $1"#,
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
    let claimed = claim(pool, Kind::Ad).await.unwrap().unwrap();
    assert_eq!(claimed.0, id);
    claimed.1
}

async fn retained(pool: &sqlx::PgPool, id: i32, runtime: &str, token: Uuid) {
    let row: (Option<String>, Option<Uuid>) = sqlx::query_as(
        r#"SELECT container_id, cleanup_lease_token FROM "AdTeamServices" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(row, (Some(runtime.to_string()), Some(token)));
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL and RSCTF_REAL_DOCKER_RETRY_IMAGE"]
async fn real_cleanup_preserves_snapshot_and_runtime_across_export_capture_and_destroy_failures() {
    let image =
        std::env::var("RSCTF_REAL_DOCKER_RETRY_IMAGE").expect("disposable Docker image required");
    assert!(
        !crate::services::ad_vpn::enabled(),
        "fixture does not own a VPN namespace"
    );
    let db = crate::utils::test_database::TestDatabase::new("cleanup_runtime").await;
    let connection = SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone());
    crate::migrations::Migrator::up(&connection, None)
        .await
        .unwrap();
    let root = std::env::temp_dir().join(format!("cleanup-runtime-{}", Uuid::new_v4()));
    let manager = Arc::new(Faults {
        docker: DockerContainerManager::connect().unwrap(),
        fail_export: AtomicBool::new(true),
        fail_destroy: AtomicBool::new(true),
        exports: AtomicUsize::new(0),
        destroys: AtomicUsize::new(0),
    });
    let engine = bollard::Docker::connect_with_local_defaults().unwrap();
    let image_digest = engine.inspect_image(&image).await.unwrap().id.unwrap();
    let mut config = crate::models::internal::configs::AppConfig::default();
    config.storage_root = root.to_string_lossy().into_owned();
    let state = crate::app_state::AppState::new(
        connection,
        Arc::new(config),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.join("blobs"))),
        crate::services::token::TokenService::new("cleanup-runtime-test-0123456789abcdef", 60),
        manager.clone(),
    );
    let actor = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
        (id, user_name, normalized_user_name, email_confirmed, phone_number_confirmed,
         two_factor_enabled, lockout_enabled, access_failed_count, role, ip,
         last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name,
         std_number, exercise_visible)
        VALUES ($1, 'fixture', 'FIXTURE', TRUE, FALSE, FALSE, FALSE, 0, 1, '',
            now(), now(), now(), '', '', '', TRUE)"#,
    )
    .bind(actor)
    .execute(&db.pool)
    .await
    .unwrap();
    let game_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Games"
        (title, public_key, private_key, hidden, practice_mode, summary, content,
         accept_without_review, allow_user_submissions, writeup_required,
         team_member_count_limit, container_count_limit, start_time_utc, end_time_utc,
         writeup_deadline, writeup_note, blood_bonus_value, ad_allow_snapshot_download,
         ad_scoring_paused)
        VALUES ('Cleanup', '', '', TRUE, FALSE, '', '', TRUE, FALSE, FALSE, 0, 0,
            now() - interval '2 hours', now() - interval '1 hour', now(), '', 0, TRUE, FALSE)
        RETURNING id"#,
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let team_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Teams" (name, locked, invite_token, captain_id)
        VALUES ('Cleanup', FALSE, $1, $2) RETURNING id"#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(actor)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let participation_id: i32 = sqlx::query_scalar(r#"INSERT INTO "Participations"
        (status, token, game_id, team_id, suspicion_score) VALUES (1, 'fixture', $1, $2, 0) RETURNING id"#)
        .bind(game_id).bind(team_id).fetch_one(&db.pool).await.unwrap();
    let challenge_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "GameChallenges"
        (game_id, title, content, category, "Type", is_enabled, submission_limit,
         accepted_count, submission_count, review_status, build_status,
         enable_traffic_capture, enable_shared_container, original_score, min_score_rate,
         difficulty, score_curve, ad_allow_egress, ad_allow_self_reset,
         ad_ssh_requires_flag, ad_self_hosted, container_image, build_image_digest)
        VALUES ($1, 'Cleanup', '', 0, 4, TRUE, 0, 0, 0, 0, 1, TRUE, FALSE,
            500, 0.25, 5, 0, FALSE, FALSE, FALSE, FALSE, $2, $3) RETURNING id"#,
    )
    .bind(game_id)
    .bind(&image)
    .bind(&image_digest)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let runtime = manager
        .create(ContainerSpec {
            game_kind: rsctf_worker_protocol::GameKind::Jeopardy,
            image: image_digest,
            memory_limit: 64,
            cpu_count: 1,
            storage_limit: 32,
            expose_port: 8080,
            publish_port: false,
            proxy_only: false,
            env: vec![],
            flag: None,
            ad_network: None,
            allow_egress: false,
            control_plane_callback_ports: vec![],
            network_mode: crate::utils::enums::NetworkMode::Isolated,
            operation_id: Some(format!("cleanup-fixture:{}", Uuid::new_v4())),
        })
        .await
        .unwrap();
    let mut reconciler = None;
    let outcome = std::panic::AssertUnwindSafe(async {
        let id: i32 = sqlx::query_scalar(r#"INSERT INTO "AdTeamServices"
            (game_id, participation_id, challenge_id, host, port, status, container_id)
            VALUES ($1, $2, $3, '127.0.0.1', 8080, 0, $4) RETURNING id"#)
            .bind(game_id).bind(participation_id).bind(challenge_id).bind(&runtime.id)
            .fetch_one(&db.pool).await.unwrap();
        let (_, first_token) = claim(&db.pool, Kind::Ad).await.unwrap().unwrap();
        assert_eq!(super::super::backend_reaper::reap_ad_service(&state, id).await.unwrap(), 0);
        finish(&db.pool, Kind::Ad, id, first_token, false).await.unwrap();
        retained(&db.pool, id, &runtime.id, first_token).await;
        assert!(manager.docker.is_running(&runtime.id).await);
        assert!(crate::services::blob_refs::load_service_snapshot(&db.pool, id).await.unwrap().is_none());
        assert_eq!(manager.destroys.load(Ordering::SeqCst), 0);

        // No capture owner is running: real reconciliation waits to its timeout.
        // Export succeeds first, and neither identity nor archive may be lost.
        let token = retry(&db.pool, id).await;
        assert_eq!(super::super::backend_reaper::reap_ad_service(&state, id).await.unwrap(), 0);
        finish(&db.pool, Kind::Ad, id, token, false).await.unwrap();
        retained(&db.pool, id, &runtime.id, token).await;
        assert!(manager.docker.is_running(&runtime.id).await);
        assert_eq!(manager.destroys.load(Ordering::SeqCst), 0);
        let snapshot = crate::services::blob_refs::load_service_snapshot(&db.pool, id).await.unwrap().unwrap();
        let bytes = state.storage.load(&snapshot.hash).await.unwrap();
        let gzip = flate2::read::GzDecoder::new(bytes.as_slice());
        assert!(tar::Archive::new(gzip).entries().unwrap().next().unwrap().is_ok());

        let (shutdown, rx) = watch::channel(false);
        let handle = crate::services::traffic::start_capture_reconciler(state.clone(), rx);
        reconciler = Some((shutdown, handle));
        let token = retry(&db.pool, id).await;
        assert_eq!(super::super::backend_reaper::reap_ad_service(&state, id).await.unwrap(), 0);
        finish(&db.pool, Kind::Ad, id, token, false).await.unwrap();
        retained(&db.pool, id, &runtime.id, token).await;
        assert!(manager.docker.is_running(&runtime.id).await);
        assert_eq!(manager.destroys.load(Ordering::SeqCst), 1);
        assert_eq!(manager.exports.load(Ordering::SeqCst), 2, "retry reuses the exact final snapshot");

        let recovered = retry(&db.pool, id).await;
        assert!(!finish(&db.pool, Kind::Ad, id, token, true).await.unwrap());
        assert_eq!(super::super::backend_reaper::reap_ad_service(&state, id).await.unwrap(), 1);
        assert!(finish(&db.pool, Kind::Ad, id, recovered, true).await.unwrap());
        let withdrawn: (Option<String>, Option<Uuid>, String, i32) = sqlx::query_as(
            r#"SELECT container_id, cleanup_lease_token, host, port FROM "AdTeamServices" WHERE id = $1"#)
            .bind(id).fetch_one(&db.pool).await.unwrap();
        assert_eq!(withdrawn, (None, None, String::new(), 0));
        assert!(!manager.docker.is_running(&runtime.id).await);
        assert!(matches!(engine.inspect_container(&runtime.id, None).await,
            Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. })));
        assert_eq!(manager.destroys.load(Ordering::SeqCst), 2);
        let retained = crate::services::blob_refs::load_service_snapshot(&db.pool, id).await.unwrap().unwrap();
        assert_eq!(retained.id, snapshot.id);
        assert_eq!(state.storage.load(&retained.hash).await.unwrap(), bytes);
        let successes: i64 = sqlx::query_scalar(r#"SELECT count(*) FROM "TrafficCaptureReconcileRequests"
            WHERE container_id = $1 AND action = 'Stop' AND succeeded = TRUE"#)
            .bind(&runtime.id).fetch_one(&db.pool).await.unwrap();
        assert!(successes >= 2);
    }).catch_unwind().await;
    if let Some((shutdown, handle)) = reconciler {
        let _ = shutdown.send(true);
        tokio::time::timeout(Duration::from_secs(15), handle)
            .await
            .unwrap()
            .unwrap();
    }
    manager.docker.destroy(&runtime.id).await.unwrap();
    drop(state);
    db.close().await;
    if root.exists() {
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
