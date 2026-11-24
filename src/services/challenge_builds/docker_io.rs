//! Docker build and pull transport with bounded log capture.
use super::*;
use bollard::errors::Error;
use bollard::image::TagImageOptions;

// Bollard 0.17's generic build options always serialize the tag. Docker
// accepts an absent tag for an anonymous build but rejects an empty one.
#[derive(Default, Eq, Hash, PartialEq)]
struct BuildValue(String);

impl serde::Serialize for BuildValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.is_empty() {
            serializer.serialize_none()
        } else {
            serializer.serialize_str(&self.0)
        }
    }
}

impl From<BuildValue> for String {
    fn from(value: BuildValue) -> Self {
        value.0
    }
}

/// Repack a validated ZIP archive as Docker's tar build context. Reject opaque
/// legacy blobs before passing input to the privileged daemon.
pub(super) async fn build_from_context(
    docker: &Docker,
    image: &str,
    archive: Vec<u8>,
    context_subdir: Option<&str>,
) -> BuildOutcome {
    if image.is_empty()
        || image.contains('@')
        || image.contains("://")
        || crate::services::challenge_images::is_local_image_id(image)
    {
        return BuildOutcome {
            status: ChallengeBuildStatus::Failed,
            log: Some("A Docker archive build requires a mutable destination tag".into()),
            image_digest: None,
        };
    }
    // Resolve the implicit latest tag for inspect as well as publish. Otherwise
    // Docker may interpret a short hexadecimal repository name as an image ID.
    let destination = canonical_image_reference(Some(image));
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

    let previous = match current_tag(docker, &destination).await {
        Ok(previous) => previous,
        Err(error) => {
            return BuildOutcome {
                status: ChallengeBuildStatus::Failed,
                log: Some(error),
                image_digest: None,
            }
        }
    };
    let progress = build_candidate(docker, &destination, tar).await;
    finish_archive_build(docker, &destination, previous.as_deref(), progress).await
}

pub(super) async fn build_candidate(
    docker: &Docker,
    image: &str,
    tar: Vec<u8>,
) -> progress::BuildProgress {
    let labels = canonical_managed_image_tag(image)
        .map(|reference| {
            std::collections::HashMap::from([
                (
                    BuildValue(crate::services::container::IMAGE_SCOPE_LABEL.into()),
                    BuildValue(crate::services::container::docker_installation_scope()),
                ),
                (
                    BuildValue(crate::services::container::IMAGE_REFERENCE_LABEL.into()),
                    BuildValue(reference),
                ),
            ])
        })
        .unwrap_or_default();
    let options = BuildImageOptions::<BuildValue> {
        dockerfile: BuildValue("Dockerfile".into()),
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
    progress
}

async fn current_tag(docker: &Docker, name: &str) -> Result<Option<String>, String> {
    match docker.inspect_image(name).await {
        Ok(image) => image
            .id
            .filter(|id| crate::services::challenge_images::is_local_image_id(id))
            .map(Some)
            .ok_or_else(|| "Docker returned an invalid local image ID".into()),
        Err(Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(e) => Err(format!("Could not inspect build destination: {e}")),
    }
}

async fn publish_candidate(
    docker: &Docker,
    image: &str,
    previous: Option<&str>,
    built: &str,
) -> Result<String, String> {
    let candidate = docker
        .inspect_image(built)
        .await
        .map_err(|e| format!("Could not verify built image: {e}"))?;
    if candidate.id.as_deref() != Some(built) {
        return Err("Docker inspection disagrees with the build image ID".into());
    }
    if let Some(reference) = canonical_managed_image_tag(image) {
        crate::services::challenge_images::validate_image_ownership_labels(
            &candidate,
            &crate::services::container::docker_installation_scope(),
            &reference,
            true,
        )?;
    }
    let current = current_tag(docker, image).await?;
    if current.as_deref() == Some(built) {
        return Ok(built.to_owned());
    }
    if current.as_deref() != previous {
        return Err("Docker build destination changed while the build was running".into());
    }
    // The caller holds the destination's image lock across build and publish.
    // Use the verified ID as the source, including when recovering a lost tag response.
    let update = docker
        .tag_image(
            built,
            Some(TagImageOptions {
                repo: image,
                tag: "",
            }),
        )
        .await;
    let actual =
        inspect_immutable_image(docker, image, ImageOperation::ArchiveBuild, false).await?;
    if actual != built {
        return Err(update.err().map_or_else(
            || "Docker tag no longer refers to the image produced by this build".into(),
            |e| format!("Could not tag the verified build: {e}"),
        ));
    }
    Ok(actual)
}

pub(super) async fn finish_archive_build(
    docker: &Docker,
    image: &str,
    previous: Option<&str>,
    progress: progress::BuildProgress,
) -> BuildOutcome {
    let (mut log, result) = progress.into_parts();
    // A tag may already exist from the previous build. Inspect alone cannot
    // establish that this attempt produced it, even when the stream ended cleanly.
    let result = match result {
        Ok(built) => publish_candidate(docker, image, previous, &built).await,
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
