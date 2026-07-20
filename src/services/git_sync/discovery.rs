use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::utils::error::{AppError, AppResult};

/// Walk the tree rooted at `dir` and return every challenge manifest path,
/// sorted for deterministic repository reconciliation.
pub async fn discover_challenges(dir: &Path) -> AppResult<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|error| {
            AppError::internal(format!("git_sync: read_dir {}: {error}", current.display()))
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: read dir entry in {}: {error}",
                current.display()
            ))
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|error| {
                AppError::internal(format!("git_sync: stat {}: {error}", path.display()))
            })?;
            if file_type.is_dir() {
                if path.file_name() != Some(OsStr::new(".git")) {
                    stack.push(path);
                }
            } else if file_type.is_file()
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name == "challenge.yml" || name == "challenge.yaml")
            {
                manifests.push(path);
            }
        }
    }
    manifests.sort();
    Ok(manifests)
}

/// Walk `dir` for every `.gzevent`, skipping Git metadata and sorting results.
pub async fn discover_events(dir: &Path) -> AppResult<Vec<PathBuf>> {
    let mut events = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&current).await.map_err(|error| {
            AppError::internal(format!("git_sync: read_dir {}: {error}", current.display()))
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            AppError::internal(format!(
                "git_sync: read dir entry in {}: {error}",
                current.display()
            ))
        })? {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|error| {
                AppError::internal(format!("git_sync: stat {}: {error}", path.display()))
            })?;
            if file_type.is_dir() {
                if path.file_name() != Some(OsStr::new(".git")) {
                    stack.push(path);
                }
            } else if file_type.is_file()
                && path.file_name().and_then(OsStr::to_str) == Some(".gzevent")
            {
                events.push(path);
            }
        }
    }
    events.sort();
    Ok(events)
}
