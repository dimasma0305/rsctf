use std::path::{Path, PathBuf};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

use super::config::retention_days;

const MAX_RETENTION_SCAN_DIRECTORIES: usize = 100_000;

fn capture_retention_candidates(root: &Path) -> AppResult<Vec<(i32, PathBuf)>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(AppError::internal(error.to_string())),
    };
    let mut candidates = Vec::new();
    for entry in entries.take(MAX_RETENTION_SCAN_DIRECTORIES) {
        let entry = entry.map_err(|error| AppError::internal(error.to_string()))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| AppError::internal(error.to_string()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(challenge_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
            .filter(|id| *id > 0)
        else {
            continue;
        };
        candidates.push((challenge_id, path));
    }
    Ok(candidates)
}

/// Delete capture trees only after their game has been outside the configured
/// retention window. Numeric, regular directories under the fixed capture root
/// are the sole deletion targets; symlinks are never traversed.
pub(crate) async fn purge_expired_captures(state: &SharedState, batch: usize) -> AppResult<usize> {
    if batch == 0 {
        return Ok(0);
    }
    let root = PathBuf::from(&state.config.storage_root).join("capture");
    let scan_root = root.clone();
    let candidates = tokio::task::spawn_blocking(move || capture_retention_candidates(&scan_root))
        .await
        .map_err(|error| AppError::internal(error.to_string()))??;
    if candidates.is_empty() {
        return Ok(0);
    }
    let ids = candidates.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let expired = sqlx::query_scalar::<_, i32>(
        r#"SELECT challenge.id
             FROM "GameChallenges" challenge
             JOIN "Games" game ON game.id = challenge.game_id
            WHERE challenge.id = ANY($1)
              AND game.end_time_utc <
                  clock_timestamp() - ($2 * interval '1 day')"#,
    )
    .bind(&ids)
    .bind(retention_days())
    .fetch_all(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .collect::<std::collections::HashSet<_>>();
    let targets = candidates
        .into_iter()
        .filter(|(id, _)| expired.contains(id))
        .take(batch)
        .map(|(_, path)| path)
        .collect::<Vec<_>>();
    let count = targets.len();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        for path in targets {
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| AppError::internal(error.to_string()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AppError::internal(format!(
                    "capture retention target changed type: {}",
                    path.display()
                )));
            }
            std::fs::remove_dir_all(&path).map_err(|error| {
                AppError::internal(format!(
                    "failed to remove expired capture directory {}: {error}",
                    path.display()
                ))
            })?;
        }
        Ok(())
    })
    .await
    .map_err(|error| AppError::internal(error.to_string()))??;
    Ok(count)
}
