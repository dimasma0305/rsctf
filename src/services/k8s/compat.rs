use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{Container, ContainerPort, Pod, Service, ServicePort};
use k8s_openapi::api::networking::v1::{NetworkPolicy, NetworkPolicySpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

const LAUNCH_SPEC_LABEL: &str = "rsctf.launch-spec";

fn without_launch_label(
    labels: Option<&BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    labels.map(|labels| {
        let mut labels = labels.clone();
        labels.remove(LAUNCH_SPEC_LABEL);
        labels
    })
}

fn legacy_metadata_matches(actual: &ObjectMeta, expected: &ObjectMeta) -> bool {
    actual.name == expected.name
        && actual.namespace == expected.namespace
        && actual.labels == without_launch_label(expected.labels.as_ref())
}

fn optional_vec_matches<T: PartialEq>(actual: Option<&Vec<T>>, expected: Option<&Vec<T>>) -> bool {
    actual.map(Vec::as_slice).unwrap_or_default() == expected.map(Vec::as_slice).unwrap_or_default()
}

fn optional_false(value: Option<bool>) -> bool {
    !value.unwrap_or(false)
}

fn pod_security_context_matches(
    actual: Option<&k8s_openapi::api::core::v1::PodSecurityContext>,
    expected: Option<&k8s_openapi::api::core::v1::PodSecurityContext>,
) -> bool {
    actual == expected
        || (expected.is_none()
            && actual.is_some_and(|context| {
                context == &k8s_openapi::api::core::v1::PodSecurityContext::default()
            }))
}

fn container_port_matches(actual: &ContainerPort, expected: &ContainerPort) -> bool {
    actual.name == expected.name
        && actual.container_port == expected.container_port
        && actual.host_ip == expected.host_ip
        && actual.host_port == expected.host_port
        && actual.protocol.as_deref().unwrap_or("TCP")
            == expected.protocol.as_deref().unwrap_or("TCP")
}

fn container_ports_match(actual: &Container, expected: &Container) -> bool {
    let actual = actual.ports.as_deref().unwrap_or_default();
    let expected = expected.ports.as_deref().unwrap_or_default();
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| container_port_matches(actual, expected))
}

fn legacy_container_matches(actual: &Container, expected: &Container) -> bool {
    actual.name == expected.name
        && actual.image == expected.image
        && optional_vec_matches(actual.command.as_ref(), expected.command.as_ref())
        && optional_vec_matches(actual.args.as_ref(), expected.args.as_ref())
        && optional_vec_matches(actual.env.as_ref(), expected.env.as_ref())
        && optional_vec_matches(actual.env_from.as_ref(), expected.env_from.as_ref())
        && container_ports_match(actual, expected)
        && actual.resources == expected.resources
        && actual.security_context == expected.security_context
        && optional_vec_matches(
            actual.volume_mounts.as_ref(),
            expected.volume_mounts.as_ref(),
        )
        && optional_vec_matches(
            actual.volume_devices.as_ref(),
            expected.volume_devices.as_ref(),
        )
        && actual.working_dir == expected.working_dir
        && actual.lifecycle == expected.lifecycle
        && actual.liveness_probe == expected.liveness_probe
        && actual.readiness_probe == expected.readiness_probe
        && actual.startup_probe == expected.startup_probe
        && optional_false(actual.stdin)
        && optional_false(actual.stdin_once)
        && optional_false(actual.tty)
}

pub(super) fn legacy_pod_matches(actual: &Pod, expected: &Pod) -> bool {
    let (Some(actual_spec), Some(expected_spec)) = (actual.spec.as_ref(), expected.spec.as_ref())
    else {
        return false;
    };
    legacy_metadata_matches(&actual.metadata, &expected.metadata)
        && actual_spec.containers.len() == 1
        && expected_spec.containers.len() == 1
        && legacy_container_matches(&actual_spec.containers[0], &expected_spec.containers[0])
        && actual_spec.restart_policy == expected_spec.restart_policy
        && actual_spec.automount_service_account_token
            == expected_spec.automount_service_account_token
        && optional_vec_matches(actual_spec.init_containers.as_ref(), None)
        && optional_vec_matches(actual_spec.ephemeral_containers.as_ref(), None)
        && optional_vec_matches(actual_spec.volumes.as_ref(), None)
        && optional_vec_matches(actual_spec.image_pull_secrets.as_ref(), None)
        && optional_false(actual_spec.host_network)
        && optional_false(actual_spec.host_pid)
        && optional_false(actual_spec.host_ipc)
        && optional_false(actual_spec.share_process_namespace)
        && pod_security_context_matches(
            actual_spec.security_context.as_ref(),
            expected_spec.security_context.as_ref(),
        )
        && actual_spec.runtime_class_name == expected_spec.runtime_class_name
}

fn service_port_matches(actual: &ServicePort, expected: &ServicePort) -> bool {
    actual.name == expected.name
        && actual.port == expected.port
        && actual.target_port == expected.target_port
        && actual.app_protocol == expected.app_protocol
        && actual.protocol.as_deref().unwrap_or("TCP")
            == expected.protocol.as_deref().unwrap_or("TCP")
}

pub(super) fn legacy_service_matches(actual: &Service, expected: &Service) -> bool {
    let (Some(actual_spec), Some(expected_spec)) = (actual.spec.as_ref(), expected.spec.as_ref())
    else {
        return false;
    };
    let ports_match = actual_spec
        .ports
        .as_ref()
        .zip(expected_spec.ports.as_ref())
        .is_some_and(|(actual, expected)| {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| service_port_matches(actual, expected))
        });
    legacy_metadata_matches(&actual.metadata, &expected.metadata)
        && actual_spec.type_ == expected_spec.type_
        && actual_spec.selector == expected_spec.selector
        && ports_match
        && actual_spec
            .external_traffic_policy
            .as_deref()
            .unwrap_or("Cluster")
            == expected_spec
                .external_traffic_policy
                .as_deref()
                .unwrap_or("Cluster")
        && optional_vec_matches(actual_spec.external_ips.as_ref(), None)
        && optional_vec_matches(actual_spec.load_balancer_source_ranges.as_ref(), None)
        && actual_spec.load_balancer_class == expected_spec.load_balancer_class
}

fn network_policy_spec_matches(actual: &NetworkPolicySpec, expected: &NetworkPolicySpec) -> bool {
    actual.pod_selector == expected.pod_selector
        && optional_vec_matches(actual.ingress.as_ref(), expected.ingress.as_ref())
        && optional_vec_matches(actual.egress.as_ref(), expected.egress.as_ref())
        && optional_vec_matches(actual.policy_types.as_ref(), expected.policy_types.as_ref())
}

pub(super) fn legacy_policy_matches(actual: &NetworkPolicy, expected: &NetworkPolicy) -> bool {
    let mut expected = expected.clone();
    expected.metadata.labels = without_launch_label(expected.metadata.labels.as_ref());
    if let Some(labels) = expected
        .spec
        .as_mut()
        .and_then(|spec| spec.pod_selector.match_labels.as_mut())
    {
        labels.remove(LAUNCH_SPEC_LABEL);
    }
    let specs_match = actual
        .spec
        .as_ref()
        .zip(expected.spec.as_ref())
        .is_some_and(|(actual, expected)| network_policy_spec_matches(actual, expected));
    legacy_metadata_matches(&actual.metadata, &expected.metadata) && specs_match
}
