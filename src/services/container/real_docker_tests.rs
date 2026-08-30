use super::naming::legacy_operation_container_name;
use super::{
    scoped_operation_id, ContainerManager, ContainerSpec, DockerContainerManager,
    DEFAULT_CONTAINER_STORAGE_MB,
};

#[tokio::test]
async fn retryable_flag_adopts_one_real_docker_workload() {
    let Ok(image) = std::env::var("RSCTF_REAL_DOCKER_RETRY_IMAGE") else {
        return;
    };
    let manager = DockerContainerManager::connect()
        .expect("RSCTF_REAL_DOCKER_RETRY_IMAGE requires a reachable Docker daemon");
    let operation_id = format!("test-retryable-flag:{}", uuid::Uuid::new_v4());
    let spec = |operation_id: &str| ContainerSpec {
        game_kind: rsctf_worker_protocol::GameKind::Jeopardy,
        image: image.clone(),
        memory_limit: 64,
        cpu_count: 1,
        storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
        expose_port: 8080,
        publish_port: false,
        proxy_only: false,
        env: Vec::new(),
        flag: Some(crate::utils::flag_generator::generate_retryable_flag(
            Some("flag{[GUID]-[UUID]}"),
            "real-docker-team-secret",
            operation_id,
        )),
        ad_network: None,
        allow_egress: false,
        control_plane_callback_ports: Vec::new(),
        network_mode: crate::utils::enums::NetworkMode::Isolated,
        operation_id: Some(operation_id.to_string()),
    };

    let first = manager
        .create(spec(&operation_id))
        .await
        .expect("first Docker create");
    let scoped_operation = scoped_operation_id(&manager.scope, Some(&operation_id))
        .expect("stable operation identity");
    let legacy_name = legacy_operation_container_name(&image, &[], &scoped_operation);
    manager
        .client()
        .expect("real Docker client")
        .rename_container(
            &first.id,
            bollard::container::RenameContainerOptions { name: legacy_name },
        )
        .await
        .expect("simulate the previous replica's image-prefixed workload name");
    let retried = manager.create(spec(&operation_id)).await;
    let mut changed = spec(&operation_id);
    changed.memory_limit += 1;
    let changed_result = manager.create(changed).await;
    for container in [&changed_result].into_iter().flatten() {
        if container.id != first.id {
            manager
                .destroy(&container.id)
                .await
                .expect("unexpected duplicate workload cleanup");
        }
    }
    let second = retried.expect("same operation and flag should adopt");

    assert_eq!(first.id, second.id, "retry launched a duplicate workload");
    assert!(matches!(
        changed_result,
        Err(crate::utils::error::AppError::Conflict(_))
    ));
    let docker = manager.client().expect("real Docker client");
    docker
        .stop_container(
            &first.id,
            Some(bollard::container::StopContainerOptions { t: 1 }),
        )
        .await
        .expect("stop the adopted workload into a real terminal state");
    let terminal_retry = manager.create(spec(&operation_id)).await;
    assert!(matches!(
        terminal_retry,
        Err(crate::utils::error::AppError::Conflict(_))
    ));
    let missing = docker
        .inspect_container(&first.id, None)
        .await
        .expect_err("terminal operation holder was not removed before rotating the key");
    assert!(
        super::docker::is_not_found(&missing),
        "terminal operation cleanup returned an unexpected Docker error: {missing}"
    );

    let replacement_operation = format!("test-retryable-flag:{}", uuid::Uuid::new_v4());
    let replacement = manager
        .create(spec(&replacement_operation))
        .await
        .expect("a new operation identity recreates the workload");
    manager
        .destroy(&replacement.id)
        .await
        .expect("real Docker retry workload cleanup");
}
