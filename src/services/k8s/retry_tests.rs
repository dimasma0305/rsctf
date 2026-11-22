use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode};
use kube::client::Body;
use tower::service_fn;

use super::*;

type ResourceStore = Arc<Mutex<HashMap<String, serde_json::Value>>>;

#[tokio::test]
async fn ad_replacement_checks_actual_pod_storage_before_withdrawing_the_donor() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let (manager, resources) = mock_manager(ServiceFailure::None);
    let created = manager.create(retry_spec(false)).await.unwrap();
    let id = created.id;
    let path = format!("/api/v1/namespaces/rsctf-challenges/pods/{id}");
    assert!(!manager.has_ad_data_volumes(&id).await.unwrap());
    for (volume, safe) in [
        (serde_json::json!({"configMap":{"name":"settings"}}), true),
        (serde_json::json!({"secret":{"secretName":"cert"}}), true),
        (serde_json::json!({"projected":{"sources":[]}}), true),
        (serde_json::json!({"downwardAPI":{"items":[]}}), true),
        (
            serde_json::json!({"persistentVolumeClaim":{"claimName":"team-a"}}),
            false,
        ),
        (serde_json::json!({"emptyDir":{}}), false),
        (serde_json::json!({"hostPath":{"path":"/data"}}), false),
    ] {
        let mut volume = volume;
        volume["name"] = serde_json::json!("application");
        resources.lock().unwrap().get_mut(&path).unwrap()["spec"]["volumes"] =
            serde_json::json!([volume]);
        let result = manager.has_ad_data_volumes(&id).await;
        if safe {
            assert!(!result.unwrap());
        } else {
            assert!(matches!(result, Err(AppError::Conflict(_))));
        }
    }
    let pod = resources.lock().unwrap().get(&path).unwrap().clone();
    let mut foreign = pod;
    foreign["spec"]["volumes"] = serde_json::json!([]);
    foreign["metadata"]["labels"]["rsctf.scope"] = serde_json::json!("other-installation");
    resources.lock().unwrap().insert(path.clone(), foreign);
    assert!(matches!(
        manager.has_ad_data_volumes(&id).await,
        Err(AppError::Conflict(_))
    ));
    assert!(resources.lock().unwrap().remove(&path).is_some());
    assert!(!manager.has_ad_data_volumes(&id).await.unwrap());
}

#[tokio::test]
async fn microvm_launch_pins_runtime_class_and_never_falls_back_on_class_removal() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let (mut manager, resources) = mock_manager(ServiceFailure::None);
    let class_path = "/apis/node.k8s.io/v1/runtimeclasses/kata";
    resources.lock().unwrap().insert(
        class_path.into(),
        serde_json::json!({
            "apiVersion":"node.k8s.io/v1", "kind":"RuntimeClass", "metadata":{"name":"kata"},
            "handler":"kata-qemu", "overhead":{"podFixed":{"memory":"256Mi"}}
        }),
    );
    let profile = crate::services::container::profiles::config::parse(
        r#"[{"id":"microvm","name":"MicroVM","backend":"kubernetes","isolation":"microvm","runtimeClass":"kata"}]"#,
    ).unwrap().remove(0);
    manager = manager.with_profile(&profile).await.unwrap();
    assert!(
        manager.scope.len() <= 63,
        "profile selectors must fit Kubernetes label values"
    );
    let created = manager.create(retry_spec(false)).await.unwrap();
    let path = format!("/api/v1/namespaces/rsctf-challenges/pods/{}", created.id);
    assert_eq!(
        resources.lock().unwrap()[&path]["spec"]["runtimeClassName"],
        "kata"
    );
    assert_eq!(
        manager.create(retry_spec(false)).await.unwrap().id,
        created.id
    );
    resources.lock().unwrap().remove(class_path);
    let count = resources.lock().unwrap().len();
    assert!(matches!(
        manager
            .profile_pull_image(None, &retry_spec(false).image)
            .await,
        Err(AppError::ServiceUnavailable(_))
    ));
    assert!(manager.create(retry_spec(false)).await.is_err());
    assert_eq!(resources.lock().unwrap().len(), count);
    // Clearing the class cannot adopt a pod created under VM isolation.
    manager.runtime_class = None;
    assert!(matches!(
        manager.create(retry_spec(false)).await,
        Err(AppError::Conflict(_))
    ));
    manager.destroy(&created.id).await.unwrap();
}

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

