use bollard::container::{ListContainersOptions, RemoveContainerOptions};
use bollard::models::{ContainerInspectResponse, ContainerStateStatusEnum};
use bollard::Docker;
use rsctf_worker_protocol::GameKind;

use super::{restricted_profile_matches, storage_quota_policy_matches, LAUNCH_SPEC_LABEL};
#[cfg(test)]
use super::{STORAGE_QUOTA_FALLBACK, STORAGE_QUOTA_LABEL};
use crate::services::container::{
    labels_match_scope, ContainerSpec, MANAGED_LABEL, OPERATION_LABEL, SCOPE_LABEL,
};
use crate::utils::error::{AppError, AppResult};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerLaunchSpec<'a> {
    revision: u8,
    game_kind: GameKind,
    image: &'a str,
    memory_limit: i32,
    cpu_count: i32,
    storage_limit: i32,
    expose_port: i32,
    #[serde(skip_serializing_if = "is_true")]
    publish_port: bool,
    #[serde(skip_serializing_if = "is_false")]
    proxy_only: bool,
    env: &'a [(String, String)],
    flag: Option<&'a str>,
    ad_network: Option<&'a str>,
    allow_egress: bool,
    control_plane_callback_ports: &'a [i32],
    network_mode: crate::utils::enums::NetworkMode,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DockerLaunchSpecV4<'a> {
    revision: u8,
    game_kind: GameKind,
    image: &'a str,
    memory_limit: i32,
    cpu_count: i32,
    storage_limit: i32,
    expose_port: i32,
    #[serde(skip_serializing_if = "is_true")]
    publish_port: bool,
    #[serde(skip_serializing_if = "is_false")]
    proxy_only: bool,
    env: &'a [(String, String)],
    flag: Option<&'a str>,
    ad_network: Option<&'a str>,
    allow_egress: bool,
    network_mode: crate::utils::enums::NetworkMode,
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Hash every launch-affecting caller input into a non-secret identity label.
/// Operation and installation identities have their own labels and deliberately
/// do not affect whether a crash retry represents the same workload.
pub(crate) fn launch_spec_fingerprint(spec: &ContainerSpec) -> String {
    let canonical = DockerLaunchSpec {
        // v5 adds control-plane callback policy ports. Older workloads must
        // never be adopted because they do not prove the same egress policy.
        revision: 5,
        game_kind: spec.game_kind,
        image: &spec.image,
        memory_limit: spec.memory_limit,
        cpu_count: spec.cpu_count,
        storage_limit: spec.storage_limit,
        expose_port: spec.expose_port,
        publish_port: spec.publish_port,
        proxy_only: spec.proxy_only,
        env: &spec.env,
        flag: spec.flag.as_deref(),
        ad_network: spec.ad_network.as_deref(),
        allow_egress: spec.allow_egress,
        control_plane_callback_ports: &spec.control_plane_callback_ports,
        network_mode: spec.network_mode,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("the fixed container launch identity is always JSON serializable");
    crate::utils::codec::sha256_hex(&bytes)
}

pub(in crate::services::container) fn launch_spec_matches(
    info: &ContainerInspectResponse,
    expected_fingerprint: &str,
) -> bool {
    info.config
        .as_ref()
        .and_then(|config| config.labels.as_ref())
        .and_then(|labels| labels.get(LAUNCH_SPEC_LABEL))
        .map(String::as_str)
        == Some(expected_fingerprint)
}

fn legacy_v4_launch_spec_matches(info: &ContainerInspectResponse, spec: &ContainerSpec) -> bool {
    if !spec.control_plane_callback_ports.is_empty() {
        return false;
    }
    let canonical = DockerLaunchSpecV4 {
        revision: 4,
        game_kind: spec.game_kind,
        image: &spec.image,
        memory_limit: spec.memory_limit,
        cpu_count: spec.cpu_count,
        storage_limit: spec.storage_limit,
        expose_port: spec.expose_port,
        publish_port: spec.publish_port,
        proxy_only: spec.proxy_only,
        env: &spec.env,
        flag: spec.flag.as_deref(),
        ad_network: spec.ad_network.as_deref(),
        allow_egress: spec.allow_egress,
        network_mode: spec.network_mode,
    };
    let fingerprint = crate::utils::codec::sha256_hex(
        &serde_json::to_vec(&canonical).expect("the legacy launch identity serializes"),
    );
    launch_spec_matches(info, &fingerprint)
}

fn operation_container_filters(
    scope: &str,
    operation_id: &str,
) -> std::collections::HashMap<String, Vec<String>> {
    std::collections::HashMap::from([(
        "label".to_string(),
        vec![
            format!("{MANAGED_LABEL}={scope}"),
            format!("{SCOPE_LABEL}={scope}"),
            format!("{OPERATION_LABEL}={operation_id}"),
        ],
    )])
}

fn validate_operation_container(
    existing: &ContainerInspectResponse,
    scope: &str,
    spec: &ContainerSpec,
    launch_fingerprint: &str,
    restricted_profile: bool,
    storage_quota_enforced: bool,
) -> AppResult<String> {
    let labels = existing
        .config
        .as_ref()
        .and_then(|config| config.labels.as_ref());
    let actual_operation = labels
        .and_then(|labels| labels.get(OPERATION_LABEL))
        .map(String::as_str);
    let actual_image = existing
        .config
        .as_ref()
        .and_then(|config| config.image.as_deref());
    if !labels_match_scope(labels, scope)
        || actual_operation != spec.operation_id.as_deref()
        || actual_image != Some(spec.image.as_str())
        || !(launch_spec_matches(existing, launch_fingerprint)
            || legacy_v4_launch_spec_matches(existing, spec))
        || !restricted_profile_matches(existing, restricted_profile)
        || !storage_quota_policy_matches(existing, storage_quota_enforced)
    {
        return Err(AppError::conflict(
            "container operation identity is owned by a different workload",
        ));
    }
    existing
        .id
        .clone()
        .ok_or_else(|| AppError::internal("adopted container has no backend identity"))
}

fn operation_container_is_terminal(existing: &ContainerInspectResponse) -> bool {
    matches!(
        existing.state.as_ref().and_then(|state| state.status),
        Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD)
    )
}

