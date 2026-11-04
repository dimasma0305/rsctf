//! Independent disk budgets for Git objects and materialized challenge files.
use super::{MAX_REPO_DEPTH, MAX_REPO_ENTRIES, MAX_REPO_FILES};
use crate::utils::error::{AppError, AppResult};
use std::path::Path;

const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct CheckoutLimits {
    pub content: u64,
    pub file: u64,
    pub metadata: u64,
}

impl CheckoutLimits {
    pub fn configured() -> AppResult<Self> {
        fn bytes(name: &str, default: u64) -> AppResult<u64> {
            match std::env::var(name) {
                Ok(raw) => raw
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .and_then(|n| n.checked_mul(MIB))
                    .ok_or_else(|| {
                        AppError::bad_request(format!("{name} must be a positive MiB count"))
                    }),
                Err(std::env::VarError::NotPresent) => Ok(default * MIB),
                Err(_) => Err(AppError::bad_request(format!("{name} is not valid UTF-8"))),
            }
        }
        Ok(Self {
            content: bytes("RSCTF_REPO_CONTENT_MIB", 1024)?,
            file: bytes("RSCTF_REPO_FILE_MIB", 256)?,
            metadata: bytes("RSCTF_REPO_GIT_MIB", 512)?,
        })
    }

    pub fn content_file(self, path: &Path, size: u64, total: u64) -> AppResult<()> {
        check_size("file", "RSCTF_REPO_FILE_MIB", path, size, self.file)?;
        check_size(
            "checked-out content",
            "RSCTF_REPO_CONTENT_MIB",
            path,
            total.saturating_add(size),
            self.content,
        )
    }
}

fn check_size(kind: &str, setting: &str, path: &Path, size: u64, limit: u64) -> AppResult<()> {
    if size > limit {
        let path = path.to_string_lossy().chars().take(240).collect::<String>();
        return Err(AppError::payload_too_large(format!(
            "repository {kind} exceeds its limit at {path}: {size} bytes, limit {limit} bytes; adjust {setting} or remove the oversized content"
        )));
    }
    Ok(())
}

pub(super) async fn validate_checkout_tree(root: &Path) -> AppResult<()> {
    validate_tree(root, CheckoutLimits::configured()?).await
}

pub(super) async fn validate_tree(root: &Path, limits: CheckoutLimits) -> AppResult<()> {
    let mut stack = vec![(root.to_path_buf(), 0usize, false)];
    let mut bytes = [0u64; 2];
    let mut entries_seen = [0usize; 2];
    let mut files_seen = [0usize; 2];
    while let Some((current, depth, metadata)) = stack.pop() {
        if depth > MAX_REPO_DEPTH {
            return Err(AppError::payload_too_large("repository tree is too deep"));
        }
        let mut entries = match tokio::fs::read_dir(&current).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(AppError::internal(format!(
                    "git_sync: inspect checkout: {error}"
                )))
            }
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| AppError::internal(format!("git_sync: inspect checkout entry: {e}")))?
        {
            let path = entry.path();
            let metadata = metadata || (depth == 0 && entry.file_name() == ".git");
            let bucket = usize::from(metadata);
            entries_seen[bucket] += 1;
            if entries_seen[bucket] > MAX_REPO_ENTRIES {
                return Err(AppError::payload_too_large(
                    "repository contains too many entries",
                ));
            }
            let kind = match entry.file_type().await {
                Ok(kind) => kind,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(AppError::internal(format!(
                        "git_sync: inspect checkout type: {error}"
                    )))
                }
            };
            if kind.is_dir() {
                stack.push((path, depth + 1, metadata));
            } else if kind.is_file() {
                files_seen[bucket] += 1;
                if files_seen[bucket] > MAX_REPO_FILES {
                    return Err(AppError::payload_too_large(
                        "repository contains too many files",
                    ));
                }
                // Git atomically renames pack/temp files while this monitor runs.
                let size = match entry.metadata().await {
                    Ok(metadata) => metadata.len(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(AppError::internal(format!(
                            "git_sync: inspect checkout size: {error}"
                        )))
                    }
                };
                let relative = path.strip_prefix(root).unwrap_or(&path);
                if metadata {
                    check_size(
                        "Git metadata",
                        "RSCTF_REPO_GIT_MIB",
                        relative,
                        bytes[bucket].saturating_add(size),
                        limits.metadata,
                    )?;
                } else {
                    limits.content_file(relative, size, bytes[bucket])?;
                }
                bytes[bucket] = bytes[bucket].saturating_add(size);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) async fn checkout_usage_exceeds(root: &Path) -> AppResult<bool> {
    match validate_checkout_tree(root).await {
        Ok(()) => Ok(false),
        Err(AppError::PayloadTooLarge(_)) => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn git_storage_does_not_consume_content_budget_and_errors_identify_the_file() {
        let root = std::env::temp_dir().join(format!("rsctf-budget-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join(".git/objects/pack"))
            .await
            .unwrap();
        tokio::fs::write(root.join("challenge.zip"), [0; 10])
            .await
            .unwrap();
        tokio::fs::write(root.join(".git/objects/pack/a.pack"), [0; 12])
            .await
            .unwrap();
        let limits = CheckoutLimits {
            content: 10,
            file: 10,
            metadata: 12,
        };
        validate_tree(&root, limits).await.unwrap();
        let error = validate_tree(
            &root,
            CheckoutLimits {
                metadata: 11,
                ..limits
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AppError::PayloadTooLarge(_)));
        assert!(error.to_string().contains("Git metadata"));
        assert!(error
            .to_string()
            .contains(".git/objects/pack/a.pack: 12 bytes, limit 11 bytes"));
        let error = validate_tree(&root, CheckoutLimits { file: 9, ..limits })
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("challenge.zip: 10 bytes, limit 9 bytes"));
        assert!(error.to_string().contains("RSCTF_REPO_FILE_MIB"));
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
