use std::path::{Path, PathBuf};

use rsctf_worker_protocol::{
    CommandErrorCode, EnsureWorkload, FlagTarget, WorkloadFence, WriteFlag,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::state_file::{io_error, load, remove, save};
use crate::enroll::sync_parent_directory;
use crate::runtime::RuntimeError;

// The control dispatcher serializes commands per workload, across reconnects.
// Keep one record per live workload in the exclusively owned agent state.
pub(super) struct FlagDeliveryStore {
    directory: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct Registration {
    fence: WorkloadFence,
    spec_hash: String,
    target: FlagTarget,
    replicas: u16,
    sequence: Option<Sequence>,
}

#[derive(Deserialize, Serialize)]
struct Sequence {
    number: u64,
    digest: [u8; 32],
}

impl FlagDeliveryStore {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            directory: state_dir.to_owned(),
        }
    }

    fn path(&self, workload: Uuid) -> PathBuf {
        self.directory
            .join(format!("flag-delivery-{workload}.json"))
    }

    pub async fn register(&self, command: &EnsureWorkload) -> Result<(), RuntimeError> {
        let fence = command.fence;
        let path = self.path(fence.workload_id);
        let existing = load::<Registration>(&path).await?;
        if let Some(old) = &existing {
            if old.fence.assignment_id == fence.assignment_id
                && old.fence.generation > fence.generation
            {
                return Err(RuntimeError::new(
                    CommandErrorCode::StaleGeneration,
                    "newer flag target already registered",
                ));
            }
            if old.fence == fence {
                if old.spec_hash == command.spec_hash
                    && Some(&old.target) == command.spec.flag_target.as_ref()
                {
                    return Ok(());
                }
                return Err(RuntimeError::new(
                    CommandErrorCode::SpecConflict,
                    "flag target changed within its workload generation",
                ));
            }
        }
        let Some(target) = &command.spec.flag_target else {
            if existing.is_some() {
                remove(&path).await?;
            }
            return Ok(());
        };
        let service = command
            .spec
            .services
            .iter()
            .find(|s| s.name == target.service)
            .ok_or_else(|| {
                RuntimeError::new(
                    CommandErrorCode::InvalidSpec,
                    "flag service missing from workload",
                )
            })?;
        tokio::fs::create_dir_all(&self.directory)
            .await
            .map_err(io_error)?;
        save(
            &path,
            &Registration {
                fence,
                spec_hash: command.spec_hash.clone(),
                target: target.clone(),
                replicas: service.replicas,
                sequence: None,
            },
        )
        .await
    }

    /// Persist the new sequence before the first replica is touched. A retry
    /// of the same payload performs delivery and readback again, including
    /// after a partial write or an agent restart.
    pub async fn begin(&self, command: &WriteFlag) -> Result<u16, RuntimeError> {
        let path = self.path(command.fence.workload_id);
        let mut state = load::<Registration>(&path).await?.ok_or_else(|| {
            RuntimeError::new(
                CommandErrorCode::NotFound,
                "reconcile the workload's declared flag target before delivery",
            )
        })?;
        if state.fence.assignment_id != command.fence.assignment_id {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleAssignment,
                "flag target belongs to another assignment",
            ));
        }
        if state.fence != command.fence {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleGeneration,
                "flag target belongs to another generation",
            ));
        }
        if state.target != command.target {
            return Err(RuntimeError::new(
                CommandErrorCode::InvalidSpec,
                "undeclared workload flag target",
            ));
        }
        let digest: [u8; 32] = Sha256::digest(command.value.as_bytes()).into();
        if let Some(previous) = &state.sequence {
            if command.flag_sequence < previous.number {
                return Err(RuntimeError::new(
                    CommandErrorCode::StaleFlagSequence,
                    "a newer flag sequence was requested",
                ));
            }
            if command.flag_sequence == previous.number {
                if previous.digest != digest {
                    return Err(RuntimeError::new(
                        CommandErrorCode::SpecConflict,
                        "flag sequence has a different payload",
                    ));
                }
                sync_parent_directory(&path).await.map_err(io_error)?;
                return Ok(state.replicas);
            }
        }
        state.sequence = Some(Sequence {
            number: command.flag_sequence,
            digest,
        });
        save(&path, &state).await?;
        Ok(state.replicas)
    }

    pub async fn forget(&self, fence: WorkloadFence) -> Result<(), RuntimeError> {
        let path = self.path(fence.workload_id);
        if let Some(state) = load::<Registration>(&path).await? {
            if state.fence.assignment_id == fence.assignment_id
                && state.fence.generation <= fence.generation
            {
                remove(&path).await?;
            }
        }
        // An interruption before the first registration rename can leave only
        // its unpublished temporary file behind.
        remove(&path.with_extension("tmp")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn partial_delivery_cannot_be_replaced_by_an_older_round_after_restart() {
        let directory = std::env::temp_dir().join(format!("rsctf-flag-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let store = FlagDeliveryStore::new(&directory);
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 3,
        };
        let target = FlagTarget {
            service: "challenge".into(),
            path: "/tmp/flag".into(),
        };
        let path = store.path(fence.workload_id);
        save(
            &path,
            &Registration {
                fence,
                target: target.clone(),
                replicas: 2,
                spec_hash: "a".repeat(64),
                sequence: None,
            },
        )
        .await
        .unwrap();
        let mut command = WriteFlag {
            command_id: Uuid::new_v4(),
            fence,
            target,
            flag_sequence: 9,
            value: "example round nine".into(),
            timeout_ms: 1000,
        };
        assert_eq!(store.begin(&command).await.unwrap(), 2);
        assert!(!tokio::fs::read_to_string(&path)
            .await
            .unwrap()
            .contains(&command.value));

        let restarted = FlagDeliveryStore::new(&directory);
        command.command_id = Uuid::new_v4();
        assert_eq!(restarted.begin(&command).await.unwrap(), 2);
        command.value = "different".into();
        assert_eq!(
            restarted.begin(&command).await.unwrap_err().code,
            CommandErrorCode::SpecConflict
        );
        command.flag_sequence -= 1;
        assert_eq!(
            restarted.begin(&command).await.unwrap_err().code,
            CommandErrorCode::StaleFlagSequence
        );
        command.flag_sequence += 2;
        command.target.path = "/tmp/other".into();
        assert_eq!(
            restarted.begin(&command).await.unwrap_err().code,
            CommandErrorCode::InvalidSpec
        );
        command.target.path = "/tmp/flag".into();
        restarted
            .forget(WorkloadFence {
                generation: 2,
                ..fence
            })
            .await
            .unwrap();
        restarted
            .forget(WorkloadFence {
                assignment_id: Uuid::new_v4(),
                ..fence
            })
            .await
            .unwrap();
        tokio::fs::write(path.with_extension("tmp"), b"unfinished")
            .await
            .unwrap();
        assert_eq!(restarted.begin(&command).await.unwrap(), 2);
        restarted.forget(fence).await.unwrap();
        assert_eq!(
            restarted.begin(&command).await.unwrap_err().code,
            CommandErrorCode::NotFound
        );
        tokio::fs::remove_dir(directory).await.unwrap();
    }
}