pub(crate) fn mock_storage_manager() -> KubernetesContainerManager {
    mock_manager(ServiceFailure::None).0
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
            let selector = request.uri().query().and_then(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .find(|(key, _)| key == "labelSelector")
                    .map(|(_, v)| v.into_owned())
            });
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
                        .filter(|item| {
                            selector.as_deref().is_none_or(|s| {
                                s.split(',').all(|predicate| {
                                    predicate.split_once('=').is_some_and(|(key, value)| {
                                        item["metadata"]["labels"][key].as_str() == Some(value)
                                    })
                                })
                            })
                        })
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
                if value["metadata"]["annotations"]["rsctf.io/data-claim-uid"].is_string() {
                    value["status"]["conditions"] =
                        serde_json::json!([{"type":"Ready","status":"True"}]);
                }
            } else if path.ends_with("/services") {
                value["spec"]["clusterIP"] = serde_json::json!("10.96.12.34");
                value["spec"]["ports"][0]["nodePort"] = serde_json::json!(30080);
            } else if !path.ends_with("/networkpolicies")
                && !path.ends_with("/persistentvolumeclaims")
            {
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
            runtime_class: None,
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
        runtime_profile: None,
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
        team_storage: None,
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
async fn opted_in_readiness_is_persisted_and_keeps_a_running_pod_pending() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let _environment = RestoreEnv::set(&[("RSCTF_K8S_READINESS_PROBE", "true")]);
    let (manager, resources) = mock_manager(ServiceFailure::None);
    let info = manager.create(retry_spec(false)).await.unwrap();
    assert_eq!(info.status, "pending");
    let path = format!("/api/v1/namespaces/rsctf-challenges/pods/{}", info.id);
    {
        let mut resources = resources.lock().unwrap();
        let pod = resources.get_mut(&path).unwrap();
        let container = &pod["spec"]["containers"][0];
        assert_eq!(container["readinessProbe"]["tcpSocket"]["port"], 8080);
        assert!(container.get("livenessProbe").is_none());
        pod["status"]["conditions"] = serde_json::json!([{ "type": "Ready", "status": "True" }]);
    }
    assert_eq!(manager.query(&info.id).await.unwrap().status, "running");
    // A launch created with a different probe policy cannot adopt this identity.
    let _changed = RestoreEnv::set(&[("RSCTF_K8S_READINESS_PROBE", "false")]);
    assert!(matches!(
        manager.create(retry_spec(false)).await,
        Err(AppError::Conflict(_))
    ));
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
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
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
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
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
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let (manager, resources) = mock_manager(ServiceFailure::ApiOnce);
    let error = manager.create(retry_spec(false)).await.unwrap_err();

    assert!(matches!(error, AppError::Internal(_)));
    assert!(
        resources.lock().unwrap().is_empty(),
        "an authoritative API rejection must not retain the Pod"
    );
}

#[tokio::test]
async fn terminal_adopted_pod_is_removed_before_the_operation_key_rotates() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let (manager, resources) = mock_manager(ServiceFailure::None);
    let spec = retry_spec(false);
    let created = manager.create(spec.clone()).await.unwrap();
    let pod_path = format!("/api/v1/namespaces/rsctf-challenges/pods/{}", created.id);
    resources.lock().unwrap().get_mut(&pod_path).unwrap()["status"]["phase"] =
        serde_json::json!("Failed");

    assert!(matches!(
        manager.create(spec).await,
        Err(AppError::Conflict(_))
    ));
    assert!(
        resources.lock().unwrap().is_empty(),
        "a terminal adopted Pod must not leave its Service or operation resources behind"
    );
}

