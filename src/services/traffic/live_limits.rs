//! Disk and rotation bounds for the blocking libpcap writer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::utils::error::{AppError, AppResult};

const CAPTURE_WRITE_HEADROOM_BYTES: u64 = 2 * 1024 * 1024;
static ACTIVE_CAPTURE_HEADROOM_BYTES: AtomicU64 = AtomicU64::new(0);

pub(super) struct CaptureDiskReservation {
    bytes: u64,
}

impl CaptureDiskReservation {
    pub(super) fn acquire(path: &Path, free_space_floor_bytes: u64) -> AppResult<Self> {
        ACTIVE_CAPTURE_HEADROOM_BYTES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(CAPTURE_WRITE_HEADROOM_BYTES)
            })
            .map_err(|_| AppError::unavailable("traffic capture disk reservation overflow"))?;
        let reservation = Self {
            bytes: CAPTURE_WRITE_HEADROOM_BYTES,
        };
        ensure_capture_free_space(path, free_space_floor_bytes)?;
        Ok(reservation)
    }
}

impl Drop for CaptureDiskReservation {
    fn drop(&mut self) {
        ACTIVE_CAPTURE_HEADROOM_BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LiveCaptureLimits {
    pub(super) max_file_bytes: u64,
    pub(super) max_directory_bytes: u64,
    pub(super) max_directory_files: usize,
    pub(super) free_space_floor_bytes: u64,
    pub(super) max_file_duration: Duration,
}

impl Default for LiveCaptureLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 128 * 1024 * 1024,
            max_directory_bytes: 256 * 1024 * 1024,
            max_directory_files: 256,
            free_space_floor_bytes: 512 * 1024 * 1024,
            max_file_duration: Duration::from_secs(3_600),
        }
    }
}

fn available_space(path: &Path) -> AppResult<u64> {
    let stats = nix::sys::statvfs::statvfs(path).map_err(|error| {
        AppError::internal(format!(
            "failed to inspect capture filesystem {}: {error}",
            path.display()
        ))
    })?;
    Ok(stats
        .blocks_available()
        .saturating_mul(stats.fragment_size()))
}

fn ensure_capture_free_space(path: &Path, free_space_floor_bytes: u64) -> AppResult<()> {
    let reserved = ACTIVE_CAPTURE_HEADROOM_BYTES.load(Ordering::Acquire);
    let required = free_space_floor_bytes.saturating_add(reserved);
    if available_space(path)? < required {
        return Err(AppError::unavailable(
            "traffic capture paused because the storage free-space floor was reached",
        ));
    }
    Ok(())
}

pub(super) fn refresh_capture_space_if_needed(
    directory: &Path,
    free_space_floor_bytes: u64,
    current_bytes: u64,
    packet_bytes: u64,
    next_space_check: &mut u64,
) -> AppResult<()> {
    let after_packet = current_bytes.saturating_add(packet_bytes);
    if after_packet >= *next_space_check {
        ensure_capture_free_space(directory, free_space_floor_bytes)?;
        *next_space_check = after_packet.saturating_add(super::FREE_SPACE_CHECK_INTERVAL_BYTES);
    }
    Ok(())
}

pub(super) fn enforce_capture_directory_budget(
    directory: &Path,
    maximum: u64,
    reserve: u64,
    maximum_files: usize,
) -> AppResult<Vec<PathBuf>> {
    if reserve > maximum || maximum_files == 0 {
        return Err(AppError::internal(
            "capture file limit exceeds participation capture budget",
        ));
    }
    let entries = std::fs::read_dir(directory).map_err(|error| {
        AppError::internal(format!(
            "failed to enumerate capture directory {}: {error}",
            directory.display()
        ))
    })?;
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries {
        let entry = entry.map_err(|error| AppError::internal(error.to_string()))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| AppError::internal(error.to_string()))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("pcap")
        {
            continue;
        }
        total = total.saturating_add(metadata.len());
        files.push((
            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            path,
            metadata.len(),
        ));
    }
    files.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.as_os_str().cmp(right.1.as_os_str()))
    });
    let mut file_count = files.len();
    let mut removed = Vec::new();
    for (_, path, bytes) in files {
        if total.saturating_add(reserve) <= maximum && file_count.saturating_add(1) <= maximum_files
        {
            break;
        }
        std::fs::remove_file(&path).map_err(|error| {
            AppError::internal(format!(
                "failed to rotate capture file {}: {error}",
                path.display()
            ))
        })?;
        total = total.saturating_sub(bytes);
        file_count = file_count.saturating_sub(1);
        removed.push(path);
    }
    if total.saturating_add(reserve) > maximum || file_count.saturating_add(1) > maximum_files {
        return Err(AppError::internal(
            "capture participation budget could not be reclaimed",
        ));
    }
    Ok(removed)
}

pub(super) fn rotated_capture_path(path: &Path) -> AppResult<PathBuf> {
    let directory = path
        .parent()
        .ok_or_else(|| AppError::internal("capture output has no parent directory"))?;
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("capture");
    Ok(directory.join(format!("{stem}-{}.pcap", uuid::Uuid::now_v7())))
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};

    use super::*;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("rsctf-capture-budget-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str, length: u64) -> PathBuf {
            let path = self.0.join(name);
            let mut file = std::fs::File::create(&path).unwrap();
            file.seek(SeekFrom::Start(length.saturating_sub(1)))
                .unwrap();
            file.write_all(&[0]).unwrap();
            path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn directory_budget_removes_oldest_pcaps_but_ignores_other_files() {
        let scratch = ScratchDir::new();
        let oldest = scratch.file("a.pcap", 60);
        let newest = scratch.file("b.pcap", 60);
        let note = scratch.file("note.txt", 500);

        let removed = enforce_capture_directory_budget(&scratch.0, 160, 80, 10).unwrap();

        assert!(!oldest.exists());
        assert!(newest.exists());
        assert!(note.exists());
        assert_eq!(removed, vec![oldest]);
    }

    #[test]
    fn directory_budget_enforces_a_file_count_cap() {
        let scratch = ScratchDir::new();
        let oldest = scratch.file("a.pcap", 1);
        scratch.file("b.pcap", 1);

        let removed = enforce_capture_directory_budget(&scratch.0, 1_000, 1, 2).unwrap();

        assert_eq!(removed, vec![oldest]);
        assert_eq!(std::fs::read_dir(&scratch.0).unwrap().flatten().count(), 1);
    }

    #[test]
    fn impossible_file_and_directory_limits_fail_closed() {
        let scratch = ScratchDir::new();
        assert!(enforce_capture_directory_budget(&scratch.0, 10, 11, 10).is_err());
        assert!(enforce_capture_directory_budget(&scratch.0, 10, 1, 0).is_err());
    }
}
