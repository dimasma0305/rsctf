use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::InstallationStatusArgs;

const IDENTITY_FILES: [&str; 4] = [
    "worker-key.pem",
    "worker-cert.pem",
    "worker-ca.pem",
    "worker.json",
];

pub fn print_status(arguments: InstallationStatusArgs) -> Result<(), InstallationStatusError> {
    match inspect(&arguments.state_dir)? {
        InstallationStatus::Empty => println!("empty"),
        InstallationStatus::Enrolled => println!("enrolled"),
    }
    Ok(())
}

fn inspect(state_dir: &Path) -> Result<InstallationStatus, InstallationStatusError> {
    match std::fs::symlink_metadata(state_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(InstallationStatusError::UnsafeStateDirectory(
                state_dir.to_path_buf(),
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(InstallationStatus::Empty),
        Err(error) => return Err(InstallationStatusError::Io(error)),
    }

    let mut present = 0;
    for name in IDENTITY_FILES {
        let path = state_dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                present += 1;
            }
            Ok(_) => return Err(InstallationStatusError::UnsafeIdentity(path)),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(InstallationStatusError::Io(error)),
        }
    }

    match present {
        0 => Ok(InstallationStatus::Empty),
        count if count == IDENTITY_FILES.len() => Ok(InstallationStatus::Enrolled),
        _ => Err(InstallationStatusError::Incomplete),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InstallationStatus {
    Empty,
    Enrolled,
}

#[derive(Debug, Error)]
pub enum InstallationStatusError {
    #[error("worker state inspection failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("worker state directory is not a real directory: {0}")]
    UnsafeStateDirectory(PathBuf),
    #[error("worker identity path is not a regular file: {0}")]
    UnsafeIdentity(PathBuf),
    #[error(
        "worker state contains an incomplete identity; revoke the worker and clean the state deliberately before enrolling again"
    )]
    Incomplete,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> PathBuf {
        std::env::temp_dir().join(format!("rsctf-worker-status-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn classifies_absent_and_empty_state() {
        let directory = temporary_directory();
        assert_eq!(inspect(&directory).unwrap(), InstallationStatus::Empty);
        std::fs::create_dir(&directory).unwrap();
        assert_eq!(inspect(&directory).unwrap(), InstallationStatus::Empty);
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn classifies_complete_state_and_rejects_partial_state() {
        let directory = temporary_directory();
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join(IDENTITY_FILES[0]), b"key").unwrap();
        assert!(matches!(
            inspect(&directory),
            Err(InstallationStatusError::Incomplete)
        ));
        for name in &IDENTITY_FILES[1..] {
            std::fs::write(directory.join(name), b"identity").unwrap();
        }
        assert_eq!(inspect(&directory).unwrap(), InstallationStatus::Enrolled);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_identity_files() {
        use std::os::unix::fs::symlink;

        let directory = temporary_directory();
        std::fs::create_dir(&directory).unwrap();
        symlink("/etc/passwd", directory.join(IDENTITY_FILES[0])).unwrap();
        assert!(matches!(
            inspect(&directory),
            Err(InstallationStatusError::UnsafeIdentity(_))
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
