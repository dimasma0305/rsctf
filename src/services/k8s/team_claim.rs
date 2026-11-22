use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, PersistentVolumeClaimVolumeSource, Pod,
    PodSecurityContext, Volume, VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{Api, PostParams};

use super::KubernetesContainerManager;
use crate::services::container::{application_data_path, TeamStorage};
use crate::utils::error::{AppError, AppResult};

const DATA: &str = "rsctf-team-data";
const PATH: &str = "rsctf.io/data-path";
const CLAIM_UID: &str = "rsctf.io/data-claim-uid";

#[cfg(test)]
mod tests;

impl TeamStorage {
    pub(super) fn claim_name(&self, scope: &str) -> String {
        let identity = format!(
            "{scope}:{}:{}:{}:{}",
            self.game_id, self.participation_id, self.challenge_id, self.allocation_id
        );
        format!(
            "rsctf-data-{}",
            &crate::utils::codec::sha256_str(&identity)[..32]
        )
    }

    fn labels(&self, scope: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("app.kubernetes.io/managed-by".into(), "rsctf".into()),
            ("rsctf.scope".into(), scope.into()),
            ("rsctf.io/game".into(), self.game_id.to_string()),
            (
                "rsctf.io/participation".into(),
                self.participation_id.to_string(),
            ),
            ("rsctf.io/challenge".into(), self.challenge_id.to_string()),
            ("rsctf.io/allocation".into(), self.allocation_id.to_string()),
        ])
    }
}

pub(super) struct TeamClaim {
    name: String,
    pub(super) uid: String,
    path: String,
}

