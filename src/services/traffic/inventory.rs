//! Durable, bounded capture inventory maintained beside the capture tree.
//!
//! Live writers update the manifest when a pcap is created or rotated. Monitor
//! reads therefore touch one small file instead of walking every participation
//! directory. Missing/corrupt legacy metadata is reconciled behind a small,
//! same-key-coalesced blocking-work gate.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::UNIX_EPOCH;

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};

use crate::utils::error::{AppError, AppResult};

const INVENTORY_VERSION: u8 = 1;
const MAX_INVENTORY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CAPTURE_DIRECTORIES: usize = 16_384;
const MAX_FILES_PER_DIRECTORY: usize = 4_096;
const MAX_RECONCILE_ENTRIES: usize = 65_536;
const MAX_RECONCILE_KEYS: usize = 64;
const RECONCILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

static RECONCILE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
static RECONCILE_FLIGHTS: LazyLock<crate::utils::single_flight::SingleFlight<InventoryFill>> =
    LazyLock::new(crate::utils::single_flight::SingleFlight::new);

struct InventoryWriterGuard {
    _file: Flock<std::fs::File>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureDirectoryInventory {
    pub(crate) challenge_id: i32,
    pub(crate) participation_id: i32,
    pub(crate) directory_modified_ns: u64,
    pub(crate) files: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureInventory {
    version: u8,
    pub(crate) directories: Vec<CaptureDirectoryInventory>,
}

impl Default for CaptureInventory {
    fn default() -> Self {
        Self {
            version: INVENTORY_VERSION,
            directories: Vec::new(),
        }
    }
}

#[derive(Clone, Default)]
struct InventoryFill {
    inventory: Option<Arc<CaptureInventory>>,
    error: Option<String>,
}

fn inventory_path(root: &Path) -> PathBuf {
    root.join(".inventory").join("capture-index-v1.json")
}

fn dirty_path(root: &Path) -> PathBuf {
    root.join(".inventory").join("capture-index-v1.dirty")
}

fn mark_dirty(root: &Path) {
    let directory = root.join(".inventory");
    if std::fs::create_dir_all(&directory).is_ok() {
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dirty_path(root));
    }
}

fn acquire_writer(root: &Path) -> AppResult<InventoryWriterGuard> {
    let directory = root.join(".inventory");
    std::fs::create_dir_all(&directory)
        .map_err(|error| AppError::internal(format!("create capture inventory: {error}")))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".capture-index.lock"))
        .map_err(|error| AppError::internal(format!("open capture inventory lock: {error}")))?;
    let file = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(file) => file,
        Err((_, Errno::EAGAIN)) => {
            return Err(AppError::retryable_unavailable(
                "Capture inventory publication is busy",
                2,
            ))
        }
        Err((_, error)) => {
            return Err(AppError::internal(format!(
                "lock capture inventory: {error}"
            )))
        }
    };
    Ok(InventoryWriterGuard { _file: file })
}

fn modified_ns(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn is_regular_pcap(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_file())
        && entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pcap"))
}

fn valid_pcap_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
        && Path::new(name)
            .file_name()
            .and_then(|file| file.to_str())
            .is_some_and(|file| file == name)
        && Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pcap"))
}

fn read_inventory(root: &Path) -> AppResult<CaptureInventory> {
    if dirty_path(root).exists() {
        return Err(AppError::unavailable(
            "Capture inventory requires reconciliation",
        ));
    }
    let path = inventory_path(root);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| AppError::internal(format!("capture inventory metadata: {error}")))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_INVENTORY_BYTES
    {
        return Err(AppError::internal(
            "capture inventory is unsafe or oversized",
        ));
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| AppError::internal(format!("read capture inventory: {error}")))?;
    let inventory: CaptureInventory = serde_json::from_slice(&bytes)
        .map_err(|error| AppError::internal(format!("parse capture inventory: {error}")))?;
    let mut directory_keys = std::collections::HashSet::new();
    if inventory.version != INVENTORY_VERSION
        || inventory.directories.len() > MAX_CAPTURE_DIRECTORIES
        || inventory.directories.iter().any(|directory| {
            let mut names = std::collections::HashSet::new();
            directory.challenge_id <= 0
                || directory.participation_id <= 0
                || !directory_keys.insert((directory.challenge_id, directory.participation_id))
                || directory.files.len() > MAX_FILES_PER_DIRECTORY
                || directory
                    .files
                    .iter()
                    .any(|name| !valid_pcap_name(name) || !names.insert(name))
        })
    {
        return Err(AppError::internal(
            "capture inventory has an unsupported or unbounded shape",
        ));
    }
    Ok(inventory)
}

