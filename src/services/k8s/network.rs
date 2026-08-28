use std::collections::BTreeMap;
use std::net::IpAddr;

use ipnet::IpNet;
use k8s_openapi::api::networking::v1::{
    IPBlock, NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyIngressRule, NetworkPolicyPeer,
    NetworkPolicyPort, NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;

use crate::utils::error::{AppError, AppResult};

const AD_INGRESS_CIDRS_ENV: &str = "RSCTF_K8S_AD_INGRESS_CIDRS";
const CONTROL_NAMESPACE_ENV: &str = "RSCTF_K8S_CONTROL_NAMESPACE";
const CONTROL_POD_LABEL_ENV: &str = "RSCTF_K8S_CONTROL_POD_LABEL";
const KOTH_REPORTER_POD_SELECTOR_ENV: &str = "RSCTF_K8S_KOTH_REPORTER_POD_SELECTOR";
const POLICY_ENFORCED_ENV: &str = "RSCTF_K8S_NETWORK_POLICY_ENFORCED";
const ISOLATED_INGRESS_CIDRS_ENV: &str = "RSCTF_K8S_ISOLATED_INGRESS_CIDRS";
const POD_CIDRS_ENV: &str = "RSCTF_K8S_POD_CIDRS";
const KOTH_REPORTER_REQUIRED_LABELS: [&str; 3] = [
    "app.kubernetes.io/name",
    "app.kubernetes.io/instance",
    "app.kubernetes.io/component",
];

#[derive(Clone)]
pub(super) struct AdNetworkConfig {
    pub(super) service_cidr: IpNet,
    pub(super) ingress_cidrs: Vec<IpNet>,
    pub(super) control_namespace: Option<String>,
    pub(super) control_pod_label: (String, String),
    pub(super) reporter_pod_selector: Option<BTreeMap<String, String>>,
}

pub(super) fn validate_policy_enforcement_acknowledgement() -> AppResult<()> {
    let configured = std::env::var(POLICY_ENFORCED_ENV).ok();
    if policy_enforcement_acknowledged(configured.as_deref()) {
        Ok(())
    } else {
        Err(AppError::internal(format!(
            "Kubernetes challenge isolation requires {POLICY_ENFORCED_ENV}=true after verifying that the cluster CNI enforces networking.k8s.io/v1 NetworkPolicy"
        )))
    }
}

pub(super) fn policy_enforcement_acknowledged(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

pub(super) fn configured_control_namespace() -> Option<String> {
    std::env::var(CONTROL_NAMESPACE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

fn configured_control_pod_label() -> AppResult<(String, String)> {
    let label = std::env::var(CONTROL_POD_LABEL_ENV)
        .unwrap_or_else(|_| "app.kubernetes.io/name=rsctf".to_string());
    let (key, value) = label.split_once('=').ok_or_else(|| {
        AppError::internal(format!("{CONTROL_POD_LABEL_ENV} must use key=value syntax"))
    })?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() || value.is_empty() {
        return Err(AppError::internal(format!(
            "{CONTROL_POD_LABEL_ENV} must use non-empty key=value syntax"
        )));
    }
    Ok((key.to_string(), value.to_string()))
}

fn valid_label_segment(value: &str, maximum: usize) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= maximum
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_dns_subdomain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            let bytes = label.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 63
                && bytes
                    .first()
                    .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                && bytes
                    .last()
                    .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        })
}

fn valid_label_key(value: &str) -> bool {
    match value.split_once('/') {
        Some((prefix, name)) => {
            !name.contains('/') && valid_dns_subdomain(prefix) && valid_label_segment(name, 63)
        }
        None => valid_label_segment(value, 63),
    }
}

pub(super) fn parse_reporter_pod_selector(value: &str) -> AppResult<BTreeMap<String, String>> {
    if value.len() > 1_024 {
        return Err(AppError::internal(format!(
            "{KOTH_REPORTER_POD_SELECTOR_ENV} exceeds 1024 bytes"
        )));
    }
    let mut selector = BTreeMap::new();
    for item in value.split(',').map(str::trim) {
        let (key, label_value) = item.split_once('=').ok_or_else(|| {
            AppError::internal(format!(
                "{KOTH_REPORTER_POD_SELECTOR_ENV} must use comma-separated key=value labels"
            ))
        })?;
        let key = key.trim();
        let label_value = label_value.trim();
        if !valid_label_key(key)
            || !valid_label_segment(label_value, 63)
            || selector.contains_key(key)
        {
            return Err(AppError::internal(format!(
                "{KOTH_REPORTER_POD_SELECTOR_ENV} contains an invalid or duplicate label"
            )));
        }
        selector.insert(key.to_string(), label_value.to_string());
        if selector.len() > 8 {
            return Err(AppError::internal(format!(
                "{KOTH_REPORTER_POD_SELECTOR_ENV} supports at most 8 labels"
            )));
        }
    }
    if !KOTH_REPORTER_REQUIRED_LABELS
        .iter()
        .all(|key| selector.contains_key(*key))
    {
        return Err(AppError::internal(format!(
            "{KOTH_REPORTER_POD_SELECTOR_ENV} must include app.kubernetes.io/name, app.kubernetes.io/instance, and app.kubernetes.io/component from the callback Service selector"
        )));
    }
    Ok(selector)
}

fn configured_reporter_pod_selector() -> AppResult<Option<BTreeMap<String, String>>> {
    std::env::var(KOTH_REPORTER_POD_SELECTOR_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| parse_reporter_pod_selector(&value))
        .transpose()
}

fn parse_cidr(value: &str, variable: &str) -> AppResult<IpNet> {
    value.trim().parse::<IpNet>().map_err(|_| {
        AppError::internal(format!(
            "{variable} contains an invalid IP network: {value}"
        ))
    })
}

fn required_cidr_list(variable: &str) -> AppResult<Vec<IpNet>> {
    let configured = std::env::var(variable).unwrap_or_default();
    let mut networks = Vec::new();
    for value in configured
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let network = parse_cidr(value, variable)?;
        if !networks.contains(&network) {
            networks.push(network);
        }
    }
    if networks.is_empty() {
        return Err(AppError::internal(format!(
            "{variable} must list the exact source CIDRs seen after NodePort routing"
        )));
    }
    Ok(networks)
}

