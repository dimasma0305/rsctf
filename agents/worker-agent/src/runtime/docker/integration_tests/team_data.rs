use super::*;
use bollard::container::UploadToContainerOptions;
use rsctf_worker_protocol::CommandErrorCode;

fn ensure(spec: &ValidatedWorkloadSpec, fence: WorkloadFence) -> TestResult<EnsureWorkload> {
    Ok(EnsureWorkload {
        command_id: Uuid::new_v4(),
        fence,
        spec_hash: spec.spec_hash()?,
        timeout_ms: 30_000,
        spec: spec.clone(),
    })
}

fn absent(command: &EnsureWorkload) -> EnsureAbsent {
    EnsureAbsent {
        command_id: Uuid::new_v4(),
        fence: command.fence,
        spec_hash: command.spec_hash.clone(),
        timeout_ms: 30_000,
    }
}

async fn launch(runtime: &DockerRuntime, command: &EnsureWorkload) -> TestResult<String> {
    let status = runtime.ensure_workload(command.clone()).await?;
    let id = match status.replicas.as_slice() {
        [replica] => replica
            .runtime_id
            .clone()
            .ok_or_else(|| test_error("missing team container ID")),
        _ => Err(test_error("expected one hosted team container")),
    }?;
    if let ImageIdentity::WorkerLocal { image_id, .. } = &command.spec.services[0].image {
        require(
            runtime
                .docker
                .inspect_container(&id, None)
                .await?
                .image
                .as_ref()
                == Some(image_id),
            "team container started with a different image from the requested revision",
        )?;
    }
    Ok(id)
}

async fn marker(runtime: &DockerRuntime, id: &str, bytes: &[u8]) -> TestResult {
    if std::env::var_os("RSCTF_WORKER_TEST_PATCH_IMAGE_ID").is_some() {
        use bollard::exec::{CreateExecOptions, StartExecResults};
        use futures_util::TryStreamExt;
        let exec = runtime
            .docker
            .create_exec(
                id,
                CreateExecOptions {
                    attach_stderr: Some(true),
                    cmd: Some(vec![
                        "sh",
                        "-ec",
                        r#"test "$(id -u)" != 0 && printf %s "$1" > /data/marker"#,
                        "sh",
                        std::str::from_utf8(bytes)?,
                    ]),
                    ..Default::default()
                },
            )
            .await?;
        if let StartExecResults::Attached { mut output, .. } =
            runtime.docker.start_exec(&exec.id, None).await?
        {
            while output.try_next().await?.is_some() {}
        }
        return require(
            runtime.docker.inspect_exec(&exec.id).await?.exit_code == Some(0),
            "the service user could not write its own data directory",
        );
    }
    runtime
        .docker
        .upload_to_container(
            id,
            Some(UploadToContainerOptions {
                path: "/data",
                no_overwrite_dir_non_dir: "",
            }),
            super::super::make_single_file_archive("marker".into(), bytes.to_vec()).await?,
        )
        .await?;
    Ok(())
}

async fn volume_name(runtime: &DockerRuntime, id: &str) -> TestResult<String> {
    let inspected = runtime.docker.inspect_container(id, None).await?;
    inspected
        .mounts
        .unwrap_or_default()
        .into_iter()
        .find(|m| m.destination.as_deref() == Some("/data"))
        .and_then(|m| m.name)
        .ok_or_else(|| test_error("team data volume not attached"))
}

