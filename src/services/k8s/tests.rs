use super::*;
use ipnet::IpNet;

fn reporter_selector() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
        (
            "app.kubernetes.io/instance".to_string(),
            "rsctf-network".to_string(),
        ),
        (
            "app.kubernetes.io/component".to_string(),
            "network".to_string(),
        ),
    ])
}

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
        reporter_pod_selector: None,
    };
    let policy = network::ad_network_policy("test", &labels, None, 8080, false, &[], &config);
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
        reporter_pod_selector: None,
    };
    let policy = network::ad_network_policy("test", &labels, None, 8080, true, &[], &config);
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
fn managed_koth_callback_allows_only_reporter_http_and_dns() {
    let labels = BTreeMap::from([(APP_LABEL.to_string(), "rsctf-test".to_string())]);
    let config = network::AdNetworkConfig {
        service_cidr: "10.96.0.0/12".parse().unwrap(),
        ingress_cidrs: vec!["10.244.1.0/24".parse().unwrap()],
        control_namespace: Some("rsctf-system".to_string()),
        control_pod_label: ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
        reporter_pod_selector: Some(reporter_selector()),
    };
    let policy =
        network::ad_network_policy("test", &labels, None, 8080, false, &[80, 8080], &config);
    let egress = policy.spec.unwrap().egress.unwrap();
    assert_eq!(egress.len(), 2);
    assert_eq!(egress[0].ports.as_ref().unwrap().len(), 2);
    assert_eq!(
        egress[0].ports.as_ref().unwrap()[0].port,
        Some(IntOrString::Int(80))
    );
    assert_eq!(
        egress[0].ports.as_ref().unwrap()[1].port,
        Some(IntOrString::Int(8080))
    );
    let peer = &egress[0].to.as_ref().unwrap()[0];
    assert_eq!(
        peer.namespace_selector
            .as_ref()
            .and_then(|selector| selector.match_labels.as_ref())
            .and_then(|labels| labels.get("kubernetes.io/metadata.name"))
            .map(String::as_str),
        Some("rsctf-system")
    );
    assert_eq!(
        peer.pod_selector
            .as_ref()
            .and_then(|selector| selector.match_labels.as_ref()),
        Some(&reporter_selector())
    );
    assert_eq!(egress[1].ports.as_ref().map(Vec::len), Some(2));
}

#[test]
fn managed_koth_callback_selector_requires_exact_service_identity() {
    let selector = network::parse_reporter_pod_selector(
        "app.kubernetes.io/name=rsctf,app.kubernetes.io/instance=rsctf-network,app.kubernetes.io/component=network",
    )
    .unwrap();
    assert_eq!(selector, reporter_selector());

    for invalid in [
        "app.kubernetes.io/name=rsctf",
        "app.kubernetes.io/name=rsctf,app.kubernetes.io/instance=rsctf-network",
        "app.kubernetes.io/name=rsctf,app.kubernetes.io/instance=rsctf-network,app.kubernetes.io/component=.network",
        "app.kubernetes.io/name=rsctf,app.kubernetes.io/name=other,app.kubernetes.io/instance=rsctf-network,app.kubernetes.io/component=network",
    ] {
        assert!(
            network::parse_reporter_pod_selector(invalid).is_err(),
            "accepted unsafe selector {invalid}"
        );
    }
}

#[tokio::test]
async fn managed_callback_policy_round_trips_through_kubernetes_api() {
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    use axum::http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode};
    use kube::api::PostParams;
    use kube::client::Body;
    use tower::service_fn;

    let labels = BTreeMap::from([(APP_LABEL.to_string(), "rsctf-test".to_string())]);
    let config = network::AdNetworkConfig {
        service_cidr: "10.96.0.0/12".parse().unwrap(),
        ingress_cidrs: vec!["10.244.1.0/24".parse().unwrap()],
        control_namespace: Some("rsctf-system".to_string()),
        control_pod_label: ("app.kubernetes.io/name".to_string(), "rsctf".to_string()),
        reporter_pod_selector: Some(reporter_selector()),
    };
    let policy =
        network::ad_network_policy("test", &labels, None, 8080, false, &[80, 8080], &config);
    let captured = Arc::new(Mutex::new(None));
    let captured_request = Arc::clone(&captured);
    let service = service_fn(move |request: Request<Body>| {
        let captured_request = Arc::clone(&captured_request);
        async move {
            assert_eq!(request.method(), Method::POST);
            assert_eq!(
                request.uri().path(),
                "/apis/networking.k8s.io/v1/namespaces/rsctf-challenges/networkpolicies"
            );
            let body = request.into_body().collect_bytes().await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            *captured_request.lock().unwrap() = Some(value.clone());
            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::CREATED)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&value).unwrap()))
                    .unwrap(),
            )
        }
    });
    let client = Client::new(service, "rsctf-challenges");
    let policies: Api<NetworkPolicy> = Api::namespaced(client, "rsctf-challenges");

    policies
        .create(&PostParams::default(), &policy)
        .await
        .unwrap();

    let value = captured.lock().unwrap().take().unwrap();
    let ports = value["spec"]["egress"][0]["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|port| port["port"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ports, vec![80, 8080]);
    assert_eq!(
        value["spec"]["egress"][0]["to"][0]["podSelector"]["matchLabels"],
        serde_json::json!({
            "app.kubernetes.io/name": "rsctf",
            "app.kubernetes.io/instance": "rsctf-network",
            "app.kubernetes.io/component": "network"
        })
    );
}