pub(super) fn ad_network_config() -> AppResult<AdNetworkConfig> {
    let service_cidr = crate::services::ad_vpn::kubernetes_services_cidr().ok_or_else(|| {
        AppError::internal(
            "RSCTF_K8S_AD_SERVICE_CIDR must be set to the cluster Service CIDR before provisioning Kubernetes A&D services",
        )
    })?;
    let service_cidr = parse_cidr(&service_cidr, "RSCTF_K8S_AD_SERVICE_CIDR")?;
    let ingress = std::env::var(AD_INGRESS_CIDRS_ENV).unwrap_or_default();
    let mut ingress_cidrs = Vec::new();
    for value in ingress
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let cidr = parse_cidr(value, AD_INGRESS_CIDRS_ENV)?;
        if !ingress_cidrs.contains(&cidr) {
            ingress_cidrs.push(cidr);
        }
    }
    let control_namespace = configured_control_namespace();
    if control_namespace.is_none() && ingress_cidrs.is_empty() {
        return Err(AppError::internal(
            "set RSCTF_K8S_CONTROL_NAMESPACE for an in-cluster rsctf pod or RSCTF_K8S_AD_INGRESS_CIDRS for an external WireGuard hub",
        ));
    }
    let control_pod_label = configured_control_pod_label()?;
    let reporter_pod_selector = configured_reporter_pod_selector()?;
    let client_cidr = parse_cidr(
        &crate::services::ad_vpn::client_cidr(),
        "RSCTF_AD_VPN_CLIENT_CIDR",
    )?;
    if !ingress_cidrs.contains(&client_cidr) {
        ingress_cidrs.push(client_cidr);
    }
    Ok(AdNetworkConfig {
        service_cidr,
        ingress_cidrs,
        control_namespace,
        control_pod_label,
        reporter_pod_selector,
    })
}

fn ip_peer(cidr: impl ToString, except: Option<Vec<String>>) -> NetworkPolicyPeer {
    NetworkPolicyPeer {
        ip_block: Some(IPBlock {
            cidr: cidr.to_string(),
            except,
        }),
        ..Default::default()
    }
}

fn network_port(port: i32, protocol: &str) -> NetworkPolicyPort {
    NetworkPolicyPort {
        port: Some(IntOrString::Int(port)),
        protocol: Some(protocol.to_string()),
        ..Default::default()
    }
}

