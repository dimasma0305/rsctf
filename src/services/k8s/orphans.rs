use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Pod, Service};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, DeleteParams, ListParams, Preconditions};
use kube::Resource;
use serde::de::DeserializeOwned;

use crate::utils::error::{AppError, AppResult};

pub(super) const APP_LABEL: &str = "app";
const MANAGED_LABEL: &str = "rsctf.managed";
const MANAGED_VALUE: &str = "true";
const CONTAINER_LABEL: &str = "rsctf.container";
const LAUNCH_SPEC_LABEL: &str = "rsctf.launch-spec";
const OPERATION_LABEL: &str = "rsctf.operation";
const SCOPE_LABEL: &str = "rsctf.scope";

/// A scope shared by every role in one installation, while remaining distinct
/// for releases using a different control or challenge namespace.
pub(super) fn workload_scope(namespace: &str, control_namespace: Option<&str>) -> String {
    let control_namespace = control_namespace.unwrap_or(namespace);
    let identity = format!("{control_namespace}\0{namespace}");
    crate::utils::codec::sha256_str(&identity)[..32].to_string()
}

/// Labels shared by the Pod, Service, and NetworkPolicy belonging to one
/// container. The operation label is deterministic only when the caller has a
/// lifecycle-safe operation id; the container label is always present.
pub(super) fn workload_labels(
    name: &str,
    uid: &str,
    scope: &str,
    operation_id: Option<&str>,
    launch_fingerprint: &str,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([
        (APP_LABEL.to_string(), format!("rsctf-{uid}")),
        (MANAGED_LABEL.to_string(), MANAGED_VALUE.to_string()),
        (CONTAINER_LABEL.to_string(), name.to_string()),
        (
            LAUNCH_SPEC_LABEL.to_string(),
            launch_fingerprint_hash(launch_fingerprint),
        ),
        (SCOPE_LABEL.to_string(), scope.to_string()),
    ]);
    if let Some(operation_id) = operation_id {
        labels.insert(OPERATION_LABEL.to_string(), operation_hash(operation_id));
    }
    labels
}

fn operation_hash(operation_id: &str) -> String {
    crate::utils::codec::sha256_str(operation_id)[..32].to_string()
}