#[tokio::test]
async fn changed_routing_identity_does_not_adopt_a_kubernetes_crash_orphan() {
    use std::convert::Infallible;
    use std::future::pending;
    use std::sync::{Arc, Mutex};

    use axum::http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode};
    use kube::client::Body;
    use tokio::sync::Notify;
    use tower::service_fn;

    let image = format!("registry.example/hill@sha256:{}", "a".repeat(64));
    let spec = |operation_id: &str| ContainerSpec {
        game_kind: rsctf_worker_protocol::GameKind::KingOfTheHill,
        image: image.clone(),
        memory_limit: 256,
        cpu_count: 1,
        storage_limit: crate::services::container::DEFAULT_CONTAINER_STORAGE_MB,
        expose_port: 8080,
        publish_port: true,
        proxy_only: false,
        env: Vec::new(),
        flag: Some("flag{test}".to_string()),
        ad_network: None,
        allow_egress: false,
        control_plane_callback_ports: Vec::new(),
        network_mode: crate::utils::enums::NetworkMode::Open,
        operation_id: Some(operation_id.to_string()),
    };
    let original_operation = "koth-cycle:41:attempt:3:managed-reporter-v1:0123456789abcdef";
    let changed_operation = "koth-cycle:41:attempt:3:managed-reporter-v1:fedcba9876543210";
    let resource_name = |operation: &str| {
        format!(
            "{}-{}",
            sanitize_image(&image),
            &crate::utils::codec::sha256_str(operation)[..16]
        )
    };
    let original_name = resource_name(original_operation);
    let changed_name = resource_name(changed_operation);
    assert_ne!(original_name, changed_name);

    let requests = Arc::new(Mutex::new(Vec::<(Method, String, String)>::new()));
    let captured_requests = Arc::clone(&requests);
    let service_started = Arc::new(Notify::new());
    let captured_service_started = Arc::clone(&service_started);
    let captured_original_name = original_name.clone();
    let service = service_fn(move |request: Request<Body>| {
        let requests = Arc::clone(&captured_requests);
        let service_started = Arc::clone(&captured_service_started);
        let original_name = captured_original_name.clone();
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_string();
            let body = request.into_body().collect_bytes().await.unwrap();
            let mut value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let name = value["metadata"]["name"].as_str().unwrap().to_string();
            requests
                .lock()
                .unwrap()
                .push((method.clone(), path.clone(), name.clone()));

            if path.ends_with("/pods") {
                value["metadata"]["uid"] = serde_json::json!(format!("uid-{name}"));
                value["status"] = serde_json::json!({
                    "phase": "Running",
                    "hostIP": "192.0.2.10"
                });
            }
            if path.ends_with("/services") && name == original_name {
                service_started.notify_one();
                return pending::<Result<Response<Body>, Infallible>>().await;
            }

            Ok::<_, Infallible>(
                Response::builder()
                    .status(StatusCode::CREATED)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&value).unwrap()))
                    .unwrap(),
            )
        }
    });
    let manager = KubernetesContainerManager {
        client: Client::new(service, "rsctf-challenges"),
        namespace: "rsctf-challenges".to_string(),
        scope: orphans::workload_scope("rsctf-challenges", None),
        public_entry: Some("192.0.2.10".to_string()),
    };

    let original_manager = manager.clone();
    let original = spec(original_operation);
    let interrupted = tokio::spawn(async move { original_manager.create(original).await });
    tokio::time::timeout(Duration::from_secs(2), service_started.notified())
        .await
        .expect("the original create reached its Service request");
    interrupted.abort();
    assert!(interrupted.await.unwrap_err().is_cancelled());

    let created = manager.create(spec(changed_operation)).await.unwrap();
    assert_eq!(created.id, changed_name);

    let requests = requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _, name)| *method == Method::POST && name == &original_name)
            .count(),
        2,
        "the interrupted create left a Pod before its Service completed"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(method, _, name)| *method == Method::POST && name == &changed_name)
            .count(),
        2,
        "the retry created a fresh Pod and Service under the changed routing identity"
    );
    assert!(requests
        .iter()
        .all(|(method, _, _)| *method == Method::POST));
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
    let peers = network::isolated_ingress_peers(
        &["0.0.0.0/0".parse().unwrap()],
        &["10.244.0.0/16".parse().unwrap()],
    )
    .unwrap();
    let policy = network::isolated_network_policy_for_peers("isolated", &labels, 8080, peers);
    let spec = policy.spec.unwrap();
    assert_eq!(spec.egress, Some(Vec::new()));
    assert_eq!(
        spec.policy_types,
        Some(vec!["Ingress".to_string(), "Egress".to_string()])
    );
    let ingress = spec.ingress.unwrap();
    assert_eq!(ingress.len(), 1);
    let peers = ingress[0].from.as_ref().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].ip_block.as_ref().unwrap().cidr, "0.0.0.0/0");
    assert_eq!(
        peers[0].ip_block.as_ref().unwrap().except,
        Some(vec!["10.244.0.0/16".to_string()])
    );
    assert_eq!(
        ingress[0].ports.as_ref().unwrap()[0].port,
        Some(IntOrString::Int(8080))
    );
}

#[test]
fn isolated_ingress_rejects_a_source_range_inside_the_pod_network() {
    assert!(network::isolated_ingress_peers(
        &["10.244.2.0/24".parse().unwrap()],
        &["10.244.0.0/16".parse().unwrap()],
    )
    .is_err());
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
