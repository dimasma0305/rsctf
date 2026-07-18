use super::*;

#[test]
fn ad_services_use_internal_cluster_ip_services() {
    assert_eq!(service_type(true), "ClusterIP");
    assert_eq!(service_type(false), "NodePort");
    let cidr: IpNet = "10.96.0.0/12".parse().unwrap();
    assert!(service_ip_is_routed("10.96.12.34", &cidr));
    assert!(!service_ip_is_routed("10.13.40.2", &cidr));
}

#[test]
fn ad_policy_is_default_deny_with_allowlisted_ingress() {
    let labels = BTreeMap::from([(APP_LABEL.to_string(), "rsctf-test".to_string())]);
    let config = AdNetworkConfig {
        service_cidr: "10.96.0.0/12".parse().unwrap(),
        ingress_cidrs: vec!["10.244.1.0/24".parse().unwrap()],
        control_namespace: Some("rsctf-system".to_string()),
        control_pod_label: ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
    };
    let policy = ad_network_policy("test", &labels, None, 8080, false, &config);
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
    let config = AdNetworkConfig {
        service_cidr: "10.96.0.0/12".parse().unwrap(),
        ingress_cidrs: vec!["10.244.1.0/24".parse().unwrap()],
        control_namespace: Some("rsctf-system".to_string()),
        control_pod_label: ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
    };
    let policy = ad_network_policy("test", &labels, None, 8080, true, &config);
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
