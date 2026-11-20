//! Docker build and pull transport with bounded log capture.
use super::*;

/// Repack a validated ZIP archive as Docker's tar build context. Reject opaque
/// legacy blobs before passing input to the privileged daemon.
pub(super) async fn build_from_context(
    docker: &Docker,
    image: &str,
    archive: Vec<u8>,
    context_subdir: Option<&str>,
) -> BuildOutcome {
    let context_subdir = context_subdir.map(str::to_owned);
    let prepared =
        tokio::task::spawn_blocking(move || zip_bytes_to_tar(&archive, context_subdir.as_deref()))
            .await;
    let tar = match prepared {
        Err(_) => {
            return BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some("Build context preparation interrupted".to_string()),
                image_digest: None,
            };
        }
        Ok(Ok(tar)) => tar,
        Ok(Err(error)) => {
            return BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(format!("Rejected unsafe build archive: {error}")),
                image_digest: None,
            };
        }
    };

    let labels = canonical_managed_image_tag(image)
        .map(|reference| {
            std::collections::HashMap::from([
                (
                    crate::services::container::IMAGE_SCOPE_LABEL.to_string(),
                    crate::services::container::docker_installation_scope(),
                ),
                (
                    crate::services::container::IMAGE_REFERENCE_LABEL.to_string(),
                    reference,
                ),
            ])
        })
        .unwrap_or_default();
    let options = BuildImageOptions::<String> {
        t: image.to_string(),
        dockerfile: "Dockerfile".to_string(),
        rm: true,
        labels,
        version: ARCHIVE_BUILDER_VERSION,
        session: Some(uuid::Uuid::new_v4().to_string()),
        ..Default::default()
    };

    // `Some(_)` pins the tar param to bollard's concrete `bytes::Bytes`, so
    // `.into()` resolves `From<Vec<u8>>` without naming the transitive crate.
    let mut stream = docker.build_image(options, None, Some(tar.into()));

    let mut progress = progress::BuildProgress::default();
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => progress.record(info),
            Err(e) => {
                progress.fail(format!("build transport error: {e:?}"));
                break;
            }
        }
    }
    finish_archive_build(docker, image, progress).await
}

pub(super) async fn finish_archive_build(
    docker: &Docker,
    image: &str,
    progress: progress::BuildProgress,
) -> BuildOutcome {
    let (mut log, result) = progress.into_parts();
    // A tag may already exist from the previous build. Inspect alone cannot
    // establish that this attempt produced it, even when the stream ended cleanly.
    let result = match result {
        Ok(built) => inspect_immutable_image(docker, image, ImageOperation::ArchiveBuild, false)
            .await
            .and_then(|current| {
                if current == built {
                    Ok(current)
                } else {
                    Err("Docker tag no longer refers to the image produced by this build".into())
                }
            }),
        Err(error) => Err(error),
    };
    match result {
        Err(err) => {
            log.push_str(&diagnostics::explain(&err));
            BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: cap_build_log(log),
                image_digest: None,
            }
        }
        Ok(reference) => BuildOutcome {
            status: ChallengeBuildStatus::Success,
            log: cap_build_log(log),
            image_digest: Some(reference),
        },
    }
}

/// Pull the referenced image when there is no local build context, mirroring the
/// best-effort `create_image` pull in `DockerContainerManager::create`. A
/// successful pull is a successful "build" of a registry-shipped challenge.
pub(super) async fn pull_image(
    docker: &Docker,
    image: &str,
    portable_required: bool,
) -> BuildOutcome {
    let options = CreateImageOptions {
        from_image: image.to_string(),
        ..Default::default()
    };
    let mut stream = docker.create_image(Some(options), None, None);

    let mut log = String::new();
    let mut failed: Option<String> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(info) => {
                if let Some(s) = info.status {
                    log.push_str(&s);
                    log.push('\n');
                }
                if let Some(err) = info.error {
                    failed = Some(err);
                }
            }
            Err(e) => {
                failed = Some(format!("pull transport error: {e}"));
                break;
            }
        }
    }

    match failed {
        Some(err) => {
            log.push_str(&diagnostics::explain(&format!("Image {image}: {err}")));
            BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: cap_build_log(log),
                image_digest: None,
            }
        }
        None => match inspect_immutable_image(
            docker,
            image,
            ImageOperation::RegistryPull,
            portable_required,
        )
        .await
        {
            Ok(reference) => BuildOutcome {
                status: ChallengeBuildStatus::Success,
                log: cap_build_log(log),
                image_digest: Some(reference),
            },
            Err(error) => {
                log.push_str(&format!("\nimmutable image resolution failed: {error}"));
                BuildOutcome {
                    status: ChallengeBuildStatus::Failed,
                    log: cap_build_log(log),
                    image_digest: None,
                }
            }
        },
    }
}