pub(super) async fn check(
    runtime: &DockerRuntime,
    base: &ValidatedWorkloadSpec,
    options: RuntimeOptions,
) -> TestResult {
    let mut starter = base.clone().into_inner();
    require(
        runtime.capabilities().team_data_mount,
        "Linux worker did not advertise team storage",
    )?;
    starter.game_kind = GameKind::AttackDefense;
    starter.services.truncate(1);
    starter.services[0].replicas = 1;
    starter.services[0].stateless = false;
    starter.team_data_mount = Some("/data".into());
    let starter = ValidatedWorkloadSpec::try_from(starter)?;
    let fence = || WorkloadFence {
        workload_id: Uuid::new_v4(),
        assignment_id: Uuid::new_v4(),
        generation: 1,
    };
    let first = ensure(&starter, fence())?;
    let peer = ensure(&starter, fence())?;
    let donor = launch(runtime, &first).await?;
    let other = launch(runtime, &peer).await?;
    let volume = volume_name(runtime, &donor).await?;
    require(
        volume != volume_name(runtime, &other).await?,
        "different teams share one Docker volume",
    )?;
    marker(runtime, &donor, b"owner").await?;
    marker(runtime, &other, b"peer").await?;

    let mut patch = starter.clone().into_inner();
    patch.services[0]
        .environment
        .insert("PATCH".into(), "one".into());
    if let Ok(image_id) = std::env::var("RSCTF_WORKER_TEST_PATCH_IMAGE_ID") {
        patch.services[0].image = ImageIdentity::WorkerLocal {
            worker_id: runtime.worker_id,
            image_id,
        };
    }
    let next = WorkloadFence {
        generation: 2,
        ..first.fence
    };
    for mount in [None, Some("/var/lib/other".to_owned())] {
        let mut invalid = patch.clone();
        invalid.team_data_mount = mount;
        let error = runtime
            .ensure_workload(ensure(&invalid.try_into()?, next)?)
            .await
            .unwrap_err();
        require(
            error.code == CommandErrorCode::SpecConflict,
            "existing team storage declaration could be replaced",
        )?;
    }
    require(
        runtime
            .verify_guest_file(&donor, "/data/marker", b"owner")
            .await?,
        "invalid replacement disturbed the donor",
    )?;
    let patch = ensure(&patch.try_into()?, next)?;
    let replacement = launch(runtime, &patch).await?;
    require(
        replacement != donor && volume_name(runtime, &replacement).await? == volume,
        "patch failed to retain the original volume",
    )?;
    require(
        runtime
            .verify_guest_file(&replacement, "/data/marker", b"owner")
            .await?,
        "patch lost the team's marker",
    )?;
    marker(runtime, &replacement, b"updated").await?;

    let restarted =
        DockerRuntime::connect(runtime.worker_id, None, &runtime.state_dir, options).await?;
    require(
        launch(&restarted, &patch).await? == replacement,
        "restart replay replaced a live writer",
    )?;
    require(
        restarted
            .ensure_absent(absent(&first))
            .await
            .unwrap_err()
            .code
            == CommandErrorCode::StaleGeneration,
        "stale teardown reached the team's volume",
    )?;
    let mut second = patch.spec.clone().into_inner();
    second.services[0]
        .environment
        .insert("PATCH".into(), "two".into());
    if let Ok(image_id) = std::env::var("RSCTF_WORKER_TEST_SECOND_PATCH_IMAGE_ID") {
        second.services[0].image = ImageIdentity::WorkerLocal {
            worker_id: runtime.worker_id,
            image_id,
        };
    }
    let second = ensure(
        &second.try_into()?,
        WorkloadFence {
            generation: 3,
            ..next
        },
    )?;
    let latest = launch(&restarted, &second).await?;
    marker(&restarted, &latest, b"latest team data").await?;
    let previous = ensure(
        &patch.spec,
        WorkloadFence {
            generation: 4,
            ..next
        },
    )?;
    let previous_id = launch(&restarted, &previous).await?;
    require(
        restarted
            .verify_guest_file(&previous_id, "/data/marker", b"latest team data")
            .await?,
        "rolling back to the previous image lost live data",
    )?;

    let rollback = ensure(
        &starter,
        WorkloadFence {
            generation: 5,
            ..next
        },
    )?;
    let restored = launch(&restarted, &rollback).await?;
    require(
        restored != replacement && volume_name(&restarted, &restored).await? == volume,
        "rollback failed to retain the registered volume",
    )?;
    require(
        restarted
            .verify_guest_file(&restored, "/data/marker", b"latest team data")
            .await?,
        "rollback reverted live team data",
    )?;
    restarted.ensure_absent(absent(&rollback)).await?;
    restarted.ensure_absent(absent(&rollback)).await?;
    require(
        restarted
            .docker
            .inspect_volume(&volume)
            .await
            .is_err_and(|e| is_not_found(&e)),
        "final team teardown retained the volume",
    )?;
    require(
        restarted
            .verify_guest_file(&other, "/data/marker", b"peer")
            .await?,
        "team teardown damaged its peer",
    )?;
    // Simulate an external storage loss while the service is detached. A
    // retry must report the loss instead of serving a freshly seeded volume.
    let peer_volume = volume_name(&restarted, &other).await?;
    restarted
        .docker
        .remove_container(
            &other,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await?;
    let record = restarted
        .state_dir
        .join(format!("team-volume-{}.json", peer.fence.workload_id));
    let backup = record.with_extension("test-backup");
    tokio::fs::rename(&record, &backup).await?;
    let lost_record = restarted.ensure_workload(peer.clone()).await.unwrap_err();
    require(
        lost_record.code == CommandErrorCode::SpecConflict && !record.exists(),
        "a peer's existing volume was adopted without its registration",
    )?;
    tokio::fs::rename(&backup, &record).await?;
    restarted.docker.remove_volume(&peer_volume, None).await?;
    let missing = restarted.ensure_workload(peer.clone()).await.unwrap_err();
    require(
        missing.code == CommandErrorCode::SpecConflict,
        "lost team storage was silently recreated",
    )?;
    require(
        restarted
            .docker
            .inspect_volume(&peer_volume)
            .await
            .is_err_and(|e| is_not_found(&e)),
        "unexpected empty replacement volume",
    )?;
    restarted.ensure_absent(absent(&peer)).await?;
    Ok(())
}
