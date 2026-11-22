use std::{io, path::Path};

use rsctf_worker_protocol::CommandErrorCode;

use crate::enroll::{sync_parent_directory, write_new_file};
use crate::runtime::RuntimeError;

pub(super) async fn load<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, RuntimeError> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(io_error(io::Error::other("invalid worker state file")));
    }
    let bytes = tokio::fs::read(path).await.map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| io_error(io::Error::other("invalid worker state data")))
}

pub(super) async fn save(path: &Path, state: &impl serde::Serialize) -> Result<(), RuntimeError> {
    let temp = path.with_extension("tmp");
    // A killed process can leave the unpublished temporary file behind.
    remove(&temp).await?;
    let bytes = serde_json::to_vec(state).map_err(|e| io_error(e.into()))?;
    write_new_file(&temp, &bytes).await.map_err(io_error)?;
    tokio::fs::rename(&temp, path).await.map_err(io_error)?;
    sync_parent_directory(path).await.map_err(io_error)
}

pub(super) async fn remove(path: &Path) -> Result<(), RuntimeError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => sync_parent_directory(path).await.map_err(io_error),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_error(e)),
    }
}

pub(super) fn io_error(error: io::Error) -> RuntimeError {
    RuntimeError::new(
        CommandErrorCode::Internal,
        format!("worker runtime state: {error}"),
    )
}
