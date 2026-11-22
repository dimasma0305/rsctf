use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{Service, ServiceSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::PostParams;

use super::{challenge_service_ports, team_claim::TeamClaim, KubernetesContainerManager};
use crate::services::container::TeamStorage;
use crate::utils::error::{AppError, AppResult};

pub(super) const CLAIM_SELECTOR: &str = "rsctf.io/team-claim";
const FRONT_PORT: &str = "rsctf.io/service-port";

pub(super) struct TeamEndpoint {
    pub uid: String,
    pub ip: String,
    pub port: i32,
}

impl TeamEndpoint {
    pub(super) async fn wait_routed(
        &self,
        manager: &KubernetesContainerManager,
        service: &str,
        pod: &str,
        pod_uid: &str,
        target_port: i32,
    ) -> AppResult<()> {
        use k8s_openapi::api::discovery::v1::EndpointSlice;
        use kube::{api::ListParams, Api};
        use std::time::Duration;
        let slices: Api<EndpointSlice> =
            Api::namespaced(manager.client.clone(), &manager.namespace);
        let params = ListParams::default()
            .labels(&format!("kubernetes.io/service-name={service}"))
            .limit(8);
        let family = if self.ip.parse::<std::net::Ipv4Addr>().is_ok() {
            "IPv4"
        } else {
            "IPv6"
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let list = slices.list(&params).await.map_err(|e| {
                    AppError::unavailable(format!(
                        "Could not verify the team's Service routing: {e}"
                    ))
                })?;
                if list.metadata.continue_.is_some() {
                    return Err(AppError::conflict(
                        "The team's private Service has too many endpoint slices",
                    ));
                }
                let mut ready = false;
                let mut stale = false;
                for slice in list.items.iter().filter(|s| s.address_type == family) {
                    let owned = slice
                        .metadata
                        .owner_references
                        .iter()
                        .flatten()
                        .any(|o| o.kind == "Service" && o.name == service && o.uid == self.uid);
                    let ports = slice.ports.as_deref().unwrap_or_default();
                    let ports_match = ["TCP", "UDP"].iter().all(|protocol| {
                        ports.iter().any(|p| {
                            p.protocol.as_deref() == Some(protocol) && p.port == Some(target_port)
                        })
                    });
                    for e in &slice.endpoints {
                        if e.conditions.as_ref().and_then(|c| c.ready) == Some(false) {
                            continue;
                        }
                        let correct = owned
                            && ports_match
                            && !e.addresses.is_empty()
                            && e.target_ref.as_ref().is_some_and(|r| {
                                r.kind.as_deref() == Some("Pod")
                                    && r.name.as_deref() == Some(pod)
                                    && r.uid.as_deref() == Some(pod_uid)
                            });
                        ready |= correct;
                        stale |= !correct;
                    }
                }
                if ready && !stale {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .map_err(|_| {
            AppError::unavailable(
                "The team's stable Service has not routed to its ready replacement Pod",
            )
        })?
    }
}

impl KubernetesContainerManager {
    /// Keep the ClusterIP with the team's allocation while revision-specific Pods come and go.
    pub(super) async fn team_endpoint(
        &self,
        data: &TeamStorage,
        claim: &TeamClaim,
        initial_port: Option<i32>,
    ) -> AppResult<TeamEndpoint> {
        let services = self.services();
        let name = &claim.name;
        let unavailable = |error: kube::Error| {
            AppError::unavailable(format!("Kubernetes team endpoint: {error}"))
        };
        let selector = BTreeMap::from([(CLAIM_SELECTOR.into(), claim.uid.clone())]);
        let owner = OwnerReference {
            api_version: "v1".into(),
            kind: "PersistentVolumeClaim".into(),
            name: name.clone(),
            uid: claim.uid.clone(),
            ..Default::default()
        };
        let service = match services.get_opt(name).await.map_err(unavailable)? {
            Some(service) => service,
            None => {
                let port = initial_port
                    .filter(|p| (1..=65535).contains(p))
                    .filter(|_| data.endpoint_uid.is_none())
                    .ok_or_else(|| {
                        AppError::conflict("The team's registered Service is missing")
                    })?;
                let mut ports = challenge_service_ports(port, true);
                for entry in &mut ports {
                    entry.target_port = entry.name.clone().map(IntOrString::String);
                }
                let request = Service {
                    metadata: ObjectMeta {
                        name: Some(name.clone()),
                        labels: Some(data.labels(&self.scope)),
                        annotations: Some(BTreeMap::from([(FRONT_PORT.into(), port.to_string())])),
                        owner_references: Some(vec![owner.clone()]),
                        ..Default::default()
                    },
                    spec: Some(ServiceSpec {
                        type_: Some("ClusterIP".into()),
                        selector: Some(selector.clone()),
                        ports: Some(ports),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                match services.create(&PostParams::default(), &request).await {
                    Ok(service) => service,
                    Err(error) if super::is_conflict(&error) => {
                        services.get(name).await.map_err(unavailable)?
                    }
                    Err(error) => return Err(unavailable(error)),
                }
            }
        };
        let conflict = || {
            AppError::conflict(
                "The Service no longer matches this team's registered private endpoint",
            )
        };
        let meta = &service.metadata;
        let uid = meta
            .uid
            .as_ref()
            .filter(|uid| !uid.is_empty())
            .ok_or_else(conflict)?;
        let port = meta
            .annotations
            .as_ref()
            .and_then(|a| a.get(FRONT_PORT))
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|p| (1..=65535).contains(p))
            .ok_or_else(conflict)?;
        let spec = service.spec.as_ref().ok_or_else(conflict)?;
        let ports = spec.ports.as_deref().unwrap_or_default();
        let ports_match = ports.len() == 2
            && challenge_service_ports(port, true).iter().all(|expected| {
                ports.iter().any(|actual| {
                    actual.name == expected.name
                        && actual.port == port
                        && actual.protocol == expected.protocol
                        && actual.target_port == actual.name.clone().map(IntOrString::String)
                        && actual.node_port.is_none()
                })
            });
        if meta.deletion_timestamp.is_some()
            || data
                .endpoint_uid
                .as_ref()
                .is_some_and(|expected| expected != uid)
            || meta.owner_references.as_deref() != Some(std::slice::from_ref(&owner))
            || !data
                .labels(&self.scope)
                .iter()
                .all(|(k, v)| meta.labels.as_ref().and_then(|l| l.get(k)) == Some(v))
            || spec.selector.as_ref() != Some(&selector)
            || spec.type_.as_deref() != Some("ClusterIP")
            || !spec.external_ips.as_ref().is_none_or(Vec::is_empty)
            || spec.external_name.is_some()
            || spec.publish_not_ready_addresses == Some(true)
            || !ports_match
        {
            return Err(conflict());
        }
        let ip = spec
            .cluster_ip
            .as_ref()
            .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
            .ok_or_else(conflict)?;
        Ok(TeamEndpoint {
            uid: uid.clone(),
            ip: ip.clone(),
            port,
        })
    }
}
