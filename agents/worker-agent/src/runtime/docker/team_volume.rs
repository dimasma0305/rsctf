//! One local Docker volume per registered A&D assignment. Persist its identity
//! before allocating it so a retried command can finish the same operation.

use std::collections::HashMap;
use std::path::PathBuf;

use bollard::models::{Mount, MountTypeEnum, MountVolumeOptions};
use bollard::volume::{CreateVolumeOptions, RemoveVolumeOptions};
use rsctf_worker_protocol::{CommandErrorCode, EnsureAbsent, EnsureWorkload, WorkloadFence};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    docker_error, is_not_found, state_file, DockerRuntime, LABEL_ASSIGNMENT, LABEL_WORKER,
    LABEL_WORKLOAD,
};
use crate::runtime::RuntimeError;

pub(super) const LABEL_ALLOCATION: &str = "io.rsctf.worker.team-volume";

#[derive(Serialize, Deserialize)]
pub(super) struct TeamVolume {
    fence: WorkloadFence,
    spec_hash: String,
    allocation: Uuid,
    path: String,
    first_generation: u64,
    provisioned: bool,
}

fn conflict(message: &'static str) -> RuntimeError {
    RuntimeError::new(CommandErrorCode::SpecConflict, message)
}

impl TeamVolume {
    fn name(&self) -> String {
        format!(
            "rsctf-team-{}-{}",
            self.fence.workload_id.simple(),
            self.fence.assignment_id.simple()
        )
    }

    fn labels(&self, worker: Uuid) -> HashMap<String, String> {
        HashMap::from([
            (LABEL_ALLOCATION.to_owned(), self.allocation.to_string()),
            (LABEL_WORKER.to_owned(), worker.to_string()),
            (
                LABEL_WORKLOAD.to_owned(),
                self.fence.workload_id.to_string(),
            ),
            (
                LABEL_ASSIGNMENT.to_owned(),
                self.fence.assignment_id.to_string(),
            ),
        ])
    }

    pub(super) fn mount(&self) -> Mount {
        Mount {
            typ: Some(MountTypeEnum::VOLUME),
            source: Some(self.name()),
            target: Some(self.path.clone()),
            volume_options: Some(MountVolumeOptions {
                no_copy: Some(self.first_generation != self.fence.generation),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn check_fence(&self, fence: WorkloadFence, hash: &str) -> Result<(), RuntimeError> {
        if self.fence.workload_id != fence.workload_id
            || self.fence.assignment_id != fence.assignment_id
        {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleAssignment,
                "team volume belongs to another assignment",
            ));
        }
        if fence.generation < self.fence.generation {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleGeneration,
                "team volume belongs to a newer generation",
            ));
        }
        if fence.generation == self.fence.generation && hash != self.spec_hash {
            return Err(conflict("team volume has a different spec hash"));
        }
        Ok(())
    }
}

impl DockerRuntime {
    fn volume_record(&self, workload_id: Uuid) -> PathBuf {
        self.state_dir
            .join(format!("team-volume-{workload_id}.json"))
    }

    async fn volume_exists(&self, claim: &TeamVolume) -> Result<bool, RuntimeError> {
        let name = claim.name();
        let volume = match self
            .admission
            .read("inspect_volume", self.docker.inspect_volume(&name))
            .await?
        {
            Ok(volume) => volume,
            Err(error) if is_not_found(&error) => return Ok(false),
            Err(error) => return Err(docker_error("inspect team volume", error)),
        };
        if volume.name != name
            || volume.driver != "local"
            || !volume.options.is_empty()
            || !claim
                .labels(self.worker_id)
                .iter()
                .all(|(key, value)| volume.labels.get(key) == Some(value))
        {
            return Err(conflict(
                "Docker volume no longer matches its registered identity",
            ));
        }
        Ok(true)
    }

    pub(super) async fn verify_team_volume(&self, claim: &TeamVolume) -> Result<(), RuntimeError> {
        if !self.volume_exists(claim).await? {
            return Err(conflict("registered team volume is missing"));
        }
        Ok(())
    }

