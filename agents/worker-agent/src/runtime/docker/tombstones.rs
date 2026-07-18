use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use rsctf_worker_protocol::{CommandErrorCode, WorkloadFence};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::runtime::RuntimeError;

const SUFFIX: &str = ".tombstone";
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const GC_INTERVAL: Duration = Duration::from_secs(60 * 60);
const GC_BATCH: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Tombstone {
    pub assignment_id: Uuid,
    pub generation: u64,
    pub spec_hash: String,
}

pub(super) struct TombstoneStore {
    root: PathBuf,
    state: Mutex<TombstoneState>,
}

#[derive(Default)]
struct TombstoneState {
    scan: Option<tokio::fs::ReadDir>,
    last_completed: Option<Instant>,
}

impl TombstoneStore {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join("runtime-tombstones"),
            state: Mutex::new(TombstoneState::default()),
        }
    }

    pub async fn reject_stale_present(&self, fence: WorkloadFence) -> Result<(), RuntimeError> {
        let _state = self.state.lock().await;
        let Some(latest) = self.latest_unlocked(fence.workload_id).await? else {
            return Ok(());
        };
        if latest.assignment_id != fence.assignment_id {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleAssignment,
                "a different workload assignment was already removed",
            ));
        }
        if fence.generation <= latest.generation {
            return Err(RuntimeError::new(
                CommandErrorCode::StaleGeneration,
                "this workload generation was already removed",
            ));
        }
        Ok(())
    }

    pub async fn validate_absent(
        &self,
        fence: WorkloadFence,
        spec_hash: &str,
    ) -> Result<bool, RuntimeError> {
        let _state = self.state.lock().await;
        let Some(latest) = self.latest_unlocked(fence.workload_id).await? else {
            return Ok(true);
        };
        if latest.assignment_id != fence.assignment_id {
            // Deleting is still safe because the runtime selects containers by
            // this exact assignment. Never replace the newer assignment's
            // durable tombstone with this orphan-cleanup fence.
            return Ok(false);
        }
        if fence.generation < latest.generation {
            // Likewise, a late older container may materialize after a newer
            // delete fence. Permit an exact bounded re-sweep without moving
            // the durable fence backwards.
            return Ok(false);
        }
        if fence.generation == latest.generation && latest.spec_hash != spec_hash {
            return Err(RuntimeError::new(
                CommandErrorCode::SpecConflict,
                "removed workload generation has a different spec hash",
            ));
        }
        Ok(true)
    }

    /// Persist before deleting Docker objects. A successful fsync makes a
    /// delayed EnsureWorkload unable to resurrect this generation, including
    /// after an agent restart.
    pub async fn record(&self, fence: WorkloadFence, spec_hash: &str) -> Result<(), RuntimeError> {
        let _state = self.state.lock().await;
        let directory = self.root.join(fence.workload_id.to_string());
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(io_error)?;
        let path = directory.join(format!(
            "{:020}-{}{}",
            fence.generation, fence.assignment_id, SUFFIX
        ));
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        match options.open(&path).await {
            Ok(mut file) => {
                file.write_all(spec_hash.as_bytes())
                    .await
                    .map_err(io_error)?;
                file.sync_all().await.map_err(io_error)?;
                self.compact_unlocked(&directory, fence).await
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = tokio::fs::read_to_string(&path).await.map_err(io_error)?;
                if existing == spec_hash {
                    self.compact_unlocked(&directory, fence).await
                } else {
                    Err(RuntimeError::new(
                        CommandErrorCode::SpecConflict,
                        "workload tombstone has a different spec hash",
                    ))
                }
            }
            Err(error) => Err(io_error(error)),
        }
    }

    async fn latest_unlocked(&self, workload_id: Uuid) -> Result<Option<Tombstone>, RuntimeError> {
        let directory = self.root.join(workload_id.to_string());
        let mut entries = match tokio::fs::read_dir(directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(error)),
        };
        let mut latest: Option<Tombstone> = None;
        while let Some(entry) = entries.next_entry().await.map_err(io_error)? {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(invalid_tombstone)?;
            let Some(name) = name.strip_suffix(SUFFIX) else {
                return Err(invalid_tombstone());
            };
            let (generation, assignment_id) = name.split_once('-').ok_or_else(invalid_tombstone)?;
            let generation = generation.parse::<u64>().map_err(|_| invalid_tombstone())?;
            let assignment_id = Uuid::parse_str(assignment_id).map_err(|_| invalid_tombstone())?;
            let spec_hash = tokio::fs::read_to_string(entry.path())
                .await
                .map_err(io_error)?;
            if spec_hash.len() != 64 || !spec_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(invalid_tombstone());
            }
            let candidate = Tombstone {
                assignment_id,
                generation,
                spec_hash,
            };
            if latest
                .as_ref()
                .is_some_and(|current| current.assignment_id != candidate.assignment_id)
            {
                // Workload IDs are immutable assignment identities. Conflicting
                // tombstones indicate corrupt or externally modified state; do
                // not select one by generation and risk resurrecting the other.
                return Err(invalid_tombstone());
            }
            if latest
                .as_ref()
                .is_none_or(|current| candidate.generation > current.generation)
            {
                latest = Some(candidate);
            }
        }
        Ok(latest)
    }

    async fn compact_unlocked(
        &self,
        directory: &Path,
        fence: WorkloadFence,
    ) -> Result<(), RuntimeError> {
        let mut entries = tokio::fs::read_dir(directory).await.map_err(io_error)?;
        while let Some(entry) = entries.next_entry().await.map_err(io_error)? {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(invalid_tombstone)?;
            let name = name.strip_suffix(SUFFIX).ok_or_else(invalid_tombstone)?;
            let (generation, assignment_id) = name.split_once('-').ok_or_else(invalid_tombstone)?;
            let generation = generation.parse::<u64>().map_err(|_| invalid_tombstone())?;
            let assignment_id = Uuid::parse_str(assignment_id).map_err(|_| invalid_tombstone())?;
            if assignment_id != fence.assignment_id || generation > fence.generation {
                return Err(invalid_tombstone());
            }
            if generation < fence.generation {
                tokio::fs::remove_file(entry.path())
                    .await
                    .map_err(io_error)?;
            }
        }
        Ok(())
    }

    pub async fn prune_inactive(
        &self,
        active_workloads: &HashSet<Uuid>,
    ) -> Result<(), RuntimeError> {
        let cutoff = SystemTime::now()
            .checked_sub(RETENTION)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.prune_inactive_before(active_workloads, cutoff, false)
            .await
    }

    async fn prune_inactive_before(
        &self,
        active_workloads: &HashSet<Uuid>,
        cutoff: SystemTime,
        force: bool,
    ) -> Result<(), RuntimeError> {
        let mut state = self.state.lock().await;
        if state.scan.is_none()
            && !force
            && state
                .last_completed
                .is_some_and(|completed| completed.elapsed() < GC_INTERVAL)
        {
            return Ok(());
        }
        let mut scan = match state.scan.take() {
            Some(scan) => scan,
            None => match tokio::fs::read_dir(&self.root).await {
                Ok(scan) => scan,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    state.last_completed = Some(Instant::now());
                    return Ok(());
                }
                Err(error) => return Err(io_error(error)),
            },
        };
        let mut exhausted = false;
        for _ in 0..GC_BATCH {
            let Some(entry) = scan.next_entry().await.map_err(io_error)? else {
                exhausted = true;
                break;
            };
            if !entry.file_type().await.map_err(io_error)?.is_dir() {
                continue;
            }
            let Some(workload_id) = entry
                .file_name()
                .to_str()
                .and_then(|value| Uuid::parse_str(value).ok())
            else {
                continue;
            };
            if active_workloads.contains(&workload_id) {
                continue;
            }
            let modified = entry
                .metadata()
                .await
                .map_err(io_error)?
                .modified()
                .map_err(io_error)?;
            if modified < cutoff {
                tokio::fs::remove_dir_all(entry.path())
                    .await
                    .map_err(io_error)?;
            }
        }
        if exhausted {
            state.last_completed = Some(Instant::now());
        } else {
            state.scan = Some(scan);
        }
        Ok(())
    }
}