fn write_inventory(root: &Path, inventory: &CaptureInventory) -> AppResult<()> {
    let directory = inventory_path(root)
        .parent()
        .expect("capture inventory path has a parent")
        .to_path_buf();
    std::fs::create_dir_all(&directory)
        .map_err(|error| AppError::internal(format!("create capture inventory: {error}")))?;
    let body = serde_json::to_vec(inventory)
        .map_err(|error| AppError::internal(format!("encode capture inventory: {error}")))?;
    if body.len() as u64 > MAX_INVENTORY_BYTES {
        return Err(AppError::unavailable(
            "Capture inventory exceeded its storage bound",
        ));
    }
    let temporary = directory.join(format!(".capture-index-{}.tmp", uuid::Uuid::now_v7()));
    let result = (|| -> AppResult<()> {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| AppError::internal(format!("create capture inventory: {error}")))?;
        file.write_all(&body)
            .and_then(|_| file.sync_all())
            .map_err(|error| AppError::internal(format!("persist capture inventory: {error}")))?;
        std::fs::rename(&temporary, inventory_path(root))
            .map_err(|error| AppError::internal(format!("publish capture inventory: {error}")))?;
        std::fs::File::open(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| AppError::internal(format!("sync capture inventory: {error}")))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn scan_capture_directory(
    directory: &Path,
    challenge_id: i32,
    participation_id: i32,
    budget: &mut usize,
) -> AppResult<Option<CaptureDirectoryInventory>> {
    let metadata = match std::fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => metadata,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::internal(format!(
                "inspect capture directory: {error}"
            )))
        }
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)
        .map_err(|error| AppError::internal(format!("list capture directory: {error}")))?
    {
        *budget = budget.checked_sub(1).ok_or_else(|| {
            AppError::unavailable("Capture inventory reconciliation exceeded its I/O bound")
        })?;
        let entry = entry.map_err(|error| AppError::internal(error.to_string()))?;
        if !is_regular_pcap(&entry) {
            continue;
        }
        if files.len() >= MAX_FILES_PER_DIRECTORY {
            return Err(AppError::unavailable(
                "Capture directory exceeds the inventory file bound",
            ));
        }
        let modified = entry
            .metadata()
            .ok()
            .map(|metadata| modified_ns(&metadata))
            .unwrap_or(0);
        files.push((modified, entry.file_name().to_string_lossy().into_owned()));
    }
    if files.is_empty() {
        return Ok(None);
    }
    files.sort_unstable_by(|left, right| right.cmp(left));
    Ok(Some(CaptureDirectoryInventory {
        challenge_id,
        participation_id,
        directory_modified_ns: modified_ns(&metadata),
        files: files.into_iter().map(|(_, name)| name).collect(),
    }))
}

fn parse_numeric_directory(entry: &std::fs::DirEntry) -> Option<i32> {
    if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
        return None;
    }
    entry
        .file_name()
        .to_str()?
        .parse()
        .ok()
        .filter(|id| *id > 0)
}

