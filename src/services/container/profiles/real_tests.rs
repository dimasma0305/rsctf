//! Opt-in lifecycle proof against actual Docker and Kubernetes/Kata runtimes.
use super::{config, ProfileRouter};
use crate::services::container::{ContainerManager, ContainerSpec, DockerContainerManager};
use crate::utils::{
    enums::NetworkMode,
    error::{AppError, AppResult},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn request(image: &str, profile: &str, operation: &str) -> ContainerSpec {
    ContainerSpec {
        runtime_profile: Some(profile.into()),
        game_kind: rsctf_worker_protocol::GameKind::Jeopardy,
        image: image.into(),
        memory_limit: 256,
        cpu_count: 1,
        storage_limit: 512,
        expose_port: 8080,
        publish_port: false,
        proxy_only: false,
        env: vec![],
        flag: Some("flag{profile-lifecycle-fixture}".into()),
        ad_network: None,
        allow_egress: true,
        control_plane_callback_ports: vec![],
        network_mode: NetworkMode::Open,
        operation_id: Some(operation.into()),
    }
}

async fn ready(router: &ProfileRouter, id: &str) -> AppResult<()> {
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        if router.is_running(id).await {
            let response = router
                .exec(
                    id,
                    vec![
                        "sh".into(),
                        "-c".into(),
                        "wget -q -O- http://127.0.0.1:8080/".into(),
                    ],
                )
                .await;
            if response.is_ok_and(|body| body.contains("nginx")) {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(AppError::unavailable(
        "runtime profile fixture did not become HTTP-ready",
    ))
}

async fn exercise(
    router: &ProfileRouter,
    image: &str,
    sequence: &[&str],
    require_guest: bool,
) -> AppResult<()> {
    let mut owned = Vec::new();
    let result = async {
        let mut host_kernel = None;
        for (index, profile) in sequence.iter().enumerate() {
            let operation = format!("runtime-profiles:{}:{index}", uuid::Uuid::new_v4());
            let spec = request(image, profile, &operation);
            let start = Instant::now();
            let info = router.create(spec.clone()).await?;
            owned.push(info.id.clone());
            ready(router, &info.id).await?;
            let duration = start.elapsed().as_millis();
            let retry = router.create(spec).await?;
            assert_eq!(retry.id, info.id, "same-profile retry must adopt one runtime");
            assert_eq!(router.find_operation_runtime(&operation).await?, Some(info.id.clone()));
            let kernel = router.exec(&info.id, vec!["uname".into(), "-r".into()]).await?;
            if *profile == "docker" { host_kernel = Some(kernel.trim().to_owned()); }
            if *profile == "vm" && require_guest {
                assert_ne!(Some(kernel.trim()), host_kernel.as_deref(), "Kata must execute a guest kernel, not the host kernel");
            }
            let output = router.exec(&info.id, vec!["sh".into(), "-c".into(), "test ! -e /tmp/profile-owner && printf '%s' \"$RSCTF_FLAG\" && touch /tmp/profile-owner".into()]).await?;
            assert_eq!(output, "flag{profile-lifecycle-fixture}");
            if index > 0 {
                let previous = &owned[index - 1];
                assert!(router.is_running(previous).await, "selection must not stop the previous runtime");
                router.destroy(previous).await?;
                assert!(!router.is_running(previous).await);
                assert!(router.is_running(&info.id).await, "old-owner cleanup must preserve the new instance");
            }
            println!("profile={profile} ready_ms={duration} kernel={}", kernel.trim());
        }
        Ok(())
    }.await;
    for id in &owned {
        router.destroy(id).await?;
    }
    result
}

#[tokio::test]
#[ignore = "requires Docker and RSCTF_RUNTIME_PROFILE_TEST_IMAGE"]
async fn real_docker_profiles_keep_retry_and_cleanup_ownership() {
    let image = std::env::var("RSCTF_RUNTIME_PROFILE_TEST_IMAGE")
        .expect("pinned nginx-unprivileged image required");
    let profiles = config::parse(
        r#"[
        {"id":"docker","name":"Docker","backend":"docker","isolation":"container"},
        {"id":"docker-two","name":"Docker two","backend":"docker","isolation":"container"}
    ]"#,
    )
    .unwrap();
    let default = Arc::new(DockerContainerManager::connect().unwrap());
    let entries = profiles
        .into_iter()
        .map(|profile| {
            let manager: Arc<dyn ContainerManager> = Arc::new(
                DockerContainerManager::connect()
                    .unwrap()
                    .with_profile(&profile),
            );
            (profile, manager)
        })
        .collect();
    let router = ProfileRouter::new(default, entries).unwrap();
    exercise(&router, &image, &["docker", "docker-two", "docker"], false)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Docker, PostgreSQL and RSCTF_RUNTIME_PROFILE_TEST_IMAGE"]
async fn real_docker_profiles_share_capacity_even_when_one_profile_reconciles() {
    use crate::services::container::capacity::{CapacityGatedDockerManager, LocalCapacity};
    let image = std::env::var("RSCTF_RUNTIME_PROFILE_TEST_IMAGE").unwrap();
    let db = crate::utils::test_database::TestDatabase::new("profile_capacity").await;
    sqlx::raw_sql(crate::migrations::LOCAL_CONTAINER_CAPACITY_SQL)
        .execute(&db.pool)
        .await
        .unwrap();
    let profiles = config::parse(
        r#"[
        {"id":"capacity-a","name":"A","backend":"docker","isolation":"container"},
        {"id":"capacity-b","name":"B","backend":"docker","isolation":"container"}
    ]"#,
    )
    .unwrap();
    let base = super::super::docker_installation_scope();
    let scopes: Vec<String> = std::iter::once(base.clone())
        .chain(profiles.iter().map(|p| p.scope(&base)))
        .collect();
    let mut entries = Vec::new();
    for profile in profiles {
        let mut docker = DockerContainerManager::connect()
            .unwrap()
            .with_profile(&profile);
        docker.capacity_scopes = scopes.clone();
        let manager: Arc<dyn ContainerManager> =
            Arc::new(CapacityGatedDockerManager::with_capacity(
                docker,
                db.pool.clone(),
                LocalCapacity {
                    cpu_millis: Some(4_000),
                    memory_bytes: 1024 * 1024 * 1024,
                    slots: Some(1),
                },
            ));
        entries.push((profile, manager));
    }
    let router = ProfileRouter::new(Arc::new(super::super::NoopContainerManager), entries).unwrap();
    let first = router
        .create(request(&image, "capacity-a", "capacity-first"))
        .await
        .unwrap();
    let inventory = router.list_managed().await;
    assert!(inventory.contains(&first.id));
    let denied = router
        .create(request(&image, "capacity-b", "capacity-second"))
        .await;
    router.destroy(&first.id).await.unwrap();
    assert!(
        matches!(denied, Err(AppError::RetryableUnavailable { .. })),
        "profile B must not release profile A's host-wide reservation: {denied:?}"
    );
    let second = router
        .create(request(&image, "capacity-b", "capacity-second"))
        .await
        .unwrap();
    router.destroy(&second.id).await.unwrap();
    db.close().await;
}

#[tokio::test]
#[ignore = "requires Docker and RSCTF_RUNTIME_PROFILE_TEST_IMAGE"]
async fn real_docker_profiles_share_the_installation_owned_competitive_network() {
    let image = std::env::var("RSCTF_RUNTIME_PROFILE_TEST_IMAGE").unwrap();
    let profiles = config::parse(
        r#"[
        {"id":"ad-a","name":"A","backend":"docker","isolation":"container"},
        {"id":"ad-b","name":"B","backend":"docker","isolation":"container"}
    ]"#,
    )
    .unwrap();
    let default = Arc::new(DockerContainerManager::connect().unwrap());
    let router = ProfileRouter::new(
        default,
        profiles
            .into_iter()
            .map(|profile| {
                let manager = Arc::new(
                    DockerContainerManager::connect()
                        .unwrap()
                        .with_profile(&profile),
                ) as Arc<dyn ContainerManager>;
                (profile, manager)
            })
            .collect(),
    )
    .unwrap();
    let network = format!("rsctf-profiles-test-{}", uuid::Uuid::new_v4().simple());
    router
        .ensure_network(&network, &crate::services::ad_vpn::services_cidr())
        .await
        .unwrap();
    let mut owned = vec![];
    let result: AppResult<()> = async {
        for profile in ["ad-a", "ad-b"] {
            let mut spec = request(&image, profile, profile);
            spec.game_kind = rsctf_worker_protocol::GameKind::AttackDefense;
            spec.ad_network = Some(network.clone());
            spec.allow_egress = false;
            let instance = router.create(spec).await?;
            owned.push(instance.id.clone());
            ready(&router, &instance.id).await?;
            assert!(!instance.ip.is_empty());
        }
        Ok(())
    }
    .await;
    for id in owned {
        router.destroy(&id).await.unwrap();
    }
    bollard::Docker::connect_with_local_defaults()
        .unwrap()
        .remove_network(&network)
        .await
        .unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires Docker, Kubernetes, Kata/KVM and RSCTF_RUNTIME_PROFILE_TEST_IMAGE"]
async fn real_runtime_profile_switch_matrix_proves_guest_isolation() {
    // Standalone libtest processes do not execute the application's TLS setup.
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let image = std::env::var("RSCTF_RUNTIME_PROFILE_TEST_IMAGE")
        .expect("pinned nginx-unprivileged image required");
    let profiles = config::parse(r#"[
        {"id":"docker","name":"Docker","backend":"docker","isolation":"container"},
        {"id":"kube","name":"Kubernetes","backend":"kubernetes","isolation":"container"},
        {"id":"vm","name":"MicroVM","backend":"kubernetes","isolation":"microvm","runtimeClass":"kata-qemu-runtime-rs"}
    ]"#).unwrap();
    let default = Arc::new(DockerContainerManager::connect().unwrap());
    let mut entries = Vec::new();
    for profile in profiles {
        let manager: Arc<dyn ContainerManager> = match profile.backend {
            super::ProfileBackend::Docker => Arc::new(
                DockerContainerManager::connect()
                    .unwrap()
                    .with_profile(&profile),
            ),
            super::ProfileBackend::Kubernetes => Arc::new(
                crate::services::k8s::KubernetesContainerManager::connect()
                    .await
                    .unwrap()
                    .with_profile(&profile)
                    .await
                    .unwrap(),
            ),
        };
        entries.push((profile, manager));
    }
    let router = ProfileRouter::new(default, entries).unwrap();
    exercise(
        &router,
        &image,
        &["docker", "kube", "vm", "docker", "vm", "kube", "docker"],
        true,
    )
    .await
    .unwrap();
}