fn io_error(error: std::io::Error) -> RuntimeError {
    RuntimeError::new(
        CommandErrorCode::Internal,
        format!("persist workload deletion fence: {error}"),
    )
}

fn invalid_tombstone() -> RuntimeError {
    RuntimeError::new(
        CommandErrorCode::Internal,
        "workload deletion fence is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deletion_fence_survives_store_recreation() {
        let root = std::env::temp_dir().join(format!("rsctf-tombstone-{}", Uuid::new_v4()));
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 4,
        };
        let hash = "a".repeat(64);
        TombstoneStore::new(&root)
            .record(fence, &hash)
            .await
            .unwrap();

        let reloaded = TombstoneStore::new(&root);
        assert!(reloaded.reject_stale_present(fence).await.is_err());
        assert!(reloaded.validate_absent(fence, &hash).await.is_ok());
        assert!(reloaded
            .reject_stale_present(WorkloadFence {
                generation: 5,
                ..fence
            })
            .await
            .is_ok());
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn newer_generation_compacts_old_files_and_inactive_history_is_pruned() {
        let root = std::env::temp_dir().join(format!("rsctf-tombstone-{}", Uuid::new_v4()));
        let store = TombstoneStore::new(&root);
        let workload_id = Uuid::new_v4();
        let assignment_id = Uuid::new_v4();
        let first = WorkloadFence {
            workload_id,
            assignment_id,
            generation: 1,
        };
        let second = WorkloadFence {
            generation: 2,
            ..first
        };
        store.record(first, &"a".repeat(64)).await.unwrap();
        store.record(second, &"b".repeat(64)).await.unwrap();

        let directory = root
            .join("runtime-tombstones")
            .join(workload_id.to_string());
        let mut entries = tokio::fs::read_dir(&directory).await.unwrap();
        assert!(entries.next_entry().await.unwrap().is_some());
        assert!(entries.next_entry().await.unwrap().is_none());
        assert!(store.reject_stale_present(second).await.is_err());

        store
            .prune_inactive_before(
                &HashSet::new(),
                SystemTime::now() + Duration::from_secs(1),
                true,
            )
            .await
            .unwrap();
        assert!(!directory.exists());
        let _ = tokio::fs::remove_dir_all(root).await;
    }

    #[tokio::test]
    async fn cleanup_keeps_active_workload_tombstones() {
        let root = std::env::temp_dir().join(format!("rsctf-tombstone-{}", Uuid::new_v4()));
        let store = TombstoneStore::new(&root);
        let fence = WorkloadFence {
            workload_id: Uuid::new_v4(),
            assignment_id: Uuid::new_v4(),
            generation: 1,
        };
        store.record(fence, &"c".repeat(64)).await.unwrap();
        store
            .prune_inactive_before(
                &HashSet::from([fence.workload_id]),
                SystemTime::now() + Duration::from_secs(1),
                true,
            )
            .await
            .unwrap();
        assert!(store.reject_stale_present(fence).await.is_err());
        let _ = tokio::fs::remove_dir_all(root).await;
    }
}
