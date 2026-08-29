use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode};
use kube::client::Body;
use tower::service_fn;

use super::*;

type ResourceStore = Arc<Mutex<HashMap<String, serde_json::Value>>>;

#[derive(Clone, Copy)]
#[repr(u8)]
enum ServiceFailure {
    None = 0,
    AmbiguousOnce = 1,
    ApiOnce = 2,
}

struct RestoreEnv(Vec<(&'static str, Option<OsString>)>);

impl RestoreEnv {
    fn set(values: &[(&'static str, &'static str)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var_os(key)))
            .collect();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self(previous)
    }
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            if let Some(value) = value {
                std::env::set_var(key, value);
            } else {
                std::env::remove_var(key);
            }
        }
    }
}

fn conflict(name: &str) -> Response<Body> {
    let error = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Status",
        "status": "Failure",
        "message": format!("{name} already exists"),
        "reason": "AlreadyExists",
        "details": { "name": name },
        "code": 409
    });
    Response::builder()
        .status(StatusCode::CONFLICT)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&error).unwrap()))
        .unwrap()
}

fn api_error(code: StatusCode, reason: &str, message: &str) -> Response<Body> {
    let error = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Status",
        "status": "Failure",
        "message": message,
        "reason": reason,
        "code": code.as_u16()
    });
    Response::builder()
        .status(code)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&error).unwrap()))
        .unwrap()
}

fn mock_manager(failure: ServiceFailure) -> (KubernetesContainerManager, ResourceStore) {
    let resources = Arc::new(Mutex::new(HashMap::<String, serde_json::Value>::new()));
    let captured_resources = Arc::clone(&resources);
    let fail_service = Arc::new(AtomicU8::new(failure as u8));
    let service = service_fn(move |request: Request<Body>| {
        let resources = Arc::clone(&captured_resources);
        let fail_service = Arc::clone(&fail_service);
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_string();
            let is_list = request.uri().query().is_some();
            let body = request.into_body().collect_bytes().await.unwrap();

            if method == Method::GET {
                if let Some(value) = resources.lock().unwrap().get(&path).cloned() {
                    return Ok::<_, std::io::Error>(
                        Response::builder()
                            .status(StatusCode::OK)
                            .header(CONTENT_TYPE, "application/json")
                            .body(Body::from(serde_json::to_vec(&value).unwrap()))
                            .unwrap(),
                    );
                }
                if is_list {
                    let items: Vec<_> = resources
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(resource_path, _)| {
                            resource_path
                                .strip_prefix(&path)
                                .is_some_and(|suffix| suffix.starts_with('/'))
                        })
                        .map(|(_, value)| value.clone())
                        .collect();
                    let kind = if path.ends_with("/pods") {
                        "PodList"
                    } else if path.ends_with("/services") {
                        "ServiceList"
                    } else {
                        "NetworkPolicyList"
                    };
                    let list = serde_json::json!({
                        "apiVersion": "v1",
                        "kind": kind,
                        "metadata": {},
                        "items": items
                    });
                    return Ok::<_, std::io::Error>(
                        Response::builder()
                            .status(StatusCode::OK)
                            .header(CONTENT_TYPE, "application/json")
                            .body(Body::from(serde_json::to_vec(&list).unwrap()))
                            .unwrap(),
                    );
                }
                return Ok::<_, std::io::Error>(api_error(
                    StatusCode::NOT_FOUND,
                    "NotFound",
                    "resource not found",
                ));
            }

            if method == Method::DELETE {
                resources.lock().unwrap().remove(&path);
                let deleted = serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "Status",
                    "status": "Success",
                    "code": 200
                });
                return Ok::<_, std::io::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&deleted).unwrap()))
                        .unwrap(),
                );
            }

            assert_eq!(method, Method::POST, "unexpected mutation: {method} {path}");
            let mut value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let name = value["metadata"]["name"].as_str().unwrap().to_string();
            let resource_path = format!("{path}/{name}");
            if resources.lock().unwrap().contains_key(&resource_path) {
                return Ok::<_, std::io::Error>(conflict(&name));
            }

            let service_failure = path
                .ends_with("/services")
                .then(|| fail_service.swap(ServiceFailure::None as u8, Ordering::SeqCst));
            if service_failure == Some(ServiceFailure::ApiOnce as u8) {
                return Ok::<_, std::io::Error>(api_error(
                    StatusCode::FORBIDDEN,
                    "Forbidden",
                    "service admission rejected the request",
                ));
            }

            value["metadata"]["uid"] = serde_json::json!(format!("uid-{name}"));
            if path.ends_with("/pods") {
                value["status"] = serde_json::json!({
                    "phase": "Running",
                    "hostIP": "192.0.2.10"
                });
            } else if path.ends_with("/services") {
                value["spec"]["clusterIP"] = serde_json::json!("10.96.12.34");
                value["spec"]["ports"][0]["nodePort"] = serde_json::json!(30080);
            } else if !path.ends_with("/networkpolicies") {
                panic!("unexpected Kubernetes collection path: {path}");
            }
            resources
                .lock()
                .unwrap()
                .insert(resource_path, value.clone());

            if service_failure == Some(ServiceFailure::AmbiguousOnce as u8) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "the service response was lost after persistence",
                ));
            }

            Ok::<_, std::io::Error>(
                Response::builder()
                    .status(StatusCode::CREATED)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&value).unwrap()))
                    .unwrap(),
            )
        }
    });
    (
        KubernetesContainerManager {
            client: Client::new(service, "rsctf-challenges"),
            namespace: "rsctf-challenges".to_string(),
            scope: orphans::workload_scope("rsctf-challenges", None),
            public_entry: Some("192.0.2.10".to_string()),
        },
        resources,
    )
}