fn selected_pod_peer(
    namespace: String,
    match_labels: BTreeMap<String, String>,
) -> NetworkPolicyPeer {
    NetworkPolicyPeer {
        namespace_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "kubernetes.io/metadata.name".to_string(),
                namespace,
            )])),
            ..Default::default()
        }),
        pod_selector: Some(LabelSelector {
            match_labels: Some(match_labels),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn control_pod_peer(namespace: String, label: (String, String)) -> NetworkPolicyPeer {
    selected_pod_peer(namespace, BTreeMap::from([label]))
}

fn dns_egress_rule() -> NetworkPolicyEgressRule {
    let dns_peer = NetworkPolicyPeer {
        namespace_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "kubernetes.io/metadata.name".to_string(),
                "kube-system".to_string(),
            )])),
            ..Default::default()
        }),
        pod_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "k8s-app".to_string(),
                "kube-dns".to_string(),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };
    NetworkPolicyEgressRule {
        ports: Some(vec![network_port(53, "UDP"), network_port(53, "TCP")]),
        to: Some(vec![dns_peer]),
    }
}

fn internet_egress_rules(extra_private: &[IpNet]) -> Vec<NetworkPolicyEgressRule> {
    let mut v4_except = vec![
        "0.0.0.0/8".to_string(),
        "10.0.0.0/8".to_string(),
        "100.64.0.0/10".to_string(),
        "127.0.0.0/8".to_string(),
        "169.254.0.0/16".to_string(),
        "172.16.0.0/12".to_string(),
        "192.168.0.0/16".to_string(),
        "198.18.0.0/15".to_string(),
        "224.0.0.0/4".to_string(),
        "240.0.0.0/4".to_string(),
    ];
    let mut v6_except = vec![
        "::/128".to_string(),
        "::1/128".to_string(),
        "fc00::/7".to_string(),
        "fe80::/10".to_string(),
        "ff00::/8".to_string(),
    ];
    for cidr in extra_private {
        let value = cidr.to_string();
        let except = match cidr {
            IpNet::V4(_) => &mut v4_except,
            IpNet::V6(_) => &mut v6_except,
        };
        if !except.contains(&value) {
            except.push(value);
        }
    }

    let internet = NetworkPolicyEgressRule {
        ports: None,
        to: Some(vec![
            ip_peer("0.0.0.0/0", Some(v4_except)),
            ip_peer("::/0", Some(v6_except)),
        ]),
    };
    vec![internet, dns_egress_rule()]
}

pub(super) fn ad_network_policy(
    name: &str,
    labels: &BTreeMap<String, String>,
    owner_references: Option<Vec<OwnerReference>>,
    expose_port: i32,
    allow_egress: bool,
    control_plane_callback_ports: &[i32],
    config: &AdNetworkConfig,
) -> NetworkPolicy {
    let mut ingress_peers: Vec<NetworkPolicyPeer> = config
        .ingress_cidrs
        .iter()
        .map(|cidr| ip_peer(cidr, None))
        .collect();
    if let Some(namespace) = config.control_namespace.as_ref() {
        ingress_peers.push(control_pod_peer(
            namespace.clone(),
            config.control_pod_label.clone(),
        ));
    }
    let mut egress = if allow_egress {
        let mut private = config.ingress_cidrs.clone();
        private.push(config.service_cidr);
        internet_egress_rules(&private)
    } else {
        Vec::new()
    };
    if !control_plane_callback_ports.is_empty() {
        if let (Some(namespace), Some(reporter_pod_selector)) = (
            config.control_namespace.as_ref(),
            config.reporter_pod_selector.as_ref(),
        ) {
            egress.insert(
                0,
                NetworkPolicyEgressRule {
                    ports: Some(
                        control_plane_callback_ports
                            .iter()
                            .map(|port| network_port(*port, "TCP"))
                            .collect(),
                    ),
                    to: Some(vec![selected_pod_peer(
                        namespace.clone(),
                        reporter_pod_selector.clone(),
                    )]),
                },
            );
            if !allow_egress {
                egress.push(dns_egress_rule());
            }
        }
    }
    NetworkPolicy {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels.clone()),
            owner_references,
            ..Default::default()
        },
        spec: Some(NetworkPolicySpec {
            egress: Some(egress),
            ingress: Some(vec![NetworkPolicyIngressRule {
                from: Some(ingress_peers),
                ports: Some(vec![network_port(expose_port, "TCP")]),
            }]),
            pod_selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            policy_types: Some(vec!["Ingress".to_string(), "Egress".to_string()]),
        }),
    }
}

