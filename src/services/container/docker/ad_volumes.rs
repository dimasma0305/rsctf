use bollard::models::{ContainerStateStatusEnum, MountPointTypeEnum, RestartPolicyNameEnum};

use super::super::{ContainerSpec, DockerContainerManager, TEAM_ENV};
use crate::services::docker_admission::docker_admission;
use crate::utils::error::{AppError, AppResult};

fn application_data(path: &str) -> bool {
    path == "/data"
        || path.strip_prefix("/var/lib/").is_some_and(|component| {
            !component.is_empty()
                && component.len() <= 128
                && component
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        })
}

impl DockerContainerManager {
    pub(in crate::services::container) async fn inspect_ad_data_volumes(
        &self,
        id: &str,
    ) -> AppResult<bool> {
        let old = self.inspect_scoped_container(self.client()?, id).await?;
        Ok(old.and_then(|c| c.mounts).is_some_and(|mounts| {
            mounts
                .iter()
                .any(|m| m.destination.as_deref().is_some_and(application_data))
        }))
    }

    /// Docker inherits every donor mount. Accept only a stopped, same-image
    /// service whose entire mount set consists of its own anonymous volumes.
    pub(in crate::services::container) async fn ad_volume_repair_spec(
        &self,
        previous: &str,
        spec: &ContainerSpec,
    ) -> AppResult<Option<(String, ContainerSpec)>> {
        let docker = self.client()?;
        let Some(old) = self.inspect_scoped_container(docker, previous).await? else {
            return Ok(None);
        };
        let mounts = old.mounts.as_deref().unwrap_or_default();
        if !mounts
            .iter()
            .any(|m| m.destination.as_deref().is_some_and(application_data))
        {
            return Ok(None);
        }
        let mismatch = || {
            AppError::conflict(
                "Cannot verify the stopped service's application volumes; keeping its data",
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
            || old.image != image.id
            || old.state.as_ref().and_then(|s| s.status) != Some(ContainerStateStatusEnum::EXITED)
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
        Ok(Some((id.to_owned(), repair)))
    }
}
