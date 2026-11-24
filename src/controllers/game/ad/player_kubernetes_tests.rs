use super::*;
use crate::services::ad::reset_fixture::runtime_state;
use crate::services::container::{ContainerResourceLimits, ContainerSpec, TeamStorage};
use crate::services::k8s::KubernetesContainerManager;
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires disposable Kind, CSI, a registry, and PostgreSQL"]
async fn player_kubernetes_patch_activation_and_rollbacks() {
    assert_eq!(std::env::var("RSCTF_K8S_LIVE_STORAGE").as_deref(), Ok("1"));
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let starter = std::env::var("RSCTF_K8S_TEST_STARTER").unwrap();
    let previous = std::env::var("RSCTF_K8S_TEST_REVISION_ONE").unwrap();
    let candidate = std::env::var("RSCTF_K8S_TEST_REVISION_TWO").unwrap();
    let config = std::env::var("RSCTF_K8S_TEST_CANDIDATE_CONFIG").unwrap();
    let fixture = ad_scoring_fixture().await;
    let pool = &fixture.pool;
    let runtime_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let state = runtime_state(
        &runtime_pool,
        Arc::new(KubernetesContainerManager::connect().await.unwrap()),
    );
    let captain = super::cycle::register_service(pool, &starter, &previous, 6379).await;
    let flag: String = sqlx::query_scalar(
        r#"SELECT flag FROM "AdFlags" WHERE round_id=900108 AND team_service_id=900041"#,
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let mut spec = ContainerSpec::ad_service(
        previous.clone(),
        ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 1,
            storage_limit: 64,
        },
        6379,
        900011,
        false,
        String::new(),
    );
    TeamStorage::configure(&state, &mut spec, 900001, 900021, 900031)
        .await
        .unwrap();
    spec.set_ad_launch_flag(flag.clone(), true);
    let first = state.containers.create(spec).await.unwrap();
    let installed = state
        .containers
        .exec(&first.id, vec!["cat".into(), "/flag".into()])
        .await
        .unwrap();
    assert_eq!(installed.trim(), flag);
    assert_eq!(
        state
            .containers
            .exec(
                &first.id,
                [
                    "sh",
                    "-ec",
                    "echo team-one > /data/installation && cat /data/installation"
                ]
                .map(String::from)
                .to_vec()
            )
            .await
            .unwrap()
            .trim(),
        "team-one"
    );
    sqlx::query(
        r#"UPDATE "AdTeamServices" SET host=$1,port=$2,container_id=$3,published_image_backend=$3,
        published_image_ref=$4 WHERE id=900041"#,
    )
    .bind(&first.ip)
    .bind(first.port)
    .bind(&first.id)
    .bind(&previous)
    .execute(pool)
    .await
    .unwrap();
    let serving = super::cycle::exercise(
        &state,
        captain,
        [&starter, &previous, &candidate],
        &config,
        None,
        async |id, label| {
            let files = state
                .containers
                .exec(
                    id,
                    [
                        "sh",
                        "-ec",
                        "printenv RSCTF_TEST_REVISION && cat /data/installation /flag",
                    ]
                    .map(String::from)
                    .to_vec(),
                )
                .await
                .unwrap();
            assert_eq!(files.trim(), format!("{label}\nteam-one\n{flag}"));
        },
    )
    .await;
    state.containers.destroy(&serving).await.unwrap();
    runtime_pool.close().await;
    fixture.cleanup().await;
}