fn reconcile_root(root: &Path) -> AppResult<CaptureInventory> {
    // Clear before scanning while the publication lock is held. A writer that
    // loses the lock during this scan creates the marker again, so a capture
    // the scan might have missed cannot leave the published inventory silently
    // stale.
    match std::fs::remove_file(dirty_path(root)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(AppError::internal(format!(
                "clear capture inventory dirty marker: {error}"
            )))
        }
    }
    let mut inventory = CaptureInventory::default();
    let mut budget = MAX_RECONCILE_ENTRIES;
    let challenges = match std::fs::read_dir(root) {
        Ok(challenges) => challenges,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_inventory(root, &inventory)?;
            return Ok(inventory);
        }
        Err(error) => {
            return Err(AppError::internal(format!(
                "list capture inventory root: {error}"
            )))
        }
    };
    for challenge in challenges {
        budget = budget.checked_sub(1).ok_or_else(|| {
            AppError::unavailable("Capture inventory reconciliation exceeded its I/O bound")
        })?;
        let challenge =
            challenge.map_err(|error| AppError::internal(format!("list challenge: {error}")))?;
        let Some(challenge_id) = parse_numeric_directory(&challenge) else {
            continue;
        };
        let participations = std::fs::read_dir(challenge.path())
            .map_err(|error| AppError::internal(format!("list participations: {error}")))?;
        for participation in participations {
            budget = budget.checked_sub(1).ok_or_else(|| {
                AppError::unavailable("Capture inventory reconciliation exceeded its I/O bound")
            })?;
            let participation = participation
                .map_err(|error| AppError::internal(format!("list participation: {error}")))?;
            let Some(participation_id) = parse_numeric_directory(&participation) else {
                continue;
            };
            if let Some(directory) = scan_capture_directory(
                &participation.path(),
                challenge_id,
                participation_id,
                &mut budget,
            )? {
                if inventory.directories.len() >= MAX_CAPTURE_DIRECTORIES {
                    return Err(AppError::unavailable(
                        "Capture inventory exceeds the directory bound",
                    ));
                }
                inventory.directories.push(directory);
            }
        }
    }
    inventory
        .directories
        .sort_unstable_by_key(|entry| (entry.challenge_id, entry.participation_id));
    write_inventory(root, &inventory)?;
    Ok(inventory)
}

fn root_for_capture_directory(directory: &Path) -> AppResult<(PathBuf, i32, i32)> {
    let participation_id = directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| AppError::internal("capture directory has no participation id"))?;
    let challenge = directory
        .parent()
        .ok_or_else(|| AppError::internal("capture directory has no challenge parent"))?;
    let challenge_id = challenge
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| AppError::internal("capture directory has no challenge id"))?;
    let root = challenge
        .parent()
        .ok_or_else(|| AppError::internal("capture directory has no root"))?;
    Ok((root.to_path_buf(), challenge_id, participation_id))
}

/// Refresh one directory after a writer creates/rotates a pcap or an operator
/// deletes captures. The short process lock prevents lost manifest updates.
pub(crate) fn refresh_directory_sync(directory: &Path) -> AppResult<()> {
    let (root, challenge_id, participation_id) = root_for_capture_directory(directory)?;
    let result = (|| {
        let _writer = acquire_writer(&root)?;
        let mut inventory = read_inventory(&root).or_else(|_| reconcile_root(&root))?;
        let mut budget = MAX_FILES_PER_DIRECTORY.saturating_add(16);
        let replacement =
            scan_capture_directory(directory, challenge_id, participation_id, &mut budget)?;
        inventory.directories.retain(|entry| {
            entry.challenge_id != challenge_id || entry.participation_id != participation_id
        });
        if let Some(replacement) = replacement {
            if inventory.directories.len() >= MAX_CAPTURE_DIRECTORIES {
                return Err(AppError::unavailable(
                    "Capture inventory exceeds the directory bound",
                ));
            }
            inventory.directories.push(replacement);
        }
        inventory
            .directories
            .sort_unstable_by_key(|entry| (entry.challenge_id, entry.participation_id));
        write_inventory(&root, &inventory)
    })();
    if result.is_err() {
        mark_dirty(&root);
    }
    result
}

pub(super) fn refresh_for_capture_file(path: &Path) {
    let Some(directory) = path.parent() else {
        return;
    };
    if let Err(error) = refresh_directory_sync(directory) {
        // Capture remains best-effort. A missing index is repaired by the
        // coalesced cold reconciliation path on the next monitor read.
        tracing::warn!(%error, path = %path.display(), "capture inventory update failed");
    }
}

