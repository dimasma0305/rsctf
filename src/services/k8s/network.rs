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
const POLICY_ENFORCED_ENV: &str = "RSCTF_K8S_NETWORK_POLICY_ENFORCED";

#[derive(Clone)]
pub(super) struct AdNetworkConfig {
    pub(super) service_cidr: IpNet,
    pub(super) ingress_cidrs: Vec<IpNet>,
    pub(super) control_namespace: Option<String>,
    pub(super) control_pod_label: (String, String),
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

fn parse_cidr(value: &str, variable: &str) -> AppResult<IpNet> {
    value.trim().parse::<IpNet>().map_err(|_| {
        AppError::internal(format!(
            "{variable} contains an invalid IP network: {value}"
        ))
    })
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

fn control_pod_peer(namespace: String, label: (String, String)) -> NetworkPolicyPeer {
    NetworkPolicyPeer {
        namespace_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "kubernetes.io/metadata.name".to_string(),
                namespace,
            )])),
            ..Default::default()
        }),
        pod_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([label])),
            ..Default::default()
        }),
        ..Default::default()
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
    let dns = NetworkPolicyEgressRule {
        ports: Some(vec![network_port(53, "UDP"), network_port(53, "TCP")]),
        to: Some(vec![dns_peer]),
    };
    vec![internet, dns]
}

pub(super) fn ad_network_policy(
    name: &str,
    labels: &BTreeMap<String, String>,
    owner_references: Option<Vec<OwnerReference>>,
    expose_port: i32,
    allow_egress: bool,
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
    let egress = if allow_egress {
        let mut private = config.ingress_cidrs.clone();
        private.push(config.service_cidr);
        internet_egress_rules(&private)
    } else {
        Vec::new()
    };
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
                // A directly published challenge remains reachable through its
                // Service, but no other pod port is exposed.
                from: None,
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
