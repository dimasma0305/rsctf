use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::SqlxPostgresConnector;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::models::internal::configs::AppConfig;
use crate::services::ad::scoring::test_fixture::ad_scoring_fixture;
use crate::services::cache::InMemoryCache;
use crate::services::container::{capacity, ContainerManager, DockerContainerManager};
use crate::services::control_jobs::{self, ControlJobKind};
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

struct LiveService(String);

impl Drop for LiveService {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
#[ignore = "requires PostgreSQL, Docker and the CI static-flag-service image"]
async fn missing_reset_image_preserves_live_endpoint_and_scoring() {
    let image = std::env::var("RSCTF_TEST_CONTAINER_IMAGE").unwrap();
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let manager = capacity::CapacityGatedDockerManager::from_env(
        DockerContainerManager::connect().unwrap(),
        pool.clone(),
    )
    .unwrap();
    manager.profile_pull_image(None, &image).await.unwrap();
    let live = LiveService(docker(&[
        "run",
        "-d",
        "--rm",
        "-p",
        "127.0.0.1::8080",
        &image,
    ]));
    let addr = docker(&["port", &live.0, "8080/tcp"])
        .parse::<std::net::SocketAddr>()
        .unwrap();
    let url = format!("http://{addr}/health");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if client
                .get(&url)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();

    let storage = std::env::temp_dir().join(format!("rsctf-reset-{}", Uuid::new_v4()));
    let state = AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone()),
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(storage)),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(manager),
    );
    sqlx::raw_sql(r#"
        SET LOCAL rsctf.maintenance_writer = 'on';
        UPDATE "Games" SET end_time_utc=now()+interval '1 hour' WHERE id=900001;
        UPDATE "GameChallenges" SET ad_self_hosted=FALSE, build_status=1,
            build_image_digest='sha256:' || repeat('f',64) WHERE id=900031;
        UPDATE "AdRounds" SET finalized=FALSE WHERE id=900108;
        UPDATE "AdCheckResults" SET sla_credit=NULL WHERE round_id=900108 AND team_service_id=900041;
    "#).execute(pool).await.unwrap();
    sqlx::query(r#"UPDATE "AdTeamServices" SET host=$1, port=$2, container_id=$3 WHERE id=900041"#)
        .bind(addr.ip().to_string())
        .bind(i32::from(addr.port()))
        .bind(&live.0)
        .execute(pool)
        .await
        .unwrap();
    let input = json!({
        "serviceId":900041, "participationId":900021, "expectedBackendId":live.0, "playerPolicy":false
    });
    let kind = ControlJobKind::AdReset;
    let job = control_jobs::enqueue(
        pool,
        kind,
        "fixture-reset",
        fixture.game_id,
        Some(900031),
        Uuid::new_v4(),
        &"a".repeat(64),
        input,
    )
    .await
    .unwrap();
    let claimed = control_jobs::claim_next(pool, kind, Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let snapshot = || {
        sqlx::query_scalar::<_, Value>(
            r#"
        SELECT jsonb_build_array(to_jsonb(s), to_jsonb(c), to_jsonb(f), j.input)
          FROM "AdTeamServices" s
          JOIN "AdCheckResults" c ON c.team_service_id=s.id AND c.round_id=900108
          JOIN "AdFlags" f ON f.team_service_id=s.id AND f.round_id=c.round_id
          CROSS JOIN "ControlPlaneJobs" j WHERE s.id=900041 AND j.id=$1
    "#,
        )
        .bind(job.id)
        .fetch_one(pool)
    };
    let original = snapshot().await.unwrap();
    for _ in 0..2 {
        let error = super::execute_job(&state, &claimed).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("daemon-local image id is absent"),
            "{error}"
        );
        assert_eq!(snapshot().await.unwrap(), original);
        assert_eq!(
            client
                .get(&url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .text()
                .await
                .unwrap()
                .trim(),
            "ok"
        );
    }
    drop(live);
    fixture.cleanup().await;
}
