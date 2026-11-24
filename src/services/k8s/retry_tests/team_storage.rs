use super::*;

pub(super) fn endpoint_slices(resources: &ResourceStore, selector: &str) -> serde_json::Value {
    use serde_json::json;
    let service_name = selector
        .strip_prefix("kubernetes.io/service-name=")
        .unwrap();
    let store = resources.lock().unwrap();
    let service = &store[&format!("/api/v1/namespaces/rsctf-challenges/services/{service_name}")];
    let claim = &service["spec"]["selector"]["rsctf.io/team-claim"];
    let mut endpoints = Vec::new();
    let mut ports = Vec::new();
    for (path, pod) in store.iter().filter(|(path, _)| path.contains("/pods/")) {
        if &pod["metadata"]["labels"]["rsctf.io/team-claim"] != claim {
            continue;
        }
        for entry in pod["spec"]["containers"][0]["ports"].as_array().unwrap() {
            ports.push(json!({"protocol":entry["protocol"],"name":entry["name"],"port":entry["containerPort"]}));
        }
        endpoints.push(
            json!({"addresses":["10.244.0.5"],"conditions":{"ready":true},"targetRef":{
            "kind":"Pod","name":path.rsplit('/').next().unwrap(),"uid":pod["metadata"]["uid"]}}),
        );
    }
    json!({"apiVersion":"discovery.k8s.io/v1","kind":"EndpointSliceList","metadata":{},"items":[{
        "metadata":{"name":"registered","ownerReferences":[{"apiVersion":"v1","kind":"Service","name":service_name,"uid":service["metadata"]["uid"]}]},
        "addressType":"IPv4","ports":ports,"endpoints":endpoints}]})
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
        endpoint_uid: None,
        initialize_round_flag: false,
    });
    let data = spec.team_storage.as_mut().unwrap();
    let (claim_uid, endpoint_uid) = manager
        .prepare_team_claim(None, data, spec.storage_limit, spec.expose_port)
        .await
        .unwrap();
    data.claim_uid = Some(claim_uid);
    data.endpoint_uid = Some(endpoint_uid);
    let first = manager.create(spec.clone()).await.unwrap();
    let pod = |id: &str| format!("/api/v1/namespaces/rsctf-challenges/pods/{id}");
    assert!(
        resources.lock().unwrap()[&pod(&first.id)]["spec"]["containers"][0]["startupProbe"]
            .is_null()
    );
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
    data.endpoint_uid = None;
    let (claim_uid, endpoint_uid) = manager
        .prepare_team_claim(None, data, peer.storage_limit, peer.expose_port)
        .await
        .unwrap();
    data.claim_uid = Some(claim_uid);
    data.endpoint_uid = Some(endpoint_uid);
    peer.operation_id = Some("second-team".into());
    let other = manager.create(peer.clone()).await.unwrap();
    assert_ne!(first.id, other.id);
    assert!(matches!(
        manager.ad_donor_needs_transfer(&first.id, &peer).await,
        Err(AppError::Conflict(_))
    ));
    let before = resources.lock().unwrap()[&pvc].clone();
    let svc = pvc.replace("persistentvolumeclaims", "services");
    let saved_service = resources.lock().unwrap()[&svc].clone();
    resources.lock().unwrap().get_mut(&svc).unwrap()["spec"]["selector"] =
        serde_json::json!({"app":"other-team"});
    assert!(matches!(
        manager.ad_donor_needs_transfer(&first.id, &spec).await,
        Err(AppError::Conflict(_))
    ));
    resources
        .lock()
        .unwrap()
        .insert(svc.clone(), saved_service.clone());
    manager.destroy(&first.id).await.unwrap();
    assert!(matches!(
        manager.check_ad_patch_image(&first.id, &spec).await,
        Err(AppError::NotFound(_))
    ));
    spec.operation_id = Some("second-revision".into());
    spec.set_ad_launch_flag("round-flag-fixture".into(), true);
    let next = manager.create(spec.clone()).await.unwrap();
    let startup =
        resources.lock().unwrap()[&pod(&next.id)]["spec"]["containers"][0]["startupProbe"].clone();
    assert!(startup["exec"]["command"][2]
        .as_str()
        .unwrap()
        .contains("RSCTF_FLAG_FILE"));
    assert!(!startup.to_string().contains("round-flag-fixture"));
    assert_ne!(first.id, next.id);
    assert_eq!((&next.ip, next.port), (&first.ip, first.port));
    assert_eq!(resources.lock().unwrap()[&pvc], before);
    assert_eq!(resources.lock().unwrap()[&svc], saved_service);
    resources.lock().unwrap().remove(&svc);
    let data = spec.team_storage.as_ref().unwrap();
    assert!(manager
        .prepare_team_claim(None, data, spec.storage_limit, spec.expose_port)
        .await
        .is_err());
    assert!(!resources.lock().unwrap().contains_key(&svc));
    resources.lock().unwrap().insert(svc, saved_service);
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
            .prepare_team_claim(None, data, spec.storage_limit, spec.expose_port)
            .await,
        Err(AppError::Conflict(_))
    ));
    assert!(manager.create(spec).await.is_err());
    assert!(!resources.lock().unwrap().contains_key(&pvc));
}