#[tokio::test]
async fn hosted_team_claim_is_isolated_reused_and_checked_before_donor_removal() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let _environment = RestoreEnv::set(&[
        ("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12"),
        ("RSCTF_K8S_AD_INGRESS_CIDRS", "192.0.2.0/24"),
    ]);
    let (manager, resources) = mock_manager(ServiceFailure::None);
    let mut spec = retry_spec(false);
    spec.game_kind = rsctf_worker_protocol::GameKind::AttackDefense;
    spec.ad_network = Some("rsctf-ad".into());
    spec.team_storage = Some(crate::services::container::TeamStorage {
        game_id: 1,
        participation_id: 2,
        challenge_id: 3,
        mount_path: "/data".into(),
        allocation_id: uuid::Uuid::new_v4(),
        claim_uid: None,
    });
    let data = spec.team_storage.as_mut().unwrap();
    data.claim_uid = Some(
        manager
            .prepare_team_claim(None, data, spec.storage_limit)
            .await
            .unwrap(),
    );
    let first = manager.create(spec.clone()).await.unwrap();
    let claim = spec
        .team_storage
        .as_ref()
        .unwrap()
        .claim_name(&manager.scope);
    let pvc = format!("/api/v1/namespaces/rsctf-challenges/persistentvolumeclaims/{claim}");
    assert_eq!(
        resources.lock().unwrap()[&pvc]["spec"]["accessModes"][0],
        "ReadWriteOncePod"
    );
    assert_eq!(manager.create(spec.clone()).await.unwrap().id, first.id);
    assert!(!manager
        .ad_donor_needs_transfer(&first.id, &spec)
        .await
        .unwrap());
    let mut peer = spec.clone();
    peer.team_storage.as_mut().unwrap().participation_id = 4;
    let data = peer.team_storage.as_mut().unwrap();
    data.allocation_id = uuid::Uuid::new_v4();
    data.claim_uid = None;
    data.claim_uid = Some(
        manager
            .prepare_team_claim(None, data, peer.storage_limit)
            .await
            .unwrap(),
    );
    peer.operation_id = Some("second-team".into());
    let other = manager.create(peer.clone()).await.unwrap();
    assert_ne!(first.id, other.id);
    assert!(matches!(
        manager.ad_donor_needs_transfer(&first.id, &peer).await,
        Err(AppError::Conflict(_))
    ));
    let before = resources.lock().unwrap()[&pvc].clone();
    manager.destroy(&first.id).await.unwrap();
    spec.operation_id = Some("second-revision".into());
    let next = manager.create(spec.clone()).await.unwrap();
    assert_ne!(first.id, next.id);
    assert_eq!(resources.lock().unwrap()[&pvc], before);
    resources.lock().unwrap().get_mut(&pvc).unwrap()["metadata"]["uid"] =
        serde_json::json!("replaced");
    assert!(matches!(
        manager.ad_donor_needs_transfer(&next.id, &spec).await,
        Err(AppError::Conflict(_))
    ));
    resources.lock().unwrap().get_mut(&pvc).unwrap()["metadata"]["labels"]
        ["rsctf.io/participation"] = serde_json::json!("4");
    assert!(matches!(
        manager.create(spec.clone()).await,
        Err(AppError::Conflict(_))
    ));
    manager.destroy(&next.id).await.unwrap();
    resources.lock().unwrap().remove(&pvc);
    let data = spec.team_storage.as_ref().unwrap();
    assert!(matches!(
        manager
            .prepare_team_claim(None, data, spec.storage_limit)
            .await,
        Err(AppError::Conflict(_))
    ));
    assert!(manager.create(spec).await.is_err());
    assert!(!resources.lock().unwrap().contains_key(&pvc));
}