fn launch_fingerprint_hash(launch_fingerprint: &str) -> String {
    crate::utils::codec::sha256_str(launch_fingerprint)[..32].to_string()
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct OperationWorkloadIdentity {
    pub name: String,
    pub uid: String,
}

fn current_identity(meta: &ObjectMeta, name: &str, scope: &str) -> bool {
    let Some(labels) = meta.labels.as_ref() else {
        return false;
    };
    labels.get(MANAGED_LABEL).map(String::as_str) == Some(MANAGED_VALUE)
        && labels.get(CONTAINER_LABEL).map(String::as_str) == Some(name)
        && labels.get(SCOPE_LABEL).map(String::as_str) == Some(scope)
}

/// Resources created before managed identity labels were introduced still
/// have the unique `app=rsctf-<16 hex>` selector. Keep explicit DB-owned
/// teardown compatible with them, but never use this fallback for a resource
/// carrying a managed marker from another scope.
fn legacy_identity(meta: &ObjectMeta, name: &str) -> bool {
    let Some(labels) = meta.labels.as_ref() else {
        return false;
    };
    if labels.contains_key(MANAGED_LABEL) {
        return false;
    }
    let Some(uid) = name.rsplit('-').next() else {
        return false;
    };
    uid.len() == 16
        && uid.bytes().all(|byte| byte.is_ascii_hexdigit())
        && labels.get(APP_LABEL).map(String::as_str) == Some(format!("rsctf-{uid}").as_str())
}

fn owned_identity(meta: &ObjectMeta, name: &str, scope: &str) -> bool {
    current_identity(meta, name, scope) || legacy_identity(meta, name)
}

/// Locate a stable operation across both the current operation-only name and
/// the image-prefixed name emitted by older replicas. Each Kubernetes kind is
/// limited because a valid operation owns at most one resource of that kind.
pub(super) async fn find_operation_workload(
    pods: Api<Pod>,
    services: Api<Service>,
    policies: Api<NetworkPolicy>,
    scope: &str,
    operation_id: &str,
) -> AppResult<Option<OperationWorkloadIdentity>> {
    let selector = format!(
        "{MANAGED_LABEL}={MANAGED_VALUE},{SCOPE_LABEL}={scope},{OPERATION_LABEL}={}",
        operation_hash(operation_id)
    );
    let params = ListParams::default().labels(&selector).limit(2);
    let (pods, services, policies) = tokio::join!(
        pods.list(&params),
        services.list(&params),
        policies.list(&params)
    );
    let mut identities = BTreeSet::new();
    collect_operation_identities("pods", pods, scope, operation_id, &mut identities)?;
    collect_operation_identities("services", services, scope, operation_id, &mut identities)?;
    collect_operation_identities(
        "network policies",
        policies,
        scope,
        operation_id,
        &mut identities,
    )?;
    if identities.len() > 1 {
        return Err(AppError::conflict(
            "multiple Kubernetes workloads claim the same operation identity",
        ));
    }
    Ok(identities.into_iter().next())
}

fn collect_operation_identities<K>(
    kind: &str,
    resources: Result<kube::api::ObjectList<K>, kube::Error>,
    scope: &str,
    operation_id: &str,
    identities: &mut BTreeSet<OperationWorkloadIdentity>,
) -> AppResult<()>
where
    K: Clone + Resource,
{
    let resources = resources.map_err(|error| {
        AppError::internal(format!(
            "failed to discover existing Kubernetes {kind} operation resources: {error}"
        ))
    })?;
    if resources.metadata.continue_.is_some() {
        return Err(AppError::conflict(format!(
            "multiple Kubernetes {kind} claim the same operation identity"
        )));
    }
    let expected_operation = operation_hash(operation_id);
    for resource in resources.items {
        let meta = resource.meta();
        let Some(name) = meta.name.as_deref() else {
            return Err(AppError::conflict(format!(
                "Kubernetes {kind} operation resource has no name"
            )));
        };
        let labels = meta.labels.as_ref();
        let operation_matches = labels
            .and_then(|labels| labels.get(OPERATION_LABEL))
            .map(String::as_str)
            == Some(expected_operation.as_str());
        let uid = labels
            .and_then(|labels| labels.get(APP_LABEL))
            .and_then(|app| app.strip_prefix("rsctf-"));
        if !current_identity(meta, name, scope) || !operation_matches || uid.is_none() {
            return Err(AppError::conflict(format!(
                "Kubernetes {kind} operation identity is inconsistent"
            )));
        }
        identities.insert(OperationWorkloadIdentity {
            name: name.to_string(),
            uid: uid.unwrap().to_string(),
        });
    }
    Ok(())
}

pub(super) async fn adopt<K>(
    api: &Api<K>,
    name: &str,
    scope: &str,
    operation_id: Option<&str>,
    launch_fingerprint: &str,
    kind: &str,
    legacy_launch_matches: impl FnOnce(&K) -> bool,
) -> AppResult<K>
where
    K: Clone + Debug + DeserializeOwned + Resource,
{
    let Some(operation_id) = operation_id else {
        return Err(AppError::conflict(format!(
            "{kind} name is already owned by another workload"
        )));
    };
    let resource = api.get(name).await.map_err(|error| {
        AppError::internal(format!(
            "{kind} operation {name} conflicted but could not be inspected: {error}"
        ))
    })?;
    let meta = resource.meta();
    let operation_matches = meta
        .labels
        .as_ref()
        .and_then(|labels| labels.get(OPERATION_LABEL))
        .map(String::as_str)
        == Some(operation_hash(operation_id).as_str());
    let actual_launch_spec = meta
        .labels
        .as_ref()
        .and_then(|labels| labels.get(LAUNCH_SPEC_LABEL))
        .map(String::as_str);
    let launch_spec_matches = actual_launch_spec.map_or_else(
        || legacy_launch_matches(&resource),
        |actual| actual == launch_fingerprint_hash(launch_fingerprint),
    );
    if !owned_identity(meta, name, scope) || !operation_matches || !launch_spec_matches {
        return Err(AppError::conflict(format!(
            "{kind} operation identity is owned by a different workload"
        )));
    }
    Ok(resource)
}

/// Discover every resource set created by this installation. Listing all three
/// kinds catches the policy-only crash window before the Pod is created, as
/// well as Service or Pod remnants from partial Kubernetes garbage collection.
pub(super) async fn list_managed(
    pods: Api<Pod>,
    services: Api<Service>,
    policies: Api<NetworkPolicy>,
    scope: &str,
) -> Vec<String> {
    let selector = format!("{MANAGED_LABEL}={MANAGED_VALUE},{SCOPE_LABEL}={scope}");
    let params = ListParams::default().labels(&selector);
    let (pods, services, policies) = tokio::join!(
        pods.list(&params),
        services.list(&params),
        policies.list(&params)
    );
    let mut names = BTreeSet::new();
    collect_names("pods", pods, scope, &mut names);
    collect_names("services", services, scope, &mut names);
    collect_names("network policies", policies, scope, &mut names);
    names.into_iter().collect()
}

fn collect_names<K>(
    kind: &str,
    resources: Result<kube::api::ObjectList<K>, kube::Error>,
    scope: &str,
    names: &mut BTreeSet<String>,
) where
    K: Clone + Resource,
{
    let resources = match resources {
        Ok(resources) => resources,
        Err(error) => {
            tracing::warn!(%error, resource_kind = kind, "failed to list managed Kubernetes resources");
            return;
        }
    };
    for resource in resources.items {
        let meta = resource.meta();
        let Some(name) = meta.name.as_deref() else {
            tracing::warn!(
                resource_kind = kind,
                "managed Kubernetes resource has no name"
            );
            continue;
        };
        if current_identity(meta, name, scope) {
            names.insert(name.to_string());
        } else {
            tracing::warn!(
                resource_kind = kind,
                resource_name = name,
                "ignoring Kubernetes resource with inconsistent managed identity"
            );
        }
    }
}

/// Re-inspect and delete all members of one resource set. UID preconditions
/// prevent a discovery/delete race from removing a replacement object that
/// reused the same name.
pub(super) async fn destroy_owned(
    pods: Api<Pod>,
    services: Api<Service>,
    policies: Api<NetworkPolicy>,
    name: &str,
    scope: &str,
) -> AppResult<()> {
    let mut errors = Vec::new();
    if let Err(error) = delete_owned(&services, name, scope, "service").await {
        errors.push(error);
    }
    if let Err(error) = delete_pod_before_policy(&pods, &policies, name, scope).await {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::internal(format!(
            "failed to delete Kubernetes resources: {}",
            errors.join("; ")
        )))
    }
}

