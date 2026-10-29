use super::{config, ProfileRouter};
use crate::services::container::{
    ContainerBackendKind, ContainerInfo, ContainerManager, ContainerResourceLimits, ContainerSpec,
};
use crate::utils::error::AppResult;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Runtime {
    destroyed: Mutex<Vec<String>>,
    created: Mutex<Vec<ContainerSpec>>,
}

#[async_trait]
impl ContainerManager for Runtime {
    fn backend_kind(&self) -> ContainerBackendKind {
        ContainerBackendKind::Docker
    }
    async fn image_exists(&self, image: &str) -> bool {
        image == "present-on-selected-daemon"
    }
    async fn create(&self, spec: ContainerSpec) -> AppResult<ContainerInfo> {
        self.created.lock().unwrap().push(spec);
        Ok(ContainerInfo {
            id: "same-native-id".into(),
            ip: "127.0.0.1".into(),
            port: 80,
            status: "running".into(),
        })
    }
    async fn destroy(&self, id: &str) -> AppResult<()> {
        self.destroyed.lock().unwrap().push(id.to_owned());
        Ok(())
    }
    async fn query(&self, _: &str) -> AppResult<crate::services::container::ContainerStatus> {
        unreachable!()
    }
}

#[tokio::test]
async fn disabled_profiles_reject_new_launches_but_keep_cleanup_and_inventory_ownership() {
    let manager = Arc::new(Runtime::default());
    let mut profile =
        config::parse(r#"[{"id":"old","name":"Old","backend":"docker","isolation":"container"}]"#)
            .unwrap()
            .remove(0);
    let active =
        ProfileRouter::new(manager.clone(), vec![(profile.clone(), manager.clone())]).unwrap();
    let mut request = spec(Some("old"));
    request.operation_id = Some("x".repeat(256));
    let instance = active.create(request).await.unwrap();
    assert!(
        manager.created.lock().unwrap()[0]
            .operation_id
            .as_ref()
            .unwrap()
            .len()
            < 256
    );
    assert!(manager.created.lock().unwrap()[0].runtime_profile.is_none());
    let identity = profile.identity();
    profile.enabled = false;
    profile.name = "Retired".into();
    assert_eq!(profile.identity(), identity);
    let retired = ProfileRouter::new(manager.clone(), vec![(profile, manager.clone())]).unwrap();
    assert!(retired.create(spec(Some("old"))).await.is_err());
    assert!(retired.profile_backend_kind(Some("old")).is_err());
    assert!(
        !retired
            .profile_image_exists(Some("old"), "present-on-selected-daemon")
            .await
    );
    retired.destroy(&instance.id).await.unwrap();
    assert_eq!(manager.created.lock().unwrap().len(), 1);
    assert_eq!(manager.destroyed.lock().unwrap().len(), 1);
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn registered_profiles_cannot_disappear_or_change_isolation_after_restart() {
    let db = crate::utils::test_database::TestDatabase::new("runtime_profiles").await;
    sqlx::raw_sql(r#"
        CREATE TABLE "RuntimeProfileBindings" (id VARCHAR(40) PRIMARY KEY, identity VARCHAR(16) NOT NULL);
        CREATE TABLE "Containers" (container_id TEXT);
        CREATE TABLE "AdTeamServices" (container_id TEXT);
        CREATE TABLE "KothTargets" (container_id TEXT);
        CREATE TABLE "PlayerContainerOperations" (backend_id TEXT);
        CREATE TABLE "ManagedContainerReapOperations" (backend_id TEXT);
        CREATE TABLE "KothCrownCycles" (old_container_id TEXT, replacement_container_id TEXT);
    "#).execute(&db.pool).await.unwrap();
    let runtime = Arc::new(Runtime::default());
    let mut profile = config::parse(
        r#"[{"id":"stable","name":"Stable","backend":"docker","isolation":"container"}]"#,
    )
    .unwrap()
    .remove(0);
    let router =
        ProfileRouter::new(runtime.clone(), vec![(profile.clone(), runtime.clone())]).unwrap();
    router.validate_persisted_handles(&db.pool).await.unwrap();
    router.validate_persisted_handles(&db.pool).await.unwrap();
    let missing = ProfileRouter::new(runtime.clone(), vec![]).unwrap();
    assert!(missing.validate_persisted_handles(&db.pool).await.is_err());
    profile.enabled = false;
    let retired =
        ProfileRouter::new(runtime.clone(), vec![(profile.clone(), runtime.clone())]).unwrap();
    retired.validate_persisted_handles(&db.pool).await.unwrap();
    profile.backend = super::ProfileBackend::Kubernetes;
    let changed = ProfileRouter::new(runtime.clone(), vec![(profile, runtime)]).unwrap();
    assert!(changed.validate_persisted_handles(&db.pool).await.is_err());
    db.close().await;
}

fn spec(profile: Option<&str>) -> ContainerSpec {
    let mut spec = ContainerSpec::ad_service(
        "sha256:fixture".into(),
        ContainerResourceLimits {
            memory_limit: 128,
            cpu_count: 1,
            storage_limit: 64,
        },
        80,
        1,
        false,
        "fixture".into(),
    );
    spec.runtime_profile = profile.map(str::to_owned);
    spec
}

#[test]
fn profiles_reject_ambiguous_or_unisolated_microvm_configuration() {
    for input in [
        r#"[{"id":"vm","name":"VM","backend":"docker","isolation":"microvm"}]"#,
        r#"[{"id":"vm","name":"VM","backend":"kubernetes","isolation":"microvm"}]"#,
        r#"[{"id":"../x","name":"VM","backend":"docker","isolation":"container"}]"#,
        r#"[{"id":"default","name":"Default","backend":"docker","isolation":"container"}]"#,
        r#"[{"id":"vm","name":"VM","backend":"kubernetes","isolation":"container","runtimeClass":"kata"}]"#,
    ] {
        assert!(config::parse(input).is_err(), "{input}");
    }
}

#[test]
fn checker_routes_cover_both_backends_and_retired_instances_without_a_fallback_cidr() {
    let profiles = config::parse(r#"[
        {"id":"docker","name":"Docker","backend":"docker","isolation":"container"},
        {"id":"vm","name":"Retired VM","backend":"kubernetes","isolation":"microvm","runtimeClass":"kata","enabled":false}
    ]"#).unwrap();
    assert!(config::checker_service_cidrs(&profiles, "10.13.40.0/24", None).is_err());
    assert!(config::checker_service_cidrs(&profiles, "10.13.40.0/24", Some("invalid")).is_err());
    assert_eq!(
        config::checker_service_cidrs(&profiles, "10.13.40.0/24", Some("10.96.0.0/12")).unwrap(),
        vec!["10.13.40.0/24", "10.96.0.0/12"]
    );
}

#[tokio::test]
async fn switching_profiles_keeps_old_instances_owned_by_their_original_runtime() {
    let default = Arc::new(Runtime::default());
    let docker = Arc::new(Runtime::default());
    let kube = Arc::new(Runtime::default());
    let profiles = config::parse(r#"[
        {"id":"docker","name":"Docker","backend":"docker","isolation":"container"},
        {"id":"vm","name":"MicroVM","backend":"kubernetes","isolation":"microvm","runtimeClass":"kata"}
    ]"#).unwrap();
    let router = ProfileRouter::new(
        default.clone(),
        vec![
            (profiles[0].clone(), docker.clone()),
            (profiles[1].clone(), kube.clone()),
        ],
    )
    .unwrap();
    let old = router.create(spec(Some("docker"))).await.unwrap();
    let new = router.create(spec(Some("vm"))).await.unwrap();
    assert_ne!(old.id, new.id);
    router.destroy(&old.id).await.unwrap();
    assert_eq!(*docker.destroyed.lock().unwrap(), vec!["same-native-id"]);
    assert!(kube.destroyed.lock().unwrap().is_empty());
    router.destroy(&new.id).await.unwrap();
    router.destroy("legacy-native-id").await.unwrap();
    assert_eq!(*kube.destroyed.lock().unwrap(), vec!["same-native-id"]);
    assert_eq!(*default.destroyed.lock().unwrap(), vec!["legacy-native-id"]);
    assert!(router.create(spec(Some("missing"))).await.is_err());
    assert!(
        router
            .profile_image_exists(Some("docker"), "present-on-selected-daemon")
            .await
    );
    assert!(
        !router
            .profile_image_exists(Some("missing"), "present-on-selected-daemon")
            .await
    );
    let changed = old
        .id
        .replacen(&profiles[0].identity(), "0000000000000000", 1);
    assert!(router.destroy(&changed).await.is_err());
    assert_eq!(docker.destroyed.lock().unwrap().len(), 1);
}
