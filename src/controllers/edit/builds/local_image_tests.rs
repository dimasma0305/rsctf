use super::*;
use crate::models::internal::configs::RuntimeRole;
use crate::services::container::ContainerBackendKind;

#[test]
fn explicit_local_image_adoption_requires_one_shared_docker_daemon() {
    let image = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert!(validate_local_image_adoption(
        image,
        RuntimeRole::All,
        ContainerBackendKind::Docker,
        false,
    )
    .is_ok());
    assert!(validate_local_image_adoption(
        image,
        RuntimeRole::Web,
        ContainerBackendKind::Docker,
        true,
    )
    .is_ok());
    assert!(validate_local_image_adoption(
        image,
        RuntimeRole::Web,
        ContainerBackendKind::Docker,
        false,
    )
    .is_err());
    assert!(validate_local_image_adoption(
        image,
        RuntimeRole::Control,
        ContainerBackendKind::Kubernetes,
        true,
    )
    .is_err());
    assert!(validate_local_image_adoption(
        image,
        RuntimeRole::All,
        ContainerBackendKind::Worker,
        false,
    )
    .is_err());
}

#[test]
fn build_log_caps_advance_past_multibyte_utf8_boundaries() {
    let log = format!("■{}", "x".repeat(16 * 1024 - 2));
    let capped = cap_build_log(log).expect("non-empty build log is retained");

    assert!(capped.starts_with("…(truncated)…\n"));
    assert!(capped.ends_with('x'));
    assert!(!capped.contains('■'));
}