async fn reconcile_coalesced(root: PathBuf, key: String) -> AppResult<Arc<CaptureInventory>> {
    let fill = RECONCILE_FLIGHTS
        .run_with_limit(
            &key,
            RECONCILE_TIMEOUT,
            MAX_RECONCILE_KEYS,
            move || async move {
                let Ok(_permit) = RECONCILE_SLOTS.try_acquire() else {
                    return InventoryFill {
                        error: Some("Capture inventory reconciliation is busy".into()),
                        ..Default::default()
                    };
                };
                match tokio::task::spawn_blocking(move || {
                    let _writer = acquire_writer(&root)?;
                    let result = read_inventory(&root).or_else(|_| reconcile_root(&root));
                    if result.is_err() {
                        mark_dirty(&root);
                    }
                    result
                })
                .await
                {
                    Ok(Ok(inventory)) => InventoryFill {
                        inventory: Some(Arc::new(inventory)),
                        error: None,
                    },
                    Ok(Err(error)) => InventoryFill {
                        error: Some(error.to_string()),
                        ..Default::default()
                    },
                    Err(error) => InventoryFill {
                        error: Some(format!("capture inventory task failed: {error}")),
                        ..Default::default()
                    },
                }
            },
        )
        .await;
    fill.inventory.ok_or_else(|| {
        AppError::retryable_unavailable(
            fill.error
                .as_deref()
                .unwrap_or("Capture inventory reconciliation is busy"),
            2,
        )
    })
}

pub(crate) async fn load(root: PathBuf) -> AppResult<Arc<CaptureInventory>> {
    let read_root = root.clone();
    if let Ok(Ok(inventory)) = tokio::task::spawn_blocking(move || read_inventory(&read_root)).await
    {
        return Ok(Arc::new(inventory));
    }
    let key = format!("root:{}", root.display());
    reconcile_coalesced(root, key).await
}