impl KubernetesContainerManager {
    pub(super) async fn team_claim(
        &self,
        data: &TeamStorage,
        create_mib: Option<i32>,
    ) -> AppResult<TeamClaim> {
        if !application_data_path(&data.mount_path)
            || [data.game_id, data.participation_id, data.challenge_id]
                .iter()
                .any(|id| *id <= 0)
        {
            return Err(AppError::bad_request(
                "Invalid hosted team storage registration",
            ));
        }
        let api: Api<PersistentVolumeClaim> = Api::namespaced(self.client.clone(), &self.namespace);
        let name = data.claim_name(&self.scope);
        let labels = data.labels(&self.scope);
        let unavailable =
            |e: kube::Error| AppError::unavailable(format!("Kubernetes team storage: {e}"));
        let claim = match api.get_opt(&name).await.map_err(unavailable)? {
            Some(claim) => claim,
            None => {
                let mib = create_mib.ok_or_else(|| {
                    AppError::conflict("The team's existing storage claim is missing")
                })?;
                let request = PersistentVolumeClaim {
                    metadata: ObjectMeta {
                        name: Some(name.clone()),
                        labels: Some(labels.clone()),
                        annotations: Some(BTreeMap::from([(PATH.into(), data.mount_path.clone())])),
                        ..Default::default()
                    },
                    spec: Some(PersistentVolumeClaimSpec {
                        // Also fences two Pods on the same node.
                        access_modes: Some(vec!["ReadWriteOncePod".into()]),
                        volume_mode: Some("Filesystem".into()),
                        resources: Some(VolumeResourceRequirements {
                            requests: Some(BTreeMap::from([(
                                "storage".into(),
                                Quantity(format!("{mib}Mi")),
                            )])),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                match api.create(&PostParams::default(), &request).await {
                    Ok(claim) => claim,
                    Err(e) if super::is_conflict(&e) => {
                        api.get(&name).await.map_err(unavailable)?
                    }
                    Err(e) => return Err(unavailable(e)),
                }
            }
        };
        let meta = &claim.metadata;
        let owned = labels
            .iter()
            .all(|(k, v)| meta.labels.as_ref().and_then(|ls| ls.get(k)) == Some(v))
            && meta.annotations.as_ref().and_then(|a| a.get(PATH)) == Some(&data.mount_path)
            && meta.deletion_timestamp.is_none()
            && meta.owner_references.as_ref().is_none_or(Vec::is_empty)
            && claim.spec.as_ref().is_some_and(|s| {
                s.volume_mode.as_deref().unwrap_or("Filesystem") == "Filesystem"
                    && s.access_modes
                        .as_ref()
                        .is_some_and(|m| m.len() == 1 && m[0] == "ReadWriteOncePod")
                    && s.data_source.is_none()
                    && s.data_source_ref.is_none()
            });
        if !owned {
            return Err(AppError::conflict(
                "The storage claim does not match this team and challenge",
            ));
        }
        let uid = claim
            .metadata
            .uid
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                AppError::unavailable("Kubernetes did not confirm the team claim UID")
            })?;
        if data
            .claim_uid
            .as_ref()
            .is_some_and(|expected| expected != &uid)
        {
            return Err(AppError::conflict("The team storage claim was replaced"));
        }
        Ok(TeamClaim {
            name,
            uid,
            path: data.mount_path.clone(),
        })
    }
}

impl TeamClaim {
    pub(super) fn check_replacement(&self, pod: &Pod, name: &str) -> AppResult<()> {
        self.verify(pod, name)?;
        super::storage::check_remaining_volumes(pod, Some(DATA))
    }

    pub(super) async fn wait_ready(
        &self,
        pods: &Api<Pod>,
        name: &str,
        uid: &str,
        image: &str,
    ) -> AppResult<&'static str> {
        use std::time::Duration;
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let pod = pods.get(name).await.map_err(|error| {
                    AppError::unavailable(format!("Could not inspect team storage Pod: {error}"))
                })?;
                if pod.metadata.uid.as_deref() != Some(uid)
                    || !super::compat::pod_image_matches(&pod, name, image)
                {
                    return Err(AppError::conflict("Team Pod changed while waiting for storage"));
                }
                self.check_replacement(&pod, name)?;
                let status = pod.status.as_ref();
                let ready = status.and_then(|s| s.conditions.as_ref()).is_some_and(|conditions| {
                    conditions.iter().any(|c| c.type_ == "Ready" && c.status == "True")
                });
                match status.and_then(|s| s.phase.as_deref()) {
                    Some("Failed" | "Succeeded") => {
                        return Err(AppError::unavailable("The team storage Pod stopped before it became ready"));
                    }
                    Some("Running") if ready => return Ok("running"),
                    _ => tokio::time::sleep(Duration::from_secs(1)).await,
                }
            }
        })
        .await
        .map_err(|_| AppError::unavailable(
            "Team Pod is waiting for its storage or readiness. Check the claim events and ReadWriteOncePod support on the default CSI StorageClass."
        ))?
    }

    pub(super) fn mount(&self, pod: &mut Pod) {
        let spec = pod.spec.as_mut().expect("platform Pod spec");
        let container = &mut spec.containers[0];
        spec.security_context = Some(PodSecurityContext {
            fs_group: container
                .security_context
                .as_ref()
                .and_then(|s| s.run_as_group),
            fs_group_change_policy: Some("OnRootMismatch".into()),
            ..Default::default()
        });
        container.volume_mounts = Some(vec![VolumeMount {
            name: DATA.into(),
            mount_path: self.path.clone(),
            ..Default::default()
        }]);
        spec.volumes = Some(vec![Volume {
            name: DATA.into(),
            persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                claim_name: self.name.clone(),
                read_only: None,
            }),
            ..Default::default()
        }]);
        pod.metadata
            .annotations
            .get_or_insert_with(Default::default)
            .insert(CLAIM_UID.into(), self.uid.clone());
    }

    pub(super) fn verify(&self, pod: &Pod, name: &str) -> AppResult<()> {
        let spec = pod
            .spec
            .as_ref()
            .ok_or_else(|| AppError::conflict("Missing team Pod spec"))?;
        let mounted = spec
            .containers
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| c.volume_mounts.as_ref())
            .is_some_and(|mounts| {
                mounts.iter().any(|m| {
                    m.name == DATA
                        && m.mount_path == self.path
                        && !m.read_only.unwrap_or(false)
                        && m.sub_path.is_none()
                        && m.sub_path_expr.is_none()
                })
            });
        let volume = spec
            .volumes
            .iter()
            .flatten()
            .find(|v| v.name == DATA)
            .and_then(|v| v.persistent_volume_claim.as_ref());
        if !mounted
            || !volume.is_some_and(|v| v.claim_name == self.name && !v.read_only.unwrap_or(false))
            || pod
                .metadata
                .annotations
                .as_ref()
                .and_then(|a| a.get(CLAIM_UID))
                != Some(&self.uid)
        {
            return Err(AppError::conflict(
                "The Pod does not mount its registered team storage claim",
            ));
        }
        Ok(())
    }
}
