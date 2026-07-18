use super::{
    assign_available_ip, assign_deterministic_ip, peer_address_allowed, retry_operation,
    validate_kubernetes_service_routes, validate_vpn_networks, vpn_target, VpnBackendConfig,
};
use crate::services::container::ContainerBackendKind;
use ipnet::Ipv4Net;
use std::cell::Cell;
use std::collections::HashSet;

#[test]
fn wireguard_retry_stops_after_success() {
    let attempts = Cell::new(0);
    let backoffs = Cell::new(0);
    let result = retry_operation(
        3,
        || {
            let current = attempts.get() + 1;
            attempts.set(current);
            if current < 3 {
                Err(format!("attempt {current}"))
            } else {
                Ok(())
            }
        },
        |_| backoffs.set(backoffs.get() + 1),
    );

    assert!(result.is_ok());
    assert_eq!(attempts.get(), 3);
    assert_eq!(backoffs.get(), 2);
}

#[test]
fn vpn_networks_must_be_disjoint_ipv4_ranges() {
    let overlap =
        validate_vpn_networks("10.13.0.0/16", &["10.13.40.0/24".to_string()]).unwrap_err();
    assert!(overlap.contains("overlaps"));

    let (client, services) =
        validate_vpn_networks("10.13.0.7/19", &["10.13.40.99/24".to_string()]).unwrap();
    assert_eq!(client.to_string(), "10.13.0.0/19");
    assert_eq!(services[0].to_string(), "10.13.40.0/24");
    assert!(validate_vpn_networks("fd00::/64", &["10.13.40.0/24".to_string()]).is_err());
}

#[test]
fn peer_addresses_exclude_hub_boundaries_and_service_ranges() {
    let client: Ipv4Net = "10.13.0.0/19".parse().unwrap();
    let services = vec!["10.13.40.0/24".parse().unwrap()];
    assert!(!peer_address_allowed("10.13.0.0", &client, &services));
    assert!(!peer_address_allowed("10.13.0.1", &client, &services));
    assert!(peer_address_allowed("10.13.0.2", &client, &services));
    assert!(!peer_address_allowed("10.13.31.255", &client, &services));
    assert!(!peer_address_allowed("10.13.40.5", &client, &services));
    assert_eq!(
        assign_deterministic_ip("10.13.0.0/19", 3).as_deref(),
        Some("10.13.0.5")
    );
    assert_eq!(
        assign_deterministic_ip("10.13.0.0/19", 8).as_deref(),
        Some("10.13.0.10")
    );
    let used = HashSet::from(["10.13.0.5".parse().unwrap()]);
    assert_eq!(
        assign_available_ip("10.13.0.0/19", 3, &used).as_deref(),
        Some("10.13.0.6")
    );
    assert_eq!(vpn_target("10.13.40.1", 80, &client, &services), None);
    assert_eq!(
        vpn_target("10.13.40.3", 31337, &client, &services)
            .map(|target| (target.address.to_string(), target.port)),
        Some(("10.13.40.3".to_string(), 31337))
    );
    assert_eq!(
        vpn_target("10.13.0.10", 8080, &client, &services).map(|target| target.address.to_string()),
        Some("10.13.0.10".to_string())
    );
}

#[test]
fn kubernetes_backend_requires_an_authoritative_service_cidr_without_vpn() {
    let missing = VpnBackendConfig {
        kind: ContainerBackendKind::Kubernetes,
        service_cidrs: Vec::new(),
        guard_service_interfaces: false,
    };
    assert!(validate_kubernetes_service_routes(&missing).is_err());

    let configured = VpnBackendConfig {
        kind: ContainerBackendKind::Kubernetes,
        service_cidrs: vec!["10.96.0.0/12".to_string()],
        guard_service_interfaces: false,
    };
    assert!(validate_kubernetes_service_routes(&configured).is_ok());
}
