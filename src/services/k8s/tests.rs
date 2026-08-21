use super::*;
use ipnet::IpNet;

#[test]
fn private_proxy_and_ad_services_use_cluster_ip() {
    assert_eq!(service_type(true), "ClusterIP");
    assert_eq!(service_type(false), "NodePort");
    let cidr: IpNet = "10.96.0.0/12".parse().unwrap();
    assert!(service_ip_is_routed("10.96.12.34", &cidr));
    assert!(!service_ip_is_routed("10.13.40.2", &cidr));
}

#[test]
fn ad_policy_is_default_deny_with_allowlisted_ingress() {
    let labels = BTreeMap::from([(APP_LABEL.to_string(), "rsctf-test".to_string())]);
    let config = network::AdNetworkConfig {
        service_cidr: "10.96.0.0/12".parse().unwrap(),
        ingress_cidrs: vec!["10.244.1.0/24".parse().unwrap()],
        control_namespace: Some("rsctf-system".to_string()),
        control_pod_label: ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
    };
    let policy = network::ad_network_policy("test", &labels, None, 8080, false, &config);
    let spec = policy.spec.unwrap();
    assert_eq!(spec.egress, Some(Vec::new()));
    assert_eq!(spec.ingress.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        spec.ingress
            .as_ref()
            .and_then(|rules| rules[0].from.as_ref())
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        spec.policy_types,
        Some(vec!["Ingress".to_string(), "Egress".to_string()])
    );
    assert_eq!(spec.pod_selector.match_labels, Some(labels));
}

#[test]
fn ad_internet_egress_still_excludes_private_networks() {
    let labels = BTreeMap::from([(APP_LABEL.to_string(), "rsctf-test".to_string())]);
    let config = network::AdNetworkConfig {
        service_cidr: "10.96.0.0/12".parse().unwrap(),
        ingress_cidrs: vec!["10.244.1.0/24".parse().unwrap()],
        control_namespace: Some("rsctf-system".to_string()),
        control_pod_label: ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
    };
    let policy = network::ad_network_policy("test", &labels, None, 8080, true, &config);
    let egress = policy.spec.unwrap().egress.unwrap();
    assert_eq!(egress.len(), 2);
    let internet_peers = egress[0].to.as_ref().unwrap();
    let ipv4 = internet_peers[0].ip_block.as_ref().unwrap();
    assert_eq!(ipv4.cidr, "0.0.0.0/0");
    assert!(ipv4
        .except
        .as_ref()
        .unwrap()
        .contains(&"10.0.0.0/8".to_string()));
    assert_eq!(egress[1].ports.as_ref().map(Vec::len), Some(2));
}

#[test]
fn proxy_policy_allows_only_the_control_identity_on_the_exact_tcp_port() {
    let labels = BTreeMap::from([(APP_LABEL.to_string(), "rsctf-proxy-test".to_string())]);
    let render = || {
        network::proxy_network_policy_for_control(
            "proxy-test",
            &labels,
            8080,
            "rsctf-system".to_string(),
            ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
        )
    };
    let first = render();
    let second = render();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap(),
        "retry rendering must be idempotent"
    );

    let spec = first.spec.unwrap();
    assert_eq!(spec.egress, None);
    assert_eq!(spec.policy_types, Some(vec!["Ingress".to_string()]));
    assert_eq!(spec.pod_selector.match_labels, Some(labels));
    let ingress = spec.ingress.unwrap();
    assert_eq!(ingress.len(), 1);
    let ports = ingress[0].ports.as_ref().unwrap();
    assert_eq!(ports.len(), 1);
    assert_eq!(ports[0].port, Some(IntOrString::Int(8080)));
    assert_eq!(ports[0].protocol.as_deref(), Some("TCP"));
    let peers = ingress[0].from.as_ref().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(
        peers[0]
            .namespace_selector
            .as_ref()
            .and_then(|selector| selector.match_labels.as_ref())
            .and_then(|labels| labels.get("kubernetes.io/metadata.name"))
            .map(String::as_str),
        Some("rsctf-system")
    );
    assert_eq!(
        peers[0]
            .pod_selector
            .as_ref()
            .and_then(|selector| selector.match_labels.as_ref())
            .and_then(|labels| labels.get("app.kubernetes.io/name"))
            .map(String::as_str),
        Some("rsctf")
    );
    assert!(peers[0].ip_block.is_none());
}

#[test]
fn rollback_policy_ownership_covers_private_modes_only() {
    assert!(!network::network_policy_required(false, false, false));
    assert!(network::network_policy_required(false, true, false));
    assert!(network::network_policy_required(true, false, false));
    assert!(network::network_policy_required(false, false, true));

    assert!(network::rollback_created_policy(true, false));
    assert!(!network::rollback_created_policy(false, false));
    assert!(
        !network::rollback_created_policy(true, true),
        "a retry must not remove the new policy protecting an adopted pod"
    );
}

#[test]
fn isolated_policy_allows_only_the_service_port_and_denies_egress() {
    let labels = BTreeMap::from([(APP_LABEL.to_string(), "rsctf-isolated".to_string())]);
    let policy = network::isolated_network_policy("isolated", &labels, 8080);
    let spec = policy.spec.unwrap();
    assert_eq!(spec.egress, Some(Vec::new()));
    assert_eq!(
        spec.policy_types,
        Some(vec!["Ingress".to_string(), "Egress".to_string()])
    );
    let ingress = spec.ingress.unwrap();
    assert_eq!(ingress.len(), 1);
    assert!(ingress[0].from.is_none());
    assert_eq!(
        ingress[0].ports.as_ref().unwrap()[0].port,
        Some(IntOrString::Int(8080))
    );
}

#[test]
fn kubernetes_backend_requires_explicit_network_policy_acknowledgement() {
    assert!(network::policy_enforcement_acknowledged(Some("true")));
    assert!(network::policy_enforcement_acknowledged(Some(" TRUE ")));
    assert!(!network::policy_enforcement_acknowledged(None));
    assert!(!network::policy_enforcement_acknowledged(Some("false")));
    assert!(!network::policy_enforcement_acknowledged(Some("1")));
}

#[test]
fn challenge_pods_use_restricted_security_context() {
    let context = challenge_security_context();
    assert_eq!(context.allow_privilege_escalation, Some(false));
    assert_eq!(context.privileged, Some(false));
    assert_eq!(context.run_as_non_root, Some(true));
    let capabilities = context.capabilities.unwrap();
    assert_eq!(capabilities.drop, Some(vec!["ALL".to_string()]));
    assert_eq!(capabilities.add, Some(vec!["NET_BIND_SERVICE".to_string()]));
    assert_eq!(context.seccomp_profile.unwrap().type_, "RuntimeDefault");
}

#[test]
fn only_terminal_pod_phases_authorize_repair() {
    assert_eq!(phase_liveness(Some("Running")), ContainerLiveness::Running);
    assert_eq!(
        phase_liveness(Some("Succeeded")),
        ContainerLiveness::Stopped
    );
    assert_eq!(phase_liveness(Some("Failed")), ContainerLiveness::Stopped);
    for phase in [Some("Pending"), Some("Unknown"), None] {
        assert_eq!(phase_liveness(phase), ContainerLiveness::Unknown);
    }
}
