use std::collections::HashMap;
use std::convert::Infallible;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode};
use kube::client::Body;
use tower::service_fn;

use super::*;

type ResourceStore = Arc<Mutex<HashMap<String, serde_json::Value>>>;

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

fn mock_manager(fail_first_service_response: bool) -> (KubernetesContainerManager, ResourceStore) {
    let resources = Arc::new(Mutex::new(HashMap::<String, serde_json::Value>::new()));
    let captured_resources = Arc::clone(&resources);
    let fail_service = Arc::new(AtomicBool::new(fail_first_service_response));
    let service = service_fn(move |request: Request<Body>| {
        let resources = Arc::clone(&captured_resources);
        let fail_service = Arc::clone(&fail_service);
        async move {
            let method = request.method().clone();
            let path = request.uri().path().to_string();
            let body = request.into_body().collect_bytes().await.unwrap();

            if method == Method::GET {
                let value = resources.lock().unwrap().get(&path).cloned().unwrap();
                return Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&value).unwrap()))
                        .unwrap(),
                );
            }

            assert_eq!(method, Method::POST, "unexpected mutation: {method} {path}");
            let mut value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let name = value["metadata"]["name"].as_str().unwrap().to_string();
            let resource_path = format!("{path}/{name}");
            if resources.lock().unwrap().contains_key(&resource_path) {
                return Ok::<_, Infallible>(conflict(&name));
            }

            if path.ends_with("/pods") {
                value["metadata"]["uid"] = serde_json::json!(format!("uid-{name}"));
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

            if path.ends_with("/services") && fail_service.swap(false, Ordering::SeqCst) {
                let error = serde_json::json!({
                    "apiVersion": "v1",
                    "kind": "Status",
                    "status": "Failure",
                    "message": "the service response was lost after persistence",
                    "reason": "InternalError",
                    "code": 500
                });
                return Ok::<_, Infallible>(
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&error).unwrap()))
                        .unwrap(),
                );
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
    let (manager, _) = mock_manager(false);
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
async fn lost_service_response_retains_pod_and_rejects_a_stale_owner_uid() {
    let (manager, resources) = mock_manager(true);
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
