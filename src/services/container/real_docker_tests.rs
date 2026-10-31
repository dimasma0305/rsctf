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
        runtime_profile: None,
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

    let mut strict = manager.clone();
    strict.allow_unbounded_storage = false;
    if !strict.detect_storage_quota_enforcement().await.unwrap() {
        let denied = strict.create(spec(&operation_id)).await.unwrap_err();
        assert!(denied.to_string().contains("cannot enforce"));
    }
    let first = manager
        .create(spec(&operation_id))
        .await
        .expect("first Docker create");
    let anonymous_volumes: Vec<String> = manager
        .client()
        .unwrap()
        .inspect_container(&first.id, None)
        .await
        .unwrap()
        .mounts
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mount| mount.name)
        .collect();
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
    for volume in anonymous_volumes {
        assert!(
            manager
                .client()
                .unwrap()
                .inspect_volume(&volume)
                .await
                .is_err(),
            "terminal retry leaked anonymous volume {volume}"
        );
    }

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

#[tokio::test]
async fn scoped_generator_inventory_and_destroy_preserve_foreign_and_named_resources() {
    use bollard::container::{Config, RemoveContainerOptions};
    use bollard::models::HostConfig;
    use bollard::volume::CreateVolumeOptions;
    use std::collections::HashMap;

    let Ok(image) = std::env::var("RSCTF_REAL_DOCKER_RETRY_IMAGE") else {
        return;
    };
    let manager = DockerContainerManager::connect().unwrap();
    let docker = manager.client().unwrap();
    let named = format!("rsctf-audit-kept-{}", uuid::Uuid::new_v4());
    docker
        .create_volume(CreateVolumeOptions {
            name: named.clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut ids = Vec::new();
    let mut anonymous = Vec::new();
    for foreign in [false, true] {
        let mut labels =
            crate::services::event_security::generator_labels(uuid::Uuid::new_v4().to_string());
        if foreign {
            labels = super::scoped_managed_labels("another-installation");
        }
        let created = docker
            .create_container::<String, String>(
                None,
                Config {
                    image: Some(image.clone()),
                    entrypoint: Some(vec!["/bin/sh".to_string()]),
                    cmd: Some(vec!["-c".to_string(), "exit 0".to_string()]),
                    labels: Some(labels),
                    volumes: Some(HashMap::from([(
                        "/audit-anonymous".to_string(),
                        HashMap::new(),
                    )])),
                    host_config: Some(HostConfig {
                        binds: Some(vec![format!("{named}:/audit-kept")]),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        docker
            .start_container::<String>(&created.id, None)
            .await
            .unwrap();
        let info = docker.inspect_container(&created.id, None).await.unwrap();
        if !foreign {
            anonymous = info
                .mounts
                .unwrap()
                .into_iter()
                .filter_map(|mount| mount.name)
                .filter(|name| name != &named)
                .collect();
        }
        ids.push(created.id);
    }
    let inventory = manager.list_managed().await;
    let visible = inventory.contains(&ids[0]) && !inventory.contains(&ids[1]);
    let foreign_rejected = manager.destroy(&ids[1]).await.is_err();
    manager.destroy(&ids[0]).await.unwrap();
    let leaked =
        futures::future::join_all(anonymous.iter().map(|name| docker.inspect_volume(name)))
            .await
            .into_iter()
            .any(|result| result.is_ok());
    let named_preserved = docker.inspect_volume(&named).await.is_ok();
    let foreign_preserved = docker.inspect_container(&ids[1], None).await.is_ok();
    docker
        .remove_container(
            &ids[1],
            Some(RemoveContainerOptions {
                force: true,
                v: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    docker.remove_volume(&named, None).await.unwrap();
    assert!(visible, "generator absent from installation inventory");
    assert!(
        foreign_rejected && foreign_preserved,
        "foreign workload was not protected"
    );
    assert!(
        !anonymous.is_empty() && !leaked,
        "destroy leaked anonymous volumes"
    );
    assert!(
        named_preserved,
        "destroy removed an explicitly named volume"
    );

    let failed = docker
        .create_container::<String, String>(
            None,
            Config {
                image: Some(image),
                entrypoint: Some(vec!["/does-not-exist-audit-fixture".to_string()]),
                labels: Some(super::scoped_managed_labels(&manager.scope)),
                volumes: Some(HashMap::from([(
                    "/audit-anonymous".to_string(),
                    HashMap::new(),
                )])),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let failed_volumes: Vec<_> = docker
        .inspect_container(&failed.id, None)
        .await
        .unwrap()
        .mounts
        .unwrap()
        .into_iter()
        .filter_map(|mount| mount.name)
        .collect();
    assert!(manager
        .start_or_reconcile_container(docker, &failed.id, false, false)
        .await
        .is_err());
    assert!(docker.inspect_container(&failed.id, None).await.is_err());
    for name in failed_volumes {
        assert!(
            docker.inspect_volume(&name).await.is_err(),
            "failed start leaked {name}"
        );
    }
}
