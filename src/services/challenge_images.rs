//! Immutable challenge-image references shared by every provisioning path.
//!
//! `container_image` is organizer configuration and may contain a mutable tag.
//! Runtime workloads must instead use the exact reference recorded by a
//! successful build/pull in `build_image_digest`.

use crate::app_state::SharedState;
use crate::models::data::game_challenge;
use crate::models::internal::configs::RuntimeRole;
use crate::services::container::ContainerBackendKind;
use crate::utils::enums::ChallengeBuildStatus;
use crate::utils::error::{AppError, AppResult};
use rsctf_worker_protocol::is_valid_registry_repository;
use uuid::Uuid;

fn valid_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// A daemon-local immutable image id (`sha256:…`). It is safe only when the
/// builder and every runtime owner genuinely address the same Docker daemon.
pub(crate) fn is_local_image_id(reference: &str) -> bool {
    valid_sha256(reference.trim())
}

/// A portable OCI/Docker repository digest (`registry/name@sha256:…`).
pub(crate) fn is_repository_digest(reference: &str) -> bool {
    let reference = reference.trim();
    let Some((repository, digest)) = reference.rsplit_once('@') else {
        return false;
    };
    is_valid_registry_repository(repository) && valid_sha256(digest)
}

/// An immutable daemon-local image explicitly scoped to one enrolled worker.
/// This lets a private Linux Docker host run locally-built images without a
/// registry while preventing the scheduler from placing the id elsewhere.
/// Wire/config shape: `worker://<uuid>/sha256:<64 hex>`.
pub(crate) fn worker_local_image(reference: &str) -> Option<(Uuid, &str)> {
    let rest = reference.trim().strip_prefix("worker://")?;
    let (worker, image) = rest.split_once('/')?;
    let worker = Uuid::parse_str(worker).ok()?;
    valid_sha256(image).then_some((worker, image))
}

pub(crate) fn shared_docker_daemon_acknowledged() -> bool {
    std::env::var("RSCTF_SHARED_DOCKER_DAEMON")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

/// Validate an already-pinned image against the current runtime topology.
/// Repository digests are portable. Bare image ids are deliberately limited to
/// the historical all-in-one Docker deployment or an explicitly acknowledged
/// shared-daemon split deployment.
pub(crate) fn validate_runtime_reference(
    reference: &str,
    backend: ContainerBackendKind,
    role: RuntimeRole,
    shared_docker_daemon: bool,
) -> AppResult<String> {
    let reference = reference.trim();
    if is_repository_digest(reference) {
        return Ok(reference.to_string());
    }
    if worker_local_image(reference).is_some() && backend == ContainerBackendKind::Worker {
        return Ok(reference.to_string());
    }
    if worker_local_image(reference).is_some() {
        return Err(AppError::bad_request(
            "A worker-scoped image can only run with RSCTF_CONTAINER_BACKEND=worker.",
        ));
    }
    if is_local_image_id(reference)
        && backend == ContainerBackendKind::Docker
        && (role == RuntimeRole::All || shared_docker_daemon)
    {
        return Ok(reference.to_string());
    }
    if is_local_image_id(reference) {
        return Err(AppError::bad_request(
            "The challenge image is pinned to one Docker daemon. This topology requires a portable registry digest; rebuild from a registry image.",
        ));
    }
    Err(AppError::bad_request(
        "The challenge image has no valid immutable digest; rebuild it before provisioning.",
    ))
}

/// Resolve the only image reference a challenge workload may execute.
pub(crate) fn runtime_image_from_build_fields(
    st: &SharedState,
    build_status: i16,
    build_image_digest: Option<&str>,
) -> AppResult<String> {
    if build_status != ChallengeBuildStatus::Success as i16 {
        return Err(AppError::bad_request(
            "The challenge image has not completed a successful immutable build/pull.",
        ));
    }
    let reference = build_image_digest
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::bad_request(
                "The successful legacy build has no immutable image digest; rebuild it before provisioning.",
            )
        })?;
    validate_runtime_reference(
        reference,
        st.containers.backend_kind(),
        st.config.runtime_role,
        shared_docker_daemon_acknowledged(),
    )
}

/// Resolve the only image reference a challenge workload may execute.
pub(crate) fn runtime_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> AppResult<String> {
    runtime_image_from_build_fields(
        st,
        challenge.build_status as i16,
        challenge.build_image_digest.as_deref(),
    )
}

/// Resolve a legacy single-container image that will be scheduled on the
/// trusted worker plane. Hybrid deployments deliberately use worker semantics
/// here even though their local A&D/KotH backend remains Docker/Kubernetes.
pub(crate) fn runtime_worker_image(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> AppResult<String> {
    if !st.containers.supports_worker_workloads() {
        return runtime_image(st, challenge);
    }
    if challenge.build_status != ChallengeBuildStatus::Success {
        return Err(AppError::bad_request(
            "The challenge image has not completed a successful immutable build/pull.",
        ));
    }
    let reference = challenge
        .build_image_digest
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::bad_request(
                "The successful legacy build has no immutable image digest; rebuild it before provisioning.",
            )
        })?;
    validate_runtime_reference(
        reference,
        ContainerBackendKind::Worker,
        st.config.runtime_role,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REPO: &str =
        "registry.example/team/service@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const WORKER: &str = "worker://018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb/sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn immutable_reference_shapes_are_strict() {
        assert!(is_local_image_id(ID));
        assert!(is_repository_digest(REPO));
        assert!(!is_repository_digest("service:latest"));
        assert!(!is_repository_digest("service@sha256:short"));
        assert!(!is_repository_digest(&format!(
            "service@forged@sha256:{}",
            "a".repeat(64)
        )));
        assert!(!is_repository_digest(&format!(
            "service\0forged@sha256:{}",
            "a".repeat(64)
        )));
        assert!(!is_repository_digest(&format!(
            "service:latest@sha256:{}",
            "a".repeat(64)
        )));
        assert!(!is_local_image_id("sha256:not-hex"));
        assert!(worker_local_image(WORKER).is_some());
        assert!(worker_local_image("worker://invalid/sha256:short").is_none());
    }

    #[test]
    fn local_ids_are_rejected_outside_one_docker_daemon() {
        assert!(validate_runtime_reference(
            ID,
            ContainerBackendKind::Docker,
            RuntimeRole::All,
            false,
        )
        .is_ok());
        assert!(validate_runtime_reference(
            ID,
            ContainerBackendKind::Docker,
            RuntimeRole::Control,
            true,
        )
        .is_ok());
        assert!(validate_runtime_reference(
            ID,
            ContainerBackendKind::Docker,
            RuntimeRole::Control,
            false,
        )
        .is_err());
        assert!(validate_runtime_reference(
            ID,
            ContainerBackendKind::Kubernetes,
            RuntimeRole::Control,
            true,
        )
        .is_err());
        assert!(validate_runtime_reference(
            REPO,
            ContainerBackendKind::Kubernetes,
            RuntimeRole::Control,
            false,
        )
        .is_ok());
        assert!(validate_runtime_reference(
            WORKER,
            ContainerBackendKind::Worker,
            RuntimeRole::Web,
            false,
        )
        .is_ok());
        assert!(validate_runtime_reference(
            WORKER,
            ContainerBackendKind::Docker,
            RuntimeRole::All,
            false,
        )
        .is_err());
    }
}
