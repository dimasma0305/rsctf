use super::*;
use sea_orm::SqlxPostgresConnector;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;
use tokio::time::{sleep, timeout};

async fn history(pool: &sqlx::PgPool, service: i32) -> Value {
    let mut connection = pool.acquire().await.unwrap();
    let response = crate::services::byoc_tunnel::source::history(
        &mut connection,
        Some(service),
        serde_json::from_str("{}").unwrap(),
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn await_change(pool: &sqlx::PgPool, kind: &str) -> std::time::Duration {
    let started = Instant::now();
    timeout(Duration::from_secs(155), async {
        loop {
            let page = history(pool, 900041).await;
            if page["events"].as_array().unwrap().iter().any(|event| {
                event["status"] == "observed"
                    && event["changes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item["path"] == "main.py" && item["kind"] == kind)
            }) {
                return;
            }
            sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("team history never recorded main.py as {kind}"));
    started.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL and the Docker fixtures from scripts/test-byoc-source-compose.py"]
async fn downloaded_byoc_bundle_observes_in_container_patches() {
    let root = PathBuf::from(std::env::var_os("RSCTF_TEST_BYOC_BUNDLE").unwrap());
    // Each invocation owns its Docker project and volumes, even when two
    // disposable database schemas reuse the same seeded game IDs.
    let project: i32 = std::env::var("RSCTF_TEST_BYOC_GAME")
        .unwrap()
        .parse()
        .unwrap();
    let fixture = crate::services::ad::scoring::test_fixture::ad_scoring_fixture().await;
    let game = fixture.game_id;
    let challenge = 900031;
    // The scoring fixture seeds disabled logins. These two teams need
    // eligible captains for the real shared-credential handshake.
    sqlx::query(r#"UPDATE "AspNetUsers" SET role=$1 WHERE id IN (SELECT captain_id FROM "Teams" WHERE id IN (900011,900012))"#)
        .bind(crate::utils::enums::Role::User as i16)
        .execute(&fixture.pool).await.unwrap();
    sqlx::query(r#"UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=$1"#)
        .bind(game)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let state = crate::app_state::AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(fixture.pool.clone()),
        Arc::new(crate::models::internal::configs::AppConfig::default()),
        Arc::new(crate::services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.join("blobs"))),
        crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    crate::services::ad_vpn::acquire_instance_lease(&state.db)
        .await
        .unwrap();
    let monitor = crate::services::ad_vpn::start_instance_lease_monitor();
    let listener =
        tokio::net::TcpListener::bind((std::env::var("RSCTF_TEST_BYOC_BIND").unwrap(), 0))
            .await
            .unwrap();
    let address = listener.local_addr().unwrap();
    let router = crate::controllers::game::ad::stateful_router().with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let token = byoc_token(
        "adbyocagent:",
        "fixture-private",
        "fixture-team-one",
        900021,
        challenge,
    );
    let ctx = ByocContext {
        title: "Source history fixture".into(),
        agent_image: format!("rsctf-source-test-{project}:agent"),
        agent_image_requires_amd64: false,
        container_image: Some(std::env::var("RSCTF_TEST_BYOC_IMAGE_ID").unwrap()),
        image_url: format!("file://{}/service.tar", root.display()),
        tunnel_url: format!(
            "ws://{address}/api/stateful/Game/{game}/Ad/Byoc/Agent/900021/{challenge}/{token}"
        ),
        svc_port: 3000,
        wg_config: String::new(),
    };
    std::fs::write(root.join("standalone.yml"), build_compose(project, 1, &ctx)).unwrap();
    std::fs::write(root.join("setup.sh"), build_setup_script(project, 1, &ctx)).unwrap();
    assert!(tokio::process::Command::new("sh")
        .arg("setup.sh")
        .current_dir(&root)
        .status()
        .await
        .unwrap()
        .success());
    let baseline = await_change(&fixture.pool, "present").await;
    let compose = root.join(format!("rsctf-byoc-{project}-1/docker-compose.yml"));
    let patch = tokio::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(compose)
        .args(["exec", "-T", "service", "sh", "-ec",
            "test \"$(cat /app/main.py)\" = original; test ! -e /app/.rsctf-files; echo patched >/app/main.py"])
        .status().await.unwrap();
    assert!(patch.success());
    let changed = await_change(&fixture.pool, "modified").await;
    assert_eq!(
        history(&fixture.pool, 900042).await["events"],
        serde_json::json!([])
    );
    eprintln!("BYOC server observed baseline in {baseline:?} and in-container edit in {changed:?}");
    server.abort();
    state
        .byoc
        .disconnect_challenge(&state.db, challenge)
        .await
        .unwrap();
    crate::services::ad_vpn::release_instance_lease()
        .await
        .unwrap();
    monitor.await.unwrap();
    fixture.cleanup().await;
}