pub(in crate::services::container) async fn adopt_operation_container(
    docker: &Docker,
    existing: &ContainerInspectResponse,
    scope: &str,
    spec: &ContainerSpec,
    launch_fingerprint: &str,
    restricted_profile: bool,
    storage_quota_enforced: bool,
) -> AppResult<String> {
    let id = validate_operation_container(
        existing,
        scope,
        spec,
        launch_fingerprint,
        restricted_profile,
        storage_quota_enforced,
    )?;
    if !operation_container_is_terminal(existing) {
        return Ok(id);
    }

    match docker
        .remove_container(
            &id,
            Some(RemoveContainerOptions {
                v: false,
                force: true,
                link: false,
            }),
        )
        .await
    {
        Ok(())
        | Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Err(AppError::conflict(
            "Docker retry adopted a terminal container; retry with a new operation identity",
        )),
        Err(error) => Err(AppError::internal(format!(
            "Docker retry adopted a terminal container and cleanup failed: {error}"
        ))),
    }
}

/// Find a response-lost workload by immutable operation labels rather than by
/// name. Older replicas used an image-prefixed name, so name-only conflict
/// handling can otherwise duplicate a workload during a rolling deployment.
pub(in crate::services::container) async fn discover_operation_container(
    docker: &Docker,
    scope: &str,
    spec: &ContainerSpec,
    launch_fingerprint: &str,
    restricted_profile: bool,
    storage_quota_enforced: bool,
) -> AppResult<Option<String>> {
    let Some(operation_id) = spec.operation_id.as_deref() else {
        return Ok(None);
    };
    let candidates = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            limit: Some(2),
            filters: operation_container_filters(scope, operation_id),
            ..Default::default()
        }))
        .await
        .map_err(|error| {
            AppError::internal(format!(
                "failed to discover an existing container operation: {error}"
            ))
        })?;
    if candidates.len() > 1 {
        return Err(AppError::conflict(
            "multiple containers claim the same operation identity",
        ));
    }
    let Some(id) = candidates
        .into_iter()
        .next()
        .and_then(|container| container.id)
    else {
        return Ok(None);
    };
    let existing = docker.inspect_container(&id, None).await.map_err(|error| {
        AppError::internal(format!(
            "container operation {id} was discovered but could not be inspected: {error}"
        ))
    })?;
    adopt_operation_container(
        docker,
        &existing,
        scope,
        spec,
        launch_fingerprint,
        restricted_profile,
        storage_quota_enforced,
    )
    .await
    .map(Some)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::services::container) enum FailedStartAction {
    TreatAsStarted,
    RetainForRetry,
    RemoveOwned,
}