pub(super) fn proxy_network_policy(
    name: &str,
    labels: &BTreeMap<String, String>,
    expose_port: i32,
) -> AppResult<NetworkPolicy> {
    let namespace = configured_control_namespace().ok_or_else(|| {
        AppError::internal(
            "PlatformProxy on Kubernetes requires RSCTF_K8S_CONTROL_NAMESPACE or an in-cluster service-account namespace",
        )
    })?;
    Ok(proxy_network_policy_for_control(
        name,
        labels,
        expose_port,
        namespace,
        configured_control_pod_label()?,
    ))
}

pub(super) fn isolated_network_policy(
    name: &str,
    labels: &BTreeMap<String, String>,
    expose_port: i32,
) -> AppResult<NetworkPolicy> {
    let allowed = required_cidr_list(ISOLATED_INGRESS_CIDRS_ENV)?;
    let pod_cidrs = required_cidr_list(POD_CIDRS_ENV)?;
    let peers = isolated_ingress_peers(&allowed, &pod_cidrs)?;
    Ok(isolated_network_policy_for_peers(
        name,
        labels,
        expose_port,
        peers,
    ))
}

pub(super) fn isolated_network_policy_for_peers(
    name: &str,
    labels: &BTreeMap<String, String>,
    expose_port: i32,
    peers: Vec<NetworkPolicyPeer>,
) -> NetworkPolicy {
    NetworkPolicy {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(NetworkPolicySpec {
            egress: Some(Vec::new()),
            ingress: Some(vec![NetworkPolicyIngressRule {
                // NodePort sources are explicit IPBlocks with every overlapping
                // Pod CIDR excluded, so another challenge Pod is never a peer.
                from: Some(peers),
                ports: Some(vec![network_port(expose_port, "TCP")]),
            }]),
            pod_selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            policy_types: Some(vec!["Ingress".to_string(), "Egress".to_string()]),
        }),
    }
}

pub(super) fn isolated_ingress_peers(
    allowed: &[IpNet],
    pod_cidrs: &[IpNet],
) -> AppResult<Vec<NetworkPolicyPeer>> {
    let mut peers = Vec::with_capacity(allowed.len());
    for ingress in allowed {
        if pod_cidrs.iter().any(|pod| pod.contains(ingress)) {
            return Err(AppError::internal(format!(
                "{ISOLATED_INGRESS_CIDRS_ENV} entry {ingress} is inside a cluster Pod CIDR"
            )));
        }
        let except = pod_cidrs
            .iter()
            .filter(|pod| ingress.contains(*pod))
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        peers.push(ip_peer(ingress, (!except.is_empty()).then_some(except)));
    }
    Ok(peers)
}

pub(super) fn isolated_proxy_network_policy(
    name: &str,
    labels: &BTreeMap<String, String>,
    expose_port: i32,
) -> AppResult<NetworkPolicy> {
    let mut policy = proxy_network_policy(name, labels, expose_port)?;
    let spec = policy
        .spec
        .as_mut()
        .expect("proxy policies always carry a policy spec");
    spec.egress = Some(Vec::new());
    spec.policy_types = Some(vec!["Ingress".to_string(), "Egress".to_string()]);
    Ok(policy)
}

pub(super) fn proxy_network_policy_for_control(
    name: &str,
    labels: &BTreeMap<String, String>,
    expose_port: i32,
    namespace: String,
    control_pod_label: (String, String),
) -> NetworkPolicy {
    let peer = control_pod_peer(namespace, control_pod_label);
    NetworkPolicy {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels.clone()),
            ..Default::default()
        },
        spec: Some(NetworkPolicySpec {
            // Selecting Ingress only preserves ordinary Jeopardy egress while
            // denying every source except the exact rsctf control identity.
            egress: None,
            ingress: Some(vec![NetworkPolicyIngressRule {
                from: Some(vec![peer]),
                ports: Some(vec![network_port(expose_port, "TCP")]),
            }]),
            pod_selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            policy_types: Some(vec!["Ingress".to_string()]),
        }),
    }
}

pub(super) fn network_policy_required(ad_internal: bool, proxy_only: bool, isolated: bool) -> bool {
    ad_internal || proxy_only || isolated
}

pub(super) fn rollback_created_policy(policy_created: bool, pod_adopted: bool) -> bool {
    policy_created && !pod_adopted
}

pub(super) fn service_ip_is_routed(cluster_ip: &str, service_cidr: &IpNet) -> bool {
    cluster_ip
        .parse::<IpAddr>()
        .ok()
        .is_some_and(|ip| service_cidr.contains(&ip))
}
