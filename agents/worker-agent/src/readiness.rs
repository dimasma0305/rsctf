//! Service-manager readiness marker for the authenticated worker control plane.
//!
//! The marker is deliberately local and contains no identity material. A
//! service manager can use its existence to distinguish "the process started"
//! from "the RSCTF server accepted this worker's mTLS control session."

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::io::AsyncWriteExt;

const READY_BODY: &[u8] = b"online\n";

#[derive(Clone, Debug)]
pub struct ReadinessFile {
    path: Option<PathBuf>,
}

impl ReadinessFile {
    pub fn new(path: Option<PathBuf>) -> Result<Self, ReadinessError> {
        if let Some(path) = path.as_deref() {
            validate_path(path)?;
        }
        Ok(Self { path })
    }

    pub async fn clear(&self) -> Result<(), ReadinessError> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        match tokio::fs::symlink_metadata(path).await {
            Ok(metadata) if metadata.file_type().is_file() => {
                tokio::fs::remove_file(path).await?;
            }
            Ok(_) => return Err(ReadinessError::UnsafeFile(path.to_path_buf())),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    pub async fn mark_connected(&self) -> Result<(), ReadinessError> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        self.clear().await?;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(path).await?;
        file.write_all(READY_BODY).await?;
        file.sync_all().await?;
        Ok(())
    }
}

fn validate_path(path: &Path) -> Result<(), ReadinessError> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(ReadinessError::InvalidPath);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ReadinessError {
    #[error("the worker readiness path must be an absolute file path")]
    InvalidPath,
    #[error("the worker readiness path is not a regular file: {}", .0.display())]
    UnsafeFile(PathBuf),
    #[error("worker readiness file I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> PathBuf {
        std::env::temp_dir().join(format!("rsctf-worker-ready-{}", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn marker_exists_only_between_mark_and_clear() {
        let directory = temporary_directory();
        tokio::fs::create_dir(&directory).await.unwrap();
        let path = directory.join("connected");
        let marker = ReadinessFile::new(Some(path.clone())).unwrap();

        marker.clear().await.unwrap();
        assert!(!path.exists());
        marker.mark_connected().await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), READY_BODY);
        marker.clear().await.unwrap();
        assert!(!path.exists());

        tokio::fs::remove_dir(directory).await.unwrap();
    }

    #[test]
    fn rejects_relative_marker_paths() {
        assert!(matches!(
            ReadinessFile::new(Some("connected".into())),
            Err(ReadinessError::InvalidPath)
        ));
    }

    #[tokio::test]
    async fn refuses_to_replace_a_non_file_marker() {
        let directory = temporary_directory();
        let path = directory.join("connected");
        tokio::fs::create_dir_all(&path).await.unwrap();
        let marker = ReadinessFile::new(Some(path.clone())).unwrap();

        assert!(matches!(
            marker.mark_connected().await,
            Err(ReadinessError::UnsafeFile(actual)) if actual == path
        ));

        tokio::fs::remove_dir_all(directory).await.unwrap();
    }
}