fn retry_spec(proxy_only: bool) -> ContainerSpec {
    ContainerSpec {
        game_kind: rsctf_worker_protocol::GameKind::Jeopardy,
        image: format!("registry.example/challenge@sha256:{}", "a".repeat(64)),
        memory_limit: 256,
        cpu_count: 1,
        storage_limit: crate::services::container::DEFAULT_CONTAINER_STORAGE_MB,
        expose_port: 8080,
        publish_port: true,
        proxy_only,
        env: Vec::new(),
        flag: Some("flag{stable-retry}".to_string()),
        ad_network: None,
        allow_egress: false,
        control_plane_callback_ports: Vec::new(),
        network_mode: crate::utils::enums::NetworkMode::Open,
        operation_id: Some("jeopardy-instance:41:team:7".to_string()),
    }
}

fn rename_mock_workload_as_legacy(
    resources: &ResourceStore,
    current_name: &str,
    spec: &ContainerSpec,
) -> String {
    let operation_id = spec.operation_id.as_deref().unwrap();
    let legacy_uid = crate::utils::codec::sha256_str(operation_id)[..16].to_string();
    let legacy_name = format!("{}-{legacy_uid}", sanitize_image(&spec.image));
    let current_suffix = format!("/{current_name}");
    let mut stored = resources.lock().unwrap();
    let paths: Vec<_> = stored
        .keys()
        .filter(|path| path.ends_with(&current_suffix))
        .cloned()
        .collect();
    for path in paths {
        let mut value = stored.remove(&path).unwrap();
        value["metadata"]["name"] = serde_json::json!(legacy_name);
        value["metadata"]["labels"]["app"] = serde_json::json!(format!("rsctf-{legacy_uid}"));
        value["metadata"]["labels"]["rsctf.container"] = serde_json::json!(legacy_name);
        value["metadata"]["labels"]
            .as_object_mut()
            .unwrap()
            .remove("rsctf.launch-spec");
        if path.contains("/pods/") {
            value["metadata"]["uid"] = serde_json::json!(format!("uid-{legacy_name}"));
            value["spec"]["containers"][0]["name"] = serde_json::json!(legacy_name);
        } else if path.contains("/services/") {
            value["metadata"]["ownerReferences"][0]["name"] = serde_json::json!(legacy_name);
            value["metadata"]["ownerReferences"][0]["uid"] =
                serde_json::json!(format!("uid-{legacy_name}"));
            value["spec"]["selector"]["app"] = serde_json::json!(format!("rsctf-{legacy_uid}"));
        }
        let collection = path.rsplit_once('/').unwrap().0;
        stored.insert(format!("{collection}/{legacy_name}"), value);
    }
    legacy_name
}

#[tokio::test]
async fn changed_spec_or_rendered_policy_does_not_adopt_a_kubernetes_crash_orphan() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let _environment = RestoreEnv::set(&[
        ("RSCTF_K8S_CONTROL_NAMESPACE", "rsctf-system"),
        (
            "RSCTF_K8S_CONTROL_POD_LABEL",
            "app.kubernetes.io/name=rsctf-a",
        ),
    ]);
    let (manager, _) = mock_manager(ServiceFailure::None);
    let original = retry_spec(true);

    let first = manager.create(original.clone()).await.unwrap();
    let mut changed = original.clone();
    changed.memory_limit += 1;
    assert!(matches!(
        manager.create(changed).await,
        Err(AppError::Conflict(_))
    ));
    let mut changed_image = original.clone();
    changed_image.image = format!("other.registry/renamed@sha256:{}", "b".repeat(64));
    assert!(matches!(
        manager.create(changed_image).await,
        Err(AppError::Conflict(_))
    ));

    std::env::set_var(
        "RSCTF_K8S_CONTROL_POD_LABEL",
        "app.kubernetes.io/name=rsctf-b",
    );
    assert!(matches!(
        manager.create(original.clone()).await,
        Err(AppError::Conflict(_))
    ));

    std::env::set_var(
        "RSCTF_K8S_CONTROL_POD_LABEL",
        "app.kubernetes.io/name=rsctf-a",
    );
    let retried = manager.create(original).await.unwrap();
    assert_eq!(first.id, retried.id);
}

