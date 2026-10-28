//! Incremental, constant-memory traversal; a live catalog cannot monopolize GC.
use std::path::{Path, PathBuf};

const ENTRIES_PER_PASS: usize = 512;

pub(super) struct Discovery {
    root: PathBuf,
    stack: Vec<tokio::fs::ReadDir>,
}

impl Discovery {
    pub(super) fn new(storage: &Path) -> Self {
        Self {
            root: storage.join("checkers"),
            stack: Vec::with_capacity(4),
        }
    }

    pub(super) async fn scan(&mut self, limit: usize) -> std::io::Result<Vec<PathBuf>> {
        if self.stack.is_empty() {
            let metadata = match tokio::fs::symlink_metadata(&self.root).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(std::io::Error::other(
                    "checker root must be a real directory",
                ));
            }
            self.stack.push(tokio::fs::read_dir(&self.root).await?);
        }
        let mut candidates = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        for _ in 0..ENTRIES_PER_PASS {
            if candidates.len() >= limit || tokio::time::Instant::now() >= deadline {
                break;
            }
            let depth = self.stack.len();
            let Some(reader) = self.stack.last_mut() else {
                break;
            };
            let Some(entry) = reader.next_entry().await? else {
                self.stack.pop();
                continue;
            };
            let kind = entry.file_type().await?;
            if !kind.is_dir() || kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if depth == 4 {
                if super::revision_name_is_managed(&name) {
                    candidates.push(entry.path());
                }
            } else if depth != 3 || name == "revisions" {
                match tokio::fs::read_dir(entry.path()).await {
                    Ok(reader) => self.stack.push(reader),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scan_bounds_work_and_eventually_visits_a_large_live_catalog() {
        let root = std::env::temp_dir().join(format!("rsctf-scan-{}", uuid::Uuid::new_v4()));
        let revisions = root.join("checkers/1/test/revisions");
        tokio::fs::create_dir_all(&revisions).await.unwrap();
        for n in 0..700 {
            tokio::fs::create_dir(revisions.join(format!("{n:032x}")))
                .await
                .unwrap();
        }
        let mut cursor = Discovery::new(&root);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..8 {
            let page = cursor.scan(128).await.unwrap();
            assert!(page.len() <= 128);
            seen.extend(page);
        }
        assert_eq!(seen.len(), 700);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
