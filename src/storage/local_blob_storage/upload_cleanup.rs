//! Incremental cleanup of exact, unlocked upload files left by process death.

use nix::fcntl::{Flock, FlockArg};
use std::fs::{File, ReadDir};
use std::time::{Duration, Instant, SystemTime};

use super::LocalBlobStorage;
use crate::storage::file_upload::io_error;
use crate::utils::error::{AppError, AppResult};

const GRACE: Duration = Duration::from_secs(60 * 60);
const ENTRY_LIMIT: usize = 256;

pub(super) async fn purge(storage: &LocalBlobStorage) -> AppResult<u64> {
    let Ok(mut cursor) = storage.upload_scan.try_lock() else {
        return Ok(0);
    };
    let root = storage.root.join(".uploads");
    let scan = cursor.take();
    let (scan, removed) = tokio::task::spawn_blocking(move || {
        let scan = match scan {
            Some(scan) => scan,
            None => match std::fs::read_dir(root) {
                Ok(scan) => scan,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((None, 0)),
                Err(error) => return Err(error),
            },
        };
        scan_batch(scan, SystemTime::now(), GRACE, ENTRY_LIMIT)
    })
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .map_err(io_error)?;
    *cursor = scan;
    Ok(removed)
}

fn managed_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_suffix(".upload"))
        .is_some_and(|name| name.len() == 36 && uuid::Uuid::parse_str(name).is_ok())
}

fn scan_batch(
    mut scan: ReadDir,
    now: SystemTime,
    grace: Duration,
    limit: usize,
) -> std::io::Result<(Option<ReadDir>, u64)> {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut removed = 0;
    for _ in 0..limit {
        if Instant::now() >= deadline {
            break;
        }
        let Some(entry) = scan.next() else {
            return Ok((None, removed));
        };
        let entry = entry?;
        if !managed_name(&entry.file_name()) || !entry.file_type()?.is_file() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !now
            .duration_since(metadata.modified()?)
            .is_ok_and(|age| age >= grace)
        {
            continue;
        }
        let file = match File::open(entry.path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let _lock = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => lock,
            Err((_, nix::errno::Errno::EAGAIN)) => continue,
            Err((_, error)) => return Err(std::io::Error::from_raw_os_error(error as i32)),
        };
        match std::fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok((Some(scan), removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_respects_live_uploads_and_reclaims_a_bounded_crash_backlog() {
        let root = std::env::temp_dir().join(format!("upload-gc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let live = root.join(format!("{}.upload", uuid::Uuid::new_v4()));
        let lock = Flock::lock(
            File::create(&live).unwrap(),
            FlockArg::LockExclusiveNonblock,
        )
        .unwrap();
        let foreign = root.join("foreign.upload");
        std::fs::write(&foreign, b"preserve").unwrap();
        let directory = root.join(format!("{}.upload", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let abandoned = (0..37)
            .map(|_| {
                let path = root.join(format!("{}.upload", uuid::Uuid::new_v4()));
                std::fs::write(&path, b"abandoned").unwrap();
                path
            })
            .collect::<Vec<_>>();
        let future = SystemTime::now() + GRACE + Duration::from_secs(1);
        let (mut cursor, mut removed) =
            scan_batch(std::fs::read_dir(&root).unwrap(), future, GRACE, 8).unwrap();
        assert!(removed <= 8);
        while let Some(scan) = cursor {
            let (next, count) = scan_batch(scan, future, GRACE, 8).unwrap();
            cursor = next;
            removed += count;
        }
        assert_eq!(removed, 37);
        assert!(abandoned.iter().all(|path| !path.exists()));
        assert!(live.exists() && foreign.exists() && directory.exists());
        drop(lock); // Process death releases the kernel lock without publishing.
        assert_eq!(
            scan_batch(std::fs::read_dir(&root).unwrap(), future, GRACE, 256)
                .unwrap()
                .1,
            1
        );
        assert!(!live.exists());
        let fresh = root.join(format!("{}.upload", uuid::Uuid::new_v4()));
        std::fs::write(&fresh, b"new").unwrap();
        assert_eq!(
            scan_batch(
                std::fs::read_dir(&root).unwrap(),
                SystemTime::now(),
                GRACE,
                256
            )
            .unwrap()
            .1,
            0
        );
        assert!(fresh.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