#[tokio::test]
async fn changed_ad_service_cidr_does_not_adopt_a_kubernetes_crash_orphan() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let _environment = RestoreEnv::set(&[
        ("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12"),
        ("RSCTF_K8S_AD_INGRESS_CIDRS", "192.0.2.0/24"),
    ]);
    let (manager, _) = mock_manager(ServiceFailure::None);
    let mut original = retry_spec(false);
    original.game_kind = rsctf_worker_protocol::GameKind::AttackDefense;
    original.ad_network = Some("rsctf-ad".to_string());

    let first = manager.create(original.clone()).await.unwrap();
    std::env::set_var("RSCTF_K8S_AD_SERVICE_CIDR", "10.240.0.0/16");
    assert!(matches!(
        manager.create(original.clone()).await,
        Err(AppError::Conflict(_))
    ));

    std::env::set_var("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12");
    let retried = manager.create(original).await.unwrap();
    assert_eq!(first.id, retried.id);
}

#[tokio::test]
async fn retry_discovers_and_validates_the_previous_kubernetes_workload_name() {
    let (manager, resources) = mock_manager(ServiceFailure::None);
    let spec = retry_spec(false);
    let first = manager.create(spec.clone()).await.unwrap();
    let legacy_name = rename_mock_workload_as_legacy(&resources, &first.id, &spec);

    let retried = manager.create(spec.clone()).await.unwrap();
    assert_eq!(retried.id, legacy_name);
    assert_eq!(resources.lock().unwrap().len(), 2);

    let mut changed = spec;
    changed.image = format!("other.registry/renamed@sha256:{}", "b".repeat(64));
    assert!(matches!(
        manager.create(changed).await,
        Err(AppError::Conflict(_))
    ));
    assert_eq!(resources.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn lost_service_response_retains_pod_and_rejects_a_stale_owner_uid() {
    let (manager, resources) = mock_manager(ServiceFailure::AmbiguousOnce);
    let spec = retry_spec(false);
    assert!(matches!(
        manager.create(spec.clone()).await,
        Err(AppError::Internal(_))
    ));
    assert_eq!(resources.lock().unwrap().len(), 2);

    let retried = manager.create(spec.clone()).await.unwrap();
    let pod_path = format!("/api/v1/namespaces/rsctf-challenges/pods/{}", retried.id);
    let service_path = format!(
        "/api/v1/namespaces/rsctf-challenges/services/{}",
        retried.id
    );
    {
        let mut stored = resources.lock().unwrap();
        let pod_uid = stored[&pod_path]["metadata"]["uid"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            stored[&service_path]["metadata"]["ownerReferences"][0]["uid"],
            pod_uid
        );
        stored.get_mut(&service_path).unwrap()["metadata"]["ownerReferences"][0]["uid"] =
            serde_json::json!("uid-deleted-pod");
    }

    assert!(matches!(
        manager.create(spec).await,
        Err(AppError::Conflict(_))
    ));
}

#[tokio::test]
async fn authoritative_service_rejection_rolls_back_a_stable_operation() {
    let (manager, resources) = mock_manager(ServiceFailure::ApiOnce);
    let error = manager.create(retry_spec(false)).await.unwrap_err();

    assert!(matches!(error, AppError::Internal(_)));
    assert!(
        resources.lock().unwrap().is_empty(),
        "an authoritative API rejection must not retain the Pod"
    );
}

#[tokio::test]
#[ignore = "requires a disposable Kubernetes cluster prepared by the live regression script"]
async fn real_kubernetes_legacy_retry_and_authoritative_rollback() {
    const IMAGE: &str = "registry.k8s.io/e2e-test-images/agnhost@sha256:99c6b4bb4a1e1df3f0b3752168c89358794d02258ebebc26bf21c29399011a85";
    const OPERATION: &str = "rsctf-live-legacy-operation";
    if std::env::var("RSCTF_K8S_LIVE_RETRY").ok().as_deref() != Some("1") {
        eprintln!("skipping live Kubernetes retry regression without RSCTF_K8S_LIVE_RETRY=1");
        return;
    }
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let _environment = RestoreEnv::set(&[
        ("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12"),
        ("RSCTF_K8S_AD_INGRESS_CIDRS", "192.0.2.0/24"),
    ]);
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let manager = KubernetesContainerManager::connect()
        .await
        .expect("connect to the disposable Kubernetes cluster");
    let spec = ContainerSpec {
        game_kind: rsctf_worker_protocol::GameKind::AttackDefense,
        image: IMAGE.to_string(),
        memory_limit: 64,
        cpu_count: 1,
        storage_limit: crate::services::container::DEFAULT_CONTAINER_STORAGE_MB,
        expose_port: 8080,
        publish_port: true,
        proxy_only: false,
        env: Vec::new(),
        flag: None,
        ad_network: Some("rsctf-ad".to_string()),
        allow_egress: false,
        control_plane_callback_ports: Vec::new(),
        network_mode: crate::utils::enums::NetworkMode::Open,
        operation_id: Some(OPERATION.to_string()),
    };
    let legacy_uid = crate::utils::codec::sha256_str(OPERATION)[..16].to_string();
    let legacy_name = format!("{}-{legacy_uid}", sanitize_image(IMAGE));
    let legacy_pod = manager
        .pods()
        .get(&legacy_name)
        .await
        .expect("inspect the previous release's Pod labels");
    let legacy_labels = legacy_pod
        .metadata
        .labels
        .expect("the previous release's Pod has ownership labels");
    let config = ad_network_config(false).expect("render the live A&D policy");
    let mut legacy_policy = ad_network_policy(
        &legacy_name,
        &legacy_labels,
        None,
        spec.expose_port,
        spec.allow_egress,
        &spec.control_plane_callback_ports,
        &config,
    );
    legacy_policy.metadata.namespace = Some(manager.namespace.clone());
    let persisted_legacy_policy = manager
        .network_policies()
        .create(&PostParams::default(), &legacy_policy)
        .await
        .expect("create the previous release's NetworkPolicy");
    assert!(
        compat::legacy_policy_matches(&persisted_legacy_policy, &legacy_policy),
        "the Kubernetes API changed the legacy policy shape:\nactual={}\nexpected={}",
        serde_json::to_string_pretty(&persisted_legacy_policy).unwrap(),
        serde_json::to_string_pretty(&legacy_policy).unwrap()
    );

    let adopted = manager
        .create(spec.clone())
        .await
        .expect("adopt the previous release's workload name");
    assert_eq!(adopted.id, legacy_name);
    let mut changed = spec.clone();
    changed.image = format!("registry.example/changed@sha256:{}", "b".repeat(64));
    assert!(matches!(
        manager.create(changed).await,
        Err(AppError::Conflict(_))
    ));
    manager
        .destroy(&adopted.id)
        .await
        .expect("clean up the adopted live workload");

    let mut cidr_bound = spec.clone();
    cidr_bound.operation_id = Some("rsctf-live-service-cidr".to_string());
    let current = manager
        .create(cidr_bound.clone())
        .await
        .expect("create a workload bound to the current Service CIDR");
    std::env::set_var("RSCTF_K8S_AD_SERVICE_CIDR", "10.240.0.0/16");
    assert!(matches!(
        manager.create(cidr_bound).await,
        Err(AppError::Conflict(_))
    ));
    std::env::set_var("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12");
    orphans::rollback_owned(
        manager.pods(),
        manager.services(),
        manager.network_policies(),
        &current.id,
        &manager.scope,
    )
    .await
    .expect("clean up the Service-CIDR workload");

    let quota_namespace = std::env::var("RSCTF_K8S_REJECTION_NAMESPACE")
        .expect("rejection namespace configured by the live script");
    let quota_manager = KubernetesContainerManager {
        client: manager.client.clone(),
        scope: orphans::workload_scope(&quota_namespace, None),
        namespace: quota_namespace,
        public_entry: manager.public_entry.clone(),
    };
    let mut rejected = spec;
    rejected.operation_id = Some("rsctf-live-rejected-service".to_string());
    let rejected_name = workload_name_and_uid(
        &rejected.image,
        &quota_manager.scope,
        rejected.operation_id.as_deref(),
    )
    .0;
    assert!(matches!(
        quota_manager.create(rejected).await,
        Err(AppError::Internal(_))
    ));
    assert!(
        quota_manager
            .pods()
            .get_opt(&rejected_name)
            .await
            .expect("inspect rejected operation cleanup")
            .is_none(),
        "a definitive Service quota rejection retained its Pod"
    );
}