#[tokio::test]
async fn ad_retry_verifies_the_named_containers_current_image_before_adoption() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let _environment = RestoreEnv::set(&[
        ("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12"),
        ("RSCTF_K8S_AD_INGRESS_CIDRS", "192.0.2.0/24"),
    ]);
    let mut spec = retry_spec(false);
    spec.game_kind = rsctf_worker_protocol::GameKind::AttackDefense;
    spec.ad_network = Some("rsctf-ad".into());
    let (manager, resources) = mock_manager(ServiceFailure::None);
    let id = manager.create(spec.clone()).await.unwrap().id;
    let path = format!("/api/v1/namespaces/rsctf-challenges/pods/{id}");
    let digest = spec.image.split_once('@').unwrap().1;
    {
        let mut store = resources.lock().unwrap();
        let pod = store.get_mut(&path).unwrap();
        pod["spec"]["volumes"] = serde_json::json!([{
            "name":"data","persistentVolumeClaim":{"claimName":"team-records"}
        }]);
        let containers = pod["spec"]["containers"].as_array_mut().unwrap();
        containers[0]["image"] = serde_json::json!(format!("mirror.example/service@{digest}"));
        containers.insert(0, serde_json::json!({"name":"sidecar","image":spec.image}));
    }
    assert_eq!(manager.create(spec.clone()).await.unwrap().id, id);
    for image in [
        format!("registry.example/service@sha256:{}", "c".repeat(64)),
        "service:latest".into(),
    ] {
        resources.lock().unwrap().get_mut(&path).unwrap()["spec"]["containers"][1]["image"] =
            serde_json::json!(image);
        let before = resources.lock().unwrap().clone();
        assert!(matches!(
            manager.create(spec.clone()).await,
            Err(AppError::Conflict(_))
        ));
        assert_eq!(*resources.lock().unwrap(), before);
    }
}