/// Roll back an unpublished workload after the API definitively rejected a
/// later create. No participant can rely on this Pod, so bypass its normal
/// termination grace while retaining the same UID ownership precondition.
pub(super) async fn rollback_owned(
    pods: Api<Pod>,
    services: Api<Service>,
    policies: Api<NetworkPolicy>,
    name: &str,
    scope: &str,
) -> AppResult<()> {
    let mut errors = Vec::new();
    if let Err(error) = delete_owned(&services, name, scope, "service").await {
        errors.push(error);
    }
    if let Err(error) =
        delete_pod_before_policy_with_grace(&pods, &policies, name, scope, Some(0)).await
    {
        errors.push(error);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::internal(format!(
            "failed to roll back Kubernetes resources: {}",
            errors.join("; ")
        )))
    }
}

/// Remove an owned Pod and wait until it is absent before removing the policy
/// that isolates its labels. If Pod deletion stalls or a replacement appears,
/// the policy remains in place and the orphan reconciler can retry later.
async fn delete_pod_before_policy(
    pods: &Api<Pod>,
    policies: &Api<NetworkPolicy>,
    name: &str,
    scope: &str,
) -> Result<(), String> {
    delete_pod_before_policy_with_grace(pods, policies, name, scope, None).await
}

async fn delete_pod_before_policy_with_grace(
    pods: &Api<Pod>,
    policies: &Api<NetworkPolicy>,
    name: &str,
    scope: &str,
    grace_period_seconds: Option<u32>,
) -> Result<(), String> {
    delete_owned_with_grace(pods, name, scope, "pod", grace_period_seconds).await?;
    wait_until_absent(pods, name, "pod").await?;
    delete_owned(policies, name, scope, "network policy").await
}

pub(super) async fn rollback_created_pod(
    pods: &Api<Pod>,
    policies: &Api<NetworkPolicy>,
    name: &str,
    scope: &str,
    remove_policy: bool,
) -> Result<(), String> {
    if remove_policy {
        delete_pod_before_policy_with_grace(pods, policies, name, scope, Some(0)).await
    } else {
        delete_owned_with_grace(pods, name, scope, "pod", Some(0)).await
    }
}

