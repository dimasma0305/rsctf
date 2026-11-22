use std::collections::BTreeMap;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Container, Event, Pod, PodSpec, ResourceRequirements};
use k8s_openapi::apimachinery::pkg::{
    api::resource::Quantity,
    apis::meta::v1::{ObjectMeta, OwnerReference},
};
use kube::{
    api::{DeleteParams, ListParams, PostParams, Preconditions},
    Api,
};

use super::{challenge_security_context, KubernetesContainerManager};
use crate::services::container::ContainerSpec;
use crate::utils::error::{AppError, AppResult};

const PROBE: &str = "pull";

impl KubernetesContainerManager {
    pub(super) async fn stage_team_image(
        &self,
        donor: &str,
        spec: &ContainerSpec,
    ) -> AppResult<()> {
        self.validate_image_target(&spec.image).await?;
        let data = spec.team_storage.as_ref().ok_or_else(|| {
            AppError::conflict("Kubernetes ZIP deployment requires a registered team endpoint")
        })?;
        let claim = self.team_claim(data, None).await?;
        let pods = self.pods();
        let pod = pods.get(donor).await.map_err(api_error)?;
        if !super::orphans::owned_identity(&pod.metadata, donor, &self.scope)
            || pod.metadata.deletion_timestamp.is_some()
        {
            return Err(AppError::conflict(
                "The team Pod changed before image staging",
            ));
        }
        claim.check_replacement(&pod, donor)?;
        let source = pod
            .spec
            .as_ref()
            .ok_or_else(|| AppError::conflict("The team Pod has no spec"))?;
        let donor_uid = pod
            .metadata
            .uid
            .as_deref()
            .ok_or_else(|| AppError::conflict("The team Pod has no UID"))?;
        let node = source
            .node_name
            .as_ref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| AppError::conflict("The team Pod is not scheduled"))?;
        let operation = spec
            .operation_id
            .as_deref()
            .ok_or_else(|| AppError::conflict("Image staging requires an operation identity"))?;
        let name = format!(
            "rsctf-pull-{}",
            &crate::utils::codec::sha256_str(&format!(
                "{}:{donor_uid}:{operation}:{}",
                self.scope, spec.image
            ))[..32]
        );
        let owner = OwnerReference {
            api_version: "v1".into(),
            kind: "Pod".into(),
            name: donor.into(),
            uid: donor_uid.into(),
            ..Default::default()
        };
        let limits = BTreeMap::from([
            ("memory".into(), Quantity("16Mi".into())),
            ("cpu".into(), Quantity("10m".into())),
        ]);
        // Linux cannot execute its root directory. The kubelet pulls this digest but
        // cannot start its entrypoint. The probe has no team data, flag or Service.
        let request = Pod {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                owner_references: Some(vec![owner.clone()]),
                ..Default::default()
            },
            spec: Some(PodSpec {
                node_name: Some(node.clone()),
                runtime_class_name: source.runtime_class_name.clone(),
                service_account_name: source.service_account_name.clone(),
                image_pull_secrets: source.image_pull_secrets.clone(),
                automount_service_account_token: Some(false),
                restart_policy: Some("Never".into()),
                active_deadline_seconds: Some(90),
                containers: vec![Container {
                    name: PROBE.into(),
                    image: Some(spec.image.clone()),
                    command: Some(vec!["/".into()]),
                    image_pull_policy: Some("Always".into()),
                    security_context: Some(challenge_security_context()),
                    resources: Some(ResourceRequirements {
                        limits: Some(limits),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let probe = match pods.create(&PostParams::default(), &request).await {
            Ok(pod) => pod,
            Err(e) if super::is_conflict(&e) => pods.get(&name).await.map_err(api_error)?,
            Err(e) => return Err(api_error(e)),
        };
        let matches = probe.spec.as_ref().is_some_and(|s| {
            s.node_name.as_ref() == Some(node)
                && s.containers.iter().any(|c| {
                    c.name == PROBE
                        && c.image.as_deref() == Some(&spec.image)
                        && c.command
                            .as_ref()
                            .is_some_and(|args| args.len() == 1 && args[0] == "/")
                        && c.image_pull_policy.as_deref() == Some("Always")
                })
        });
        if !matches
            || probe.metadata.owner_references.as_deref() != Some(std::slice::from_ref(&owner))
        {
            return Err(AppError::conflict(
                "A different Pod owns the image staging operation",
            ));
        }
        let uid = probe
            .metadata
            .uid
            .ok_or_else(|| AppError::unavailable("No UID for image staging Pod"))?;
        let events: Api<Event> = Api::namespaced(self.client.clone(), &self.namespace);
        let wait = tokio::time::timeout(
            Duration::from_secs(60),
            wait_for_pull(&events, &pods, &name, &uid),
        )
        .await
        .map_err(|_| {
            AppError::unavailable(
                "Timed out pulling the patch image; the current service is still active",
            )
        });
        let delete = DeleteParams {
            grace_period_seconds: Some(0),
            preconditions: Some(Preconditions {
                uid: Some(uid),
                ..Default::default()
            }),
            ..Default::default()
        };
        match pods.delete(&name, &delete).await {
            Ok(_) => {}
            Err(kube::Error::Api(e)) if e.code == 404 => {}
            Err(e) => return Err(api_error(e)),
        }
        wait?
    }
}

fn api_error(error: kube::Error) -> AppError {
    AppError::unavailable(format!("Kubernetes image staging: {error}"))
}

async fn wait_for_pull(
    events: &Api<Event>,
    pods: &Api<Pod>,
    name: &str,
    uid: &str,
) -> AppResult<()> {
    let params = ListParams::default()
        .fields(&format!("involvedObject.uid={uid},reason=Pulled"))
        .limit(8);
    loop {
        let pod = pods.get(name).await.map_err(api_error)?;
        if pod.metadata.uid.as_deref() != Some(uid) || pod.metadata.deletion_timestamp.is_some() {
            return Err(AppError::conflict("The image staging Pod was replaced"));
        }
        if events
            .list(&params)
            .await
            .map_err(api_error)?
            .items
            .iter()
            .any(|e| {
                e.involved_object.uid.as_deref() == Some(uid)
                    && e.type_.as_deref() == Some("Normal")
                    && e.involved_object.field_path.as_deref() == Some("spec.containers{pull}")
                    && e.reason.as_deref() == Some("Pulled")
            })
        {
            return Ok(());
        }
        let failed = pod
            .status
            .as_ref()
            .and_then(|s| s.container_statuses.as_ref())
            .and_then(|cs| cs.iter().find(|c| c.name == PROBE))
            .and_then(|c| c.state.as_ref()?.waiting.as_ref()?.reason.as_deref());
        if matches!(
            failed,
            Some("ErrImagePull" | "ImagePullBackOff" | "InvalidImageName")
        ) {
            return Err(AppError::unavailable("The cluster could not pull the selected image; the current service is still active"));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