#[tokio::test]
async fn ad_retry_retains_team_storage_on_terminal_pod_or_service_rejection() {
    let _environment_lock = KUBERNETES_ENV_TEST_LOCK.lock().await;
    let _environment = RestoreEnv::set(&[
        ("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12"),
        ("RSCTF_K8S_AD_INGRESS_CIDRS", "192.0.2.0/24"),
    ]);
    let mut spec = retry_spec(false);
    spec.game_kind = rsctf_worker_protocol::GameKind::AttackDefense;
    spec.ad_network = Some("rsctf-ad".into());
    let (manager, resources) = mock_manager(ServiceFailure::None);
    let id = manager.create(spec.clone()).await.unwrap().id;
    let path = format!("/api/v1/namespaces/rsctf-challenges/pods/{id}");
    {
        let mut store = resources.lock().unwrap();
        let pod = store.get_mut(&path).unwrap();
        pod["spec"]["volumes"] = serde_json::json!([{
            "name": "application", "persistentVolumeClaim": {"claimName": "team-records"}
        }]);
        pod["status"]["phase"] = serde_json::json!("Failed");
    }
    let snapshot = resources.lock().unwrap().clone();
    let error = manager.create(spec.clone()).await.unwrap_err();
    assert!(matches!(error, AppError::Conflict(reason) if reason.contains("team-records")));
    assert_eq!(*resources.lock().unwrap(), snapshot);

    // The Pod survived an interrupted creation, but the Service did not.
    // A quota rejection on retry must preserve the Pod and its isolation.
    let (rejected, resources) = mock_manager(ServiceFailure::ApiOnce);
    let mut running = snapshot;
    running
        .remove(&format!(
            "/api/v1/namespaces/rsctf-challenges/services/{id}"
        ))
        .unwrap();
    running.get_mut(&path).unwrap()["status"]["phase"] = serde_json::json!("Running");
    *resources.lock().unwrap() = running.clone();
    let error = rejected.create(spec.clone()).await.unwrap_err();
    assert!(matches!(error, AppError::Conflict(reason) if reason.contains("team-records")));
    assert_eq!(*resources.lock().unwrap(), running);

    let (stateless, resources) = mock_manager(ServiceFailure::ApiOnce);
    running.get_mut(&path).unwrap()["spec"]["volumes"] = serde_json::json!([]);
    *resources.lock().unwrap() = running;
    assert!(matches!(
        stateless.create(spec).await,
        Err(AppError::Internal(_))
    ));
    assert!(resources.lock().unwrap().is_empty());
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
    let storage = manager.has_ad_data_volumes("team-storage-donor").await;
    assert!(
        matches!(storage, Err(AppError::Conflict(ref message)) if message.contains("cannot transfer"))
    );
    let spec = ContainerSpec {
        runtime_profile: None,
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
        team_storage: None,
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
    assert!(!manager.has_ad_data_volumes(&adopted.id).await.unwrap());
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
    cidr_bound.image =
        "curlimages/curl@sha256:463eaf6072688fe96ac64fa623fe73e1dbe25d8ad6c34404a669ad3ce1f104b6"
            .into();
    let current = manager
        .create(cidr_bound.clone())
        .await
        .expect("create a workload bound to the current Service CIDR");
    std::env::set_var("RSCTF_K8S_AD_SERVICE_CIDR", "10.240.0.0/16");
    assert!(matches!(
        manager.create(cidr_bound.clone()).await,
        Err(AppError::Conflict(_))
    ));
    std::env::set_var("RSCTF_K8S_AD_SERVICE_CIDR", "10.96.0.0/12");
    // Patch only the Pod image, as an in-place rollout does. The UID, labels
    // and Service still belong to the original operation.
    let pod_before = manager.pods().get(&current.id).await.unwrap();
    let service_before = manager.services().get(&current.id).await.unwrap();
    let patch = kube::api::Patch::Strategic(serde_json::json!({"spec":{"containers":[{
        "name": current.id, "image": IMAGE
    }]}}));
    manager
        .pods()
        .patch(&current.id, &kube::api::PatchParams::default(), &patch)
        .await
        .unwrap();
    assert!(matches!(
        manager.create(cidr_bound.clone()).await,
        Err(AppError::Conflict(reason)) if reason.contains("requested revision")
    ));
    let after = manager.pods().get(&current.id).await.unwrap();
    assert_eq!(after.metadata.uid, pod_before.metadata.uid);
    assert!(compat::pod_image_matches(&after, &current.id, IMAGE));
    let service_after = manager.services().get(&current.id).await.unwrap();
    assert_eq!(service_after.metadata.uid, service_before.metadata.uid);
    assert_eq!(
        service_after.spec.unwrap().cluster_ip,
        service_before.spec.unwrap().cluster_ip
    );
    let labels = after.metadata.labels.unwrap();
    manager
        .destroy(&current.id)
        .await
        .expect("clean up the Service-CIDR retry fixture");
    // Model a persisted, admission-mutated Pod left from an interrupted
    // launch. Let the actual kubelet run the command to completion.
    let completed: Pod = serde_json::from_value(serde_json::json!({
        "metadata": {"name": current.id, "labels": labels},
        "spec": {
            "restartPolicy": "Never", "automountServiceAccountToken": false,
            "volumes": [{"name":"data","persistentVolumeClaim":{"claimName":"team-data","readOnly":true}}],
            "containers": [{
                "name":current.id, "image":cidr_bound.image, "securityContext":challenge_security_context(),
                "command":["sh","-ec","test -s /data/installation"],
                "volumeMounts":[{"name":"data","mountPath":"/data","readOnly":true}]
            }]
        }
    })).unwrap();
    let completed = manager
        .pods()
        .create(&PostParams::default(), &completed)
        .await
        .unwrap();
    let expected_uid = completed.metadata.uid.unwrap();
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let pod = manager.pods().get(&current.id).await.unwrap();
            match pod.status.and_then(|s| s.phase).as_deref() {
                Some("Succeeded") => break,
                Some("Failed") => panic!("fixture could not read the team data"),
                _ => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
    })
    .await
    .expect("kubelet did not finish the storage fixture");
    let error = manager.create(cidr_bound).await.unwrap_err();
    assert!(matches!(error, AppError::Conflict(reason) if reason.contains("team-data")));
    assert_eq!(
        manager.pods().get(&current.id).await.unwrap().metadata.uid,
        Some(expected_uid)
    );

    let quota_namespace = std::env::var("RSCTF_K8S_REJECTION_NAMESPACE")
        .expect("rejection namespace configured by the live script");
    let quota_manager = KubernetesContainerManager {
        runtime_class: None,
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
