use bollard::models::{
    ContainerStateStatusEnum, MountPoint, MountPointTypeEnum, RestartPolicyNameEnum,
};

use super::super::{AdVolumeChange, ContainerSpec, DockerContainerManager, TEAM_ENV};
use crate::services::docker_admission::docker_admission;
use crate::utils::error::{AppError, AppResult};

use super::super::application_data_path as application_data;

// An unfamiliar mount still carries team data and must reach validation.
fn has_persistent_mount(mounts: &[MountPoint]) -> bool {
    mounts
        .iter()
        .any(|m| m.typ != Some(MountPointTypeEnum::TMPFS))
}

impl DockerContainerManager {
    pub(in crate::services::container) async fn inspect_ad_data_volumes(
        &self,
        id: &str,
    ) -> AppResult<bool> {
        let old = self.inspect_scoped_container(self.client()?, id).await?;
        Ok(old
            .and_then(|c| c.mounts)
            .is_some_and(|mounts| has_persistent_mount(&mounts)))
    }

    /// Docker inherits every donor mount. Both images must declare exactly
    /// these anonymous application volumes before a reset may stop the donor.
    pub(in crate::services::container) async fn ad_volume_repair_spec(
        &self,
        previous: &str,
        spec: &ContainerSpec,
        change: AdVolumeChange,
    ) -> AppResult<Option<(String, ContainerSpec, bool)>> {
        let docker = self.client()?;
        let Some(old) = self.inspect_scoped_container(docker, previous).await? else {
            return Ok(None);
        };
        let mounts = old.mounts.as_deref().unwrap_or_default();
        if !has_persistent_mount(mounts) {
            return Ok(None);
        }
        let mismatch = || {
            AppError::conflict(
                "Cannot verify the team's application volumes for the selected image",
            )
        };
        let image = self
            .inspect_image(&spec.image)
            .await?
            .map_err(|_| mismatch())?;
        let declared = image
            .config
            .as_ref()
            .and_then(|c| c.volumes.as_ref())
            .ok_or_else(mismatch)?;
        let config = old.config.as_ref().ok_or_else(mismatch)?;
        let old_volumes = config.volumes.as_ref().ok_or_else(mismatch)?;
        let running = match old.state.as_ref().and_then(|s| s.status) {
            Some(ContainerStateStatusEnum::EXITED) => false,
            Some(ContainerStateStatusEnum::RUNNING) if change == AdVolumeChange::Reset => true,
            _ => return Err(mismatch()),
        };
        let host = old.host_config.as_ref().ok_or_else(mismatch)?;
        let team = spec
            .env
            .iter()
            .find(|(k, _)| k == TEAM_ENV)
            .ok_or_else(mismatch)?;
        let team_env = format!("{TEAM_ENV}={}", team.1);
        let id = old.id.as_deref().ok_or_else(mismatch)?;
        let parent_is_verified = match host.volumes_from.as_deref().unwrap_or_default() {
            [] => true,
            [entry] => entry.strip_suffix(":rw").is_some_and(|parent| {
                parent.len() == 64
                    && parent
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    && config
                        .labels
                        .as_ref()
                        .and_then(|labels| labels.get(super::super::OPERATION_LABEL))
                        .is_some_and(|op| op == &format!("ad-volume-repair:{parent}"))
            }),
            _ => false,
        };
        if !parent_is_verified
            || !matches!(
                host.restart_policy.as_ref().and_then(|policy| policy.name),
                None | Some(RestartPolicyNameEnum::NO | RestartPolicyNameEnum::EMPTY)
            )
            || spec.game_kind != rsctf_worker_protocol::GameKind::AttackDefense
            || id != previous
            || old.image.is_none()
            || image.id.is_none()
            || (change == AdVolumeChange::RepairStopped && old.image != image.id)
            || host.auto_remove == Some(true)
            || old_volumes != declared
            || !config
                .env
                .as_ref()
                .is_some_and(|env| env.contains(&team_env))
            || !config
                .labels
                .as_ref()
                .is_some_and(|labels| labels.contains_key(super::LAUNCH_SPEC_LABEL))
            || host.binds.as_ref().is_some_and(|v| !v.is_empty())
            || host.mounts.as_ref().is_some_and(|v| !v.is_empty())
            || host.tmpfs.as_ref().is_some_and(|v| !v.is_empty())
            || mounts.len() != declared.len()
            || mounts.len() > 16
        {
            return Err(mismatch());
        }
        for mount in mounts {
            let path = mount.destination.as_deref().ok_or_else(mismatch)?;
            let name = mount.name.as_deref().ok_or_else(mismatch)?;
            if !application_data(path)
                || !declared.contains_key(path)
                || mount.typ != Some(MountPointTypeEnum::VOLUME)
                || mount.rw != Some(true)
                || mount.driver.as_deref() != Some("local")
                || name.len() != 64
                || !name.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err(mismatch());
            }
            let volume = docker_admission()
                .read("inspect_volume", docker.inspect_volume(name))
                .await?
                .map_err(|_| mismatch())?;
            if volume.driver != "local"
                || !volume.options.is_empty()
                || !volume.labels.contains_key("com.docker.volume.anonymous")
            {
                return Err(mismatch());
            }
        }
        let mut repair = spec.clone();
        // A reclaimed repair must find the same successor even after a lost
        // create response. Its launch fingerprint fences a changed round.
        repair.operation_id = Some(format!("ad-volume-repair:{id}"));
        Ok(Some((id.to_owned(), repair, running)))
    }

    pub(in crate::services::container) async fn prepare_volume_launch(
        &self,
        id: &str,
        spec: &ContainerSpec,
        change: AdVolumeChange,
    ) -> AppResult<Option<(String, ContainerSpec)>> {
        let Some((id, repair, running)) = self.ad_volume_repair_spec(id, spec, change).await?
        else {
            return Ok(None);
        };
        if running {
            let docker = self.client()?;
            let stopped = docker_admission()
                .lifecycle(
                    "stop_container",
                    docker.stop_container(
                        &id,
                        Some(bollard::container::StopContainerOptions { t: 10 }),
                    ),
                )
                .await?;
            let state = self
                .inspect_scoped_container(docker, &id)
                .await?
                .and_then(|c| c.state)
                .and_then(|s| s.status);
            if state != Some(ContainerStateStatusEnum::EXITED) {
                if let Err(error) = stopped {
                    tracing::warn!(backend_id=%id,%error,"could not stop team data volume writer");
                }
                return Err(AppError::conflict(
                    "The team data volume still has an active writer",
                ));
            }
        }
        Ok(Some((id, repair)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_validates_even_an_unrecognized_persistent_mount() {
        let mut mount = MountPoint {
            destination: Some("/private".into()),
            typ: Some(MountPointTypeEnum::TMPFS),
            ..Default::default()
        };
        assert!(!has_persistent_mount(std::slice::from_ref(&mount)));
        mount.typ = Some(MountPointTypeEnum::VOLUME);
        assert!(has_persistent_mount(std::slice::from_ref(&mount)));
        mount.typ = None;
        assert!(has_persistent_mount(&[mount]));
    }
}