/// Reconcile a failed Docker start without racing an idempotent adopter. A
/// stable CREATED container is retained because another replica may be starting
/// it concurrently; a terminal container is removed before the key is rotated.
pub(in crate::services::container) fn failed_start_action(
    stable_operation: bool,
    inspected: Option<&ContainerInspectResponse>,
) -> FailedStartAction {
    let status = inspected
        .and_then(|info| info.state.as_ref())
        .and_then(|state| state.status);
    match status {
        Some(ContainerStateStatusEnum::RUNNING) => FailedStartAction::TreatAsStarted,
        Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD) => {
            FailedStartAction::RemoveOwned
        }
        Some(ContainerStateStatusEnum::CREATED) if !stable_operation => {
            FailedStartAction::RemoveOwned
        }
        _ => FailedStartAction::RetainForRetry,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use bollard::models::{ContainerConfig, ContainerInspectResponse};

    use super::*;
    use crate::services::container::DEFAULT_CONTAINER_STORAGE_MB;

    #[test]
    fn previous_release_fingerprint_is_accepted_only_without_callback_ports() {
        let mut spec = ContainerSpec {
            game_kind: GameKind::Jeopardy,
            image: format!("registry.example/challenge@sha256:{}", "a".repeat(64)),
            memory_limit: 64,
            cpu_count: 1,
            storage_limit: DEFAULT_CONTAINER_STORAGE_MB,
            expose_port: 8080,
            publish_port: false,
            proxy_only: false,
            env: Vec::new(),
            flag: Some("flag{legacy}".to_string()),
            ad_network: None,
            allow_egress: false,
            control_plane_callback_ports: Vec::new(),
            network_mode: crate::utils::enums::NetworkMode::Isolated,
            operation_id: Some("legacy-operation".to_string()),
        };
        let legacy = DockerLaunchSpecV4 {
            revision: 4,
            game_kind: spec.game_kind,
            image: &spec.image,
            memory_limit: spec.memory_limit,
            cpu_count: spec.cpu_count,
            storage_limit: spec.storage_limit,
            expose_port: spec.expose_port,
            publish_port: spec.publish_port,
            proxy_only: spec.proxy_only,
            env: &spec.env,
            flag: spec.flag.as_deref(),
            ad_network: spec.ad_network.as_deref(),
            allow_egress: spec.allow_egress,
            network_mode: spec.network_mode,
        };
        let legacy_fingerprint = crate::utils::codec::sha256_hex(
            &serde_json::to_vec(&legacy).expect("legacy launch spec serializes"),
        );
        let scope = "installation-scope";
        let labels = HashMap::from([
            (MANAGED_LABEL.to_string(), scope.to_string()),
            (SCOPE_LABEL.to_string(), scope.to_string()),
            (OPERATION_LABEL.to_string(), "legacy-operation".to_string()),
            (LAUNCH_SPEC_LABEL.to_string(), legacy_fingerprint),
            (
                STORAGE_QUOTA_LABEL.to_string(),
                STORAGE_QUOTA_FALLBACK.to_string(),
            ),
        ]);
        let existing = ContainerInspectResponse {
            id: Some("legacy-container".to_string()),
            config: Some(ContainerConfig {
                image: Some(spec.image.clone()),
                labels: Some(labels),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            validate_operation_container(
                &existing,
                scope,
                &spec,
                &launch_spec_fingerprint(&spec),
                false,
                false,
            )
            .unwrap(),
            "legacy-container"
        );

        spec.control_plane_callback_ports.push(8080);
        assert!(matches!(
            validate_operation_container(
                &existing,
                scope,
                &spec,
                &launch_spec_fingerprint(&spec),
                false,
                false,
            ),
            Err(AppError::Conflict(_))
        ));
    }
}