async fn wait_until_absent<K>(api: &Api<K>, name: &str, kind: &str) -> Result<(), String>
where
    K: Clone + Debug + DeserializeOwned + Resource,
{
    for _ in 0..300 {
        match api.get(name).await {
            Err(error) if super::is_not_found(&error) => return Ok(()),
            Ok(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            Err(error) => return Err(format!("{kind} deletion check: {error}")),
        }
    }
    Err(format!(
        "{kind} deletion did not complete; retaining its NetworkPolicy"
    ))
}

async fn delete_owned<K>(api: &Api<K>, name: &str, scope: &str, kind: &str) -> Result<(), String>
where
    K: Clone + Debug + DeserializeOwned + Resource,
{
    delete_owned_with_grace(api, name, scope, kind, None).await
}

async fn delete_owned_with_grace<K>(
    api: &Api<K>,
    name: &str,
    scope: &str,
    kind: &str,
    grace_period_seconds: Option<u32>,
) -> Result<(), String>
where
    K: Clone + Debug + DeserializeOwned + Resource,
{
    let resource = match api.get(name).await {
        Ok(resource) => resource,
        Err(error) if super::is_not_found(&error) => return Ok(()),
        Err(error) => return Err(format!("{kind} inspect: {error}")),
    };
    let meta = resource.meta();
    if !owned_identity(meta, name, scope) {
        return Err(format!(
            "{kind}: refusing to delete resource with a different managed identity"
        ));
    }
    let Some(uid) = meta.uid.clone() else {
        return Err(format!("{kind}: resource has no Kubernetes UID"));
    };
    let params = DeleteParams {
        grace_period_seconds,
        preconditions: Some(Preconditions {
            uid: Some(uid),
            resource_version: None,
        }),
        ..DeleteParams::default()
    };
    match api.delete(name, &params).await {
        Ok(_) => Ok(()),
        Err(error) if super::is_not_found(&error) => Ok(()),
        Err(error) => Err(format!("{kind}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object_list<K: Clone>(items: Vec<K>) -> kube::api::ObjectList<K> {
        kube::api::ObjectList {
            types: kube::core::TypeMeta {
                api_version: "v1".to_string(),
                kind: "List".to_string(),
            },
            metadata: Default::default(),
            items,
        }
    }

    fn metadata(name: &str, labels: BTreeMap<String, String>) -> ObjectMeta {
        ObjectMeta {
            name: Some(name.to_string()),
            labels: Some(labels),
            ..Default::default()
        }
    }

    #[test]
    fn managed_identity_is_scoped_and_stable() {
        let scope = workload_scope("challenges", Some("control"));
        assert_eq!(scope, workload_scope("challenges", Some("control")));
        assert_ne!(scope, workload_scope("other", Some("control")));
        assert_ne!(scope, workload_scope("challenges", Some("other")));

        let name = "challenge-0123456789abcdef";
        let labels = workload_labels(
            name,
            "0123456789abcdef",
            &scope,
            Some("cycle:42"),
            "launch-spec-a",
        );
        let meta = metadata(name, labels.clone());
        assert!(current_identity(&meta, name, &scope));
        assert!(!current_identity(&meta, name, "another-scope"));
        assert_eq!(labels.get(CONTAINER_LABEL).map(String::as_str), Some(name));
        assert_eq!(labels.get(MANAGED_LABEL).map(String::as_str), Some("true"));
        assert_eq!(labels.get(OPERATION_LABEL).map(String::len), Some(32));
        assert_eq!(labels.get(LAUNCH_SPEC_LABEL).map(String::len), Some(32));
    }

    #[test]
    fn malformed_or_foreign_managed_identity_is_never_owned() {
        let scope = workload_scope("challenges", Some("control"));
        let name = "challenge-0123456789abcdef";
        let mut labels = workload_labels(name, "0123456789abcdef", &scope, None, "launch-spec-a");
        labels.insert(CONTAINER_LABEL.to_string(), "another-container".to_string());
        assert!(!owned_identity(&metadata(name, labels), name, &scope));

        let foreign = workload_labels(
            name,
            "0123456789abcdef",
            "foreign-scope",
            None,
            "launch-spec-a",
        );
        assert!(!owned_identity(&metadata(name, foreign), name, &scope));
    }

    #[test]
    fn legacy_identity_is_only_a_compatibility_fallback() {
        let name = "challenge-0123456789abcdef";
        let legacy = metadata(
            name,
            BTreeMap::from([(APP_LABEL.to_string(), "rsctf-0123456789abcdef".to_string())]),
        );
        assert!(owned_identity(&legacy, name, "scope"));

        let mut marked = legacy.clone();
        marked
            .labels
            .as_mut()
            .unwrap()
            .insert(MANAGED_LABEL.to_string(), MANAGED_VALUE.to_string());
        assert!(!owned_identity(&marked, name, "scope"));
    }

    #[test]
    fn discovery_unions_all_resource_kinds_and_deduplicates_names() {
        let scope = workload_scope("challenges", Some("control"));
        let first = "first-0123456789abcdef";
        let second = "second-fedcba9876543210";
        let first_labels =
            workload_labels(first, "0123456789abcdef", &scope, None, "launch-spec-a");
        let second_labels =
            workload_labels(second, "fedcba9876543210", &scope, None, "launch-spec-b");
        let mut names = BTreeSet::new();

        collect_names(
            "pods",
            Ok(object_list(vec![Pod {
                metadata: metadata(first, first_labels.clone()),
                ..Default::default()
            }])),
            &scope,
            &mut names,
        );
        collect_names(
            "services",
            Ok(object_list(vec![Service {
                metadata: metadata(first, first_labels),
                ..Default::default()
            }])),
            &scope,
            &mut names,
        );
        collect_names(
            "network policies",
            Ok(object_list(vec![NetworkPolicy {
                metadata: metadata(second, second_labels),
                ..Default::default()
            }])),
            &scope,
            &mut names,
        );

        assert_eq!(names.into_iter().collect::<Vec<_>>(), vec![first, second]);
    }
}
