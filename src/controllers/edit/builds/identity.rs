//! Docker image-name normalization and immutable result selection.

use bollard::Docker;

use crate::models::data::game_challenge;

pub(crate) fn canonical_image_reference(image: Option<&str>) -> String {
    let Some(image) = image.map(str::trim).filter(|image| !image.is_empty()) else {
        return "<none>".to_string();
    };

    let (name, suffix) = if let Some((name, digest)) = image.split_once('@') {
        (name, format!("@{digest}"))
    } else {
        let last_slash = image.rfind('/');
        let tag = image
            .rfind(':')
            .filter(|colon| last_slash.map(|slash| *colon > slash).unwrap_or(true));
        match tag {
            Some(tag) => (&image[..tag], image[tag..].to_string()),
            None => (image, ":latest".to_string()),
        }
    };

    let mut parts: Vec<&str> = name.split('/').collect();
    let has_registry = parts.first().is_some_and(|first| {
        first.contains('.') || first.contains(':') || first.eq_ignore_ascii_case("localhost")
    });
    if !has_registry {
        parts.insert(0, "docker.io");
    } else if parts
        .first()
        .is_some_and(|first| first.eq_ignore_ascii_case("index.docker.io"))
    {
        parts[0] = "docker.io";
    }
    if parts.first() == Some(&"docker.io") && parts.len() == 2 {
        parts.insert(1, "library");
    }
    format!("{}{suffix}", parts.join("/"))
}

pub(crate) fn image_build_lock_key(image: Option<&str>) -> String {
    format!("challenge-build:image:{}", canonical_image_reference(image))
}

pub(super) fn build_lock_key(challenge: &game_challenge::Model) -> String {
    image_build_lock_key(challenge.container_image.as_deref())
}

/// Canonical mutable tag managed by the local rsctf build pipeline. Registry
/// digests, worker references, and daemon IDs are immutable runtime identities,
/// not local tags that the admin image GC may remove.
pub(crate) fn canonical_managed_image_tag(image: &str) -> Option<String> {
    let image = image.trim();
    if crate::services::challenge_images::is_local_image_id(image)
        || crate::services::challenge_images::is_repository_digest(image)
        || crate::services::challenge_images::worker_local_image(image).is_some()
    {
        return None;
    }
    let canonical = canonical_image_reference(Some(image));
    canonical
        .starts_with("docker.io/rsctf/")
        .then_some(canonical)
}

fn canonical_image_repository(image: &str) -> Option<String> {
    if crate::services::challenge_images::is_local_image_id(image) {
        return None;
    }
    let canonical = canonical_image_reference(Some(image));
    if canonical == "<none>" {
        return None;
    }
    if let Some((repository, _)) = canonical.split_once('@') {
        return Some(repository.to_string());
    }
    let last_slash = canonical.rfind('/');
    let tag = canonical
        .rfind(':')
        .filter(|colon| last_slash.map(|slash| *colon > slash).unwrap_or(true));
    Some(
        tag.map_or(canonical.as_str(), |index| &canonical[..index])
            .to_string(),
    )
}

#[derive(Clone, Copy)]
pub(super) enum ImageOperation {
    ArchiveBuild,
    RegistryPull,
}

pub(super) fn immutable_image_reference(
    requested: &str,
    inspected: &bollard::models::ImageInspect,
    operation: ImageOperation,
    portable_required: bool,
) -> Result<String, String> {
    if matches!(operation, ImageOperation::RegistryPull)
        && crate::services::challenge_images::is_repository_digest(requested)
    {
        return Ok(requested.trim().to_string());
    }

    // A pulled tag may share an image id with several repositories. Select only
    // the digest for the requested repository so provenance cannot silently
    // switch to an unrelated alias returned by Docker inspect.
    if matches!(operation, ImageOperation::RegistryPull) {
        let requested_repository = canonical_image_repository(requested);
        if let Some(reference) =
            inspected
                .repo_digests
                .as_ref()
                .into_iter()
                .flatten()
                .find(|reference| {
                    crate::services::challenge_images::is_repository_digest(reference)
                        && canonical_image_repository(reference)
                            .as_ref()
                            .zip(requested_repository.as_ref())
                            .is_some_and(|(actual, requested)| actual == requested)
                })
        {
            return Ok(reference.clone());
        }
    }

    if !portable_required {
        if let Some(id) = inspected
            .id
            .as_deref()
            .filter(|id| crate::services::challenge_images::is_local_image_id(id))
        {
            return Ok(id.to_string());
        }
    }

    if portable_required {
        Err("Docker did not report a portable digest for the requested repository; this multi-node topology refuses a daemon-local image id".to_string())
    } else {
        Err("Docker did not report a valid immutable image id".to_string())
    }
}

pub(super) async fn inspect_immutable_image(
    docker: &Docker,
    requested: &str,
    operation: ImageOperation,
    portable_required: bool,
) -> Result<String, String> {
    let inspected = docker
        .inspect_image(requested)
        .await
        .map_err(|error| format!("image inspect failed after the operation: {error}"))?;
    immutable_image_reference(requested, &inspected, operation, portable_required)
}