pub(crate) async fn load_directory(
    root: PathBuf,
    challenge_id: i32,
    participation_id: i32,
) -> AppResult<Option<CaptureDirectoryInventory>> {
    let inventory = load(root.clone()).await?;
    let current = inventory
        .directories
        .iter()
        .find(|entry| {
            entry.challenge_id == challenge_id && entry.participation_id == participation_id
        })
        .cloned();
    let directory = root
        .join(challenge_id.to_string())
        .join(participation_id.to_string());
    let modified = tokio::fs::symlink_metadata(&directory)
        .await
        .ok()
        .filter(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .map(|metadata| modified_ns(&metadata));
    if current.as_ref().map(|entry| entry.directory_modified_ns) == modified {
        return Ok(current);
    }

    let key = format!(
        "directory:{}:{challenge_id}:{participation_id}",
        root.display()
    );
    let refreshed_root = root.clone();
    let refreshed_directory = directory.clone();
    let fill = RECONCILE_FLIGHTS
        .run_with_limit(
            &key,
            RECONCILE_TIMEOUT,
            MAX_RECONCILE_KEYS,
            move || async move {
                let Ok(_permit) = RECONCILE_SLOTS.try_acquire() else {
                    return InventoryFill {
                        error: Some("Capture inventory reconciliation is busy".into()),
                        ..Default::default()
                    };
                };
                match tokio::task::spawn_blocking(move || {
                    refresh_directory_sync(&refreshed_directory)?;
                    read_inventory(&refreshed_root)
                })
                .await
                {
                    Ok(Ok(inventory)) => InventoryFill {
                        inventory: Some(Arc::new(inventory)),
                        error: None,
                    },
                    Ok(Err(error)) => InventoryFill {
                        error: Some(error.to_string()),
                        ..Default::default()
                    },
                    Err(error) => InventoryFill {
                        error: Some(format!("capture inventory task failed: {error}")),
                        ..Default::default()
                    },
                }
            },
        )
        .await;
    let inventory = fill.inventory.ok_or_else(|| {
        AppError::retryable_unavailable(
            fill.error
                .as_deref()
                .unwrap_or("Capture inventory reconciliation is busy"),
            2,
        )
    })?;
    Ok(inventory
        .directories
        .iter()
        .find(|entry| {
            entry.challenge_id == challenge_id && entry.participation_id == participation_id
        })
        .cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("rsctf-capture-inventory-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(root.join("7/11")).unwrap();
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn write_time_refresh_tracks_creation_and_deletion_without_root_scans() {
        let scratch = Scratch::new();
        let directory = scratch.0.join("7/11");
        std::fs::write(directory.join("one.pcap"), b"one").unwrap();
        refresh_directory_sync(&directory).unwrap();

        let first = read_inventory(&scratch.0).unwrap();
        assert_eq!(first.directories.len(), 1);
        assert_eq!(first.directories[0].files, ["one.pcap"]);

        std::fs::write(directory.join("two.pcap"), b"two").unwrap();
        refresh_directory_sync(&directory).unwrap();
        let second = read_inventory(&scratch.0).unwrap();
        assert_eq!(second.directories[0].files.len(), 2);

        std::fs::remove_file(directory.join("one.pcap")).unwrap();
        refresh_directory_sync(&directory).unwrap();
        let third = read_inventory(&scratch.0).unwrap();
        assert_eq!(third.directories[0].files, ["two.pcap"]);
    }

    #[tokio::test]
    async fn missing_legacy_index_is_reconciled_once_into_bounded_metadata() {
        let scratch = Scratch::new();
        std::fs::write(scratch.0.join("7/11/legacy.pcap"), b"legacy").unwrap();

        let inventory = load(scratch.0.clone()).await.unwrap();
        assert_eq!(inventory.directories.len(), 1);
        assert!(inventory_path(&scratch.0).is_file());
    }

    #[tokio::test]
    async fn concurrent_legacy_reads_share_one_reconciliation_result() {
        let scratch = Scratch::new();
        std::fs::write(scratch.0.join("7/11/legacy.pcap"), b"legacy").unwrap();
        let key = format!("test:{}", scratch.0.display());
        let results = futures::future::join_all(
            (0..8).map(|_| reconcile_coalesced(scratch.0.clone(), key.clone())),
        )
        .await
        .into_iter()
        .collect::<AppResult<Vec<_>>>()
        .unwrap();
        assert!(results
            .iter()
            .skip(1)
            .all(|inventory| Arc::ptr_eq(&results[0], inventory)));
    }

    #[tokio::test]
    async fn rejected_write_marks_inventory_for_the_next_coalesced_reconciliation() {
        let scratch = Scratch::new();
        let directory = scratch.0.join("7/11");
        std::fs::write(directory.join("one.pcap"), b"one").unwrap();
        refresh_directory_sync(&directory).unwrap();

        let writer = acquire_writer(&scratch.0).unwrap();
        std::fs::write(directory.join("two.pcap"), b"two").unwrap();
        assert!(refresh_directory_sync(&directory).is_err());
        assert!(dirty_path(&scratch.0).is_file());
        drop(writer);

        let repaired = load(scratch.0.clone()).await.unwrap();
        assert_eq!(repaired.directories[0].files.len(), 2);
        assert!(!dirty_path(&scratch.0).exists());
    }

    #[test]
    fn manifest_names_cannot_escape_the_capture_directory() {
        let scratch = Scratch::new();
        let inventory = CaptureInventory {
            version: INVENTORY_VERSION,
            directories: vec![CaptureDirectoryInventory {
                challenge_id: 7,
                participation_id: 11,
                directory_modified_ns: 0,
                files: vec!["../secret.pcap".to_string()],
            }],
        };
        write_inventory(&scratch.0, &inventory).unwrap();
        assert!(read_inventory(&scratch.0).is_err());
    }

    #[test]
    fn oversized_directory_fails_closed() {
        let scratch = Scratch::new();
        let directory = scratch.0.join("7/11");
        for index in 0..=MAX_FILES_PER_DIRECTORY {
            std::fs::write(directory.join(format!("{index}.pcap")), []).unwrap();
        }
        assert!(refresh_directory_sync(&directory).is_err());
    }
}