    pub(super) async fn prepare_team_volume(
        &self,
        command: &EnsureWorkload,
        has_existing_containers: bool,
    ) -> Result<Option<TeamVolume>, RuntimeError> {
        let record = self.volume_record(command.fence.workload_id);
        let existing = state_file::load::<TeamVolume>(&record).await?;
        let Some(path) = command.spec.team_data_mount.as_ref() else {
            if existing.is_some() {
                return Err(conflict(
                    "cannot drop registered team storage during a workload update",
                ));
            }
            return Ok(None);
        };
        if !self.allow_unbounded_storage {
            return Err(RuntimeError::unsupported(
                "local Docker volumes cannot enforce a per-team disk quota on this worker",
            ));
        }
        if let Some(claim) = &existing {
            claim.check_fence(command.fence, &command.spec_hash)?;
            if &claim.path != path {
                return Err(conflict("cannot move the registered team data directory"));
            }
            if claim.provisioned && claim.fence == command.fence {
                self.verify_team_volume(claim).await?;
                return Ok(existing);
            }
        } else if has_existing_containers {
            return Err(conflict(
                "cannot replace an existing service with unregistered team storage",
            ));
        }
        self.check_free_space().await?;
        let (_, image) = self.ensure_image(&command.spec.services[0].image).await?;
        let config = image
            .config
            .ok_or_else(|| conflict("cannot verify the image's declared volumes"))?;
        if config
            .volumes
            .is_some_and(|volumes| volumes.keys().any(|directory| directory != path))
        {
            return Err(conflict(
                "the image declares a volume outside the registered team data directory",
            ));
        }
        self.check_free_space().await?;
        let mut claim = match existing {
            Some(claim) => claim,
            None => {
                let claim = TeamVolume {
                    fence: command.fence,
                    spec_hash: command.spec_hash.clone(),
                    allocation: Uuid::new_v4(),
                    path: path.clone(),
                    first_generation: command.fence.generation,
                    provisioned: false,
                };
                // The name is stable even if the state directory was lost.
                // A previous allocation under that name must never be adopted.
                self.volume_exists(&claim).await?;
                tokio::fs::create_dir_all(&self.state_dir)
                    .await
                    .map_err(state_file::io_error)?;
                state_file::save(&record, &claim).await?;
                claim
            }
        };
        if !claim.provisioned && !self.volume_exists(&claim).await? {
            self.admission
                .lifecycle(
                    "create_volume",
                    self.docker.create_volume(CreateVolumeOptions {
                        name: claim.name(),
                        driver: "local".to_owned(),
                        labels: claim.labels(self.worker_id),
                        ..Default::default()
                    }),
                )
                .await?
                .map_err(|e| docker_error("create team volume", e))?;
        }
        self.verify_team_volume(&claim).await?;
        claim.provisioned = true;
        claim.fence = command.fence;
        claim.spec_hash.clone_from(&command.spec_hash);
        state_file::save(&record, &claim).await?;
        Ok(Some(claim))
    }

    pub(super) async fn team_volume_for_removal(
        &self,
        command: &EnsureAbsent,
    ) -> Result<Option<TeamVolume>, RuntimeError> {
        let record = self.volume_record(command.fence.workload_id);
        let claim = state_file::load::<TeamVolume>(&record).await?;
        if let Some(volume) = &claim {
            // An orphan sweep for a retired assignment may share the workload UUID.
            if volume.fence.assignment_id != command.fence.assignment_id {
                return Ok(None);
            }
            volume.check_fence(command.fence, &command.spec_hash)?;
        } else {
            state_file::remove(&record.with_extension("tmp")).await?;
        }
        Ok(claim)
    }

    pub(super) async fn remove_team_volume(&self, claim: &TeamVolume) -> Result<(), RuntimeError> {
        if self.volume_exists(claim).await? {
            let removed = self
                .admission
                .lifecycle(
                    "remove_volume",
                    self.docker
                        .remove_volume(&claim.name(), Some(RemoveVolumeOptions { force: false })),
                )
                .await?;
            if let Err(e) = removed {
                if !is_not_found(&e) {
                    return Err(docker_error("remove team volume", e));
                }
            }
        }
        let record = self.volume_record(claim.fence.workload_id);
        state_file::remove(&record.with_extension("tmp")).await?;
        state_file::remove(&record).await
    }
}
