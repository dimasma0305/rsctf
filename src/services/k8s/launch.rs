use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{Capabilities, SeccompProfile, SecurityContext, Service};
use k8s_openapi::api::networking::v1::{NetworkPolicy, NetworkPolicySpec};

use super::orphans::APP_LABEL;
use crate::services::container::ContainerSpec;
use crate::utils::codec::random_hex;

/// Sanitize an image reference into an RFC1123-ish label fragment usable in a
/// resource name (lowercase alphanumerics + `-`, non-empty). Mirrors RSCTF's
/// `imageName.ToValidRFC1123String("chal")`.
pub(super) fn sanitize_image(image: &str) -> String {
    let last = image.rsplit('/').next().unwrap_or(image);
    let base = last.split(':').next().unwrap_or(last);
    let cleaned: String = base
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "chal".to_string()
    } else {
        // RFC1123 label cap is 63 chars; leave room for the "-<suffix>" tail.
        trimmed.chars().take(40).collect()
    }
}

pub(super) fn workload_name_and_uid(
    image: &str,
    scope: &str,
    operation_id: Option<&str>,
) -> (String, String) {
    if let Some(operation_id) = operation_id {
        let identity = format!("{scope}\0{operation_id}");
        let uid = crate::utils::codec::sha256_str(&identity)[..16].to_string();
        return (format!("rsctf-operation-{uid}"), uid);
    }
    let uid = random_hex(8);
    (format!("{}-{uid}", sanitize_image(image)), uid)
}

pub(super) fn service_type(internal_only: bool) -> &'static str {
    if internal_only {
        "ClusterIP"
    } else {
        "NodePort"
    }
}

pub(super) fn challenge_security_context() -> SecurityContext {
    let uid = std::env::var("RSCTF_K8S_CHALLENGE_UID")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(10_000);
    SecurityContext {
        allow_privilege_escalation: Some(false),
        capabilities: Some(Capabilities {
            // Challenge images commonly expose port 80. Preserve only the
            // narrow capability needed for a non-root process to bind it.
            add: Some(vec!["NET_BIND_SERVICE".to_string()]),
            drop: Some(vec!["ALL".to_string()]),
        }),
        privileged: Some(false),
        run_as_group: Some(uid),
        run_as_non_root: Some(true),
        run_as_user: Some(uid),
        seccomp_profile: Some(SeccompProfile {
            localhost_profile: None,
            type_: "RuntimeDefault".to_string(),
        }),
        ..Default::default()
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct KubernetesLaunchIdentity<'a> {
    revision: u8,
    portable_spec_fingerprint: &'a str,
    security_context: &'a SecurityContext,
    network_policy: Option<&'a NetworkPolicySpec>,
    ad_service_cidr: Option<&'a str>,
}

pub(super) fn kubernetes_launch_fingerprint(
    spec: &ContainerSpec,
    security_context: &SecurityContext,
    private_policy: Option<&NetworkPolicy>,
    ad_service_cidr: Option<&str>,
) -> String {
    let portable_spec_fingerprint = crate::services::container::launch_spec_fingerprint(spec);
    let canonical = KubernetesLaunchIdentity {
        // v2 also binds the A&D Service CIDR. A deny-by-default policy does not
        // render that CIDR, but it still determines whether the assigned
        // ClusterIP is routable and therefore belongs in the launch identity.
        revision: 2,
        portable_spec_fingerprint: &portable_spec_fingerprint,
        security_context,
        network_policy: private_policy.and_then(|policy| policy.spec.as_ref()),
        ad_service_cidr,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("the fixed Kubernetes launch identity is always JSON serializable");
    crate::utils::codec::sha256_hex(&bytes)
}

pub(super) fn fingerprint_policy_labels() -> BTreeMap<String, String> {
    BTreeMap::from([(APP_LABEL.to_string(), "rsctf-fingerprint".to_string())])
}

pub(super) fn stamp_policy_labels(policy: &mut NetworkPolicy, labels: &BTreeMap<String, String>) {
    policy.metadata.labels = Some(labels.clone());
    if let Some(spec) = policy.spec.as_mut() {
        spec.pod_selector.match_labels = Some(labels.clone());
    }
}

pub(super) fn service_owner_matches_pod(service: &Service, pod_name: &str, pod_uid: &str) -> bool {
    service
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner.api_version == "v1"
                    && owner.kind == "Pod"
                    && owner.name == pod_name
                    && owner.uid == pod_uid
            })
        })
}
