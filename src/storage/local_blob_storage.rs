//! Ported from RSCTF `Storage/LocalBlobStorage.cs` — filesystem blob store,
//! content-addressed by SHA-256 and sharded two levels deep.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::storage::blob_storage::{BlobStorage, StoredBlob};
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

pub struct LocalBlobStorage {
    root: PathBuf,
}

impl LocalBlobStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve the on-disk path for a content hash. Only a well-formed hash
    /// (exactly 64 hex digits) is addressable; anything else returns `None` so
    /// a crafted value containing `..`, `/`, or an absolute path can never
    /// escape the storage root — this is the crate's path-traversal guard.
    fn path_for(&self, hash: &str) -> Option<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.root.join(&hash[0..2]).join(&hash[2..4]).join(hash))
    }
}

#[async_trait]
impl BlobStorage for LocalBlobStorage {
    async fn health(&self) -> AppResult<()> {
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| AppError::internal(format!("create blob root: {e}")))?;
        let probe = self.root.join(".rsctf-health");
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(probe)
            .await
            .map_err(|e| AppError::internal(format!("open blob health probe: {e}")))?;
        Ok(())
    }

    async fn store(&self, name: &str, bytes: &[u8]) -> AppResult<StoredBlob> {
        let hash = sha256_hex(bytes);
        let path = self
            .path_for(&hash)
            .expect("sha256_hex always yields 64 hex digits");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::internal(format!("mkdir: {e}")))?;
        }
        if !Path::new(&path).exists() {
            // Publish through an atomic same-directory rename. Two replicas may
            // store identical content concurrently; readers must see either no
            // object or the complete bytes, never one writer's partial file.
            let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            tokio::fs::write(&temporary, bytes)
                .await
                .map_err(|e| AppError::internal(format!("write blob: {e}")))?;
            if let Err(error) = tokio::fs::rename(&temporary, &path).await {
                let _ = tokio::fs::remove_file(&temporary).await;
                if !Path::new(&path).exists() {
                    return Err(AppError::internal(format!("publish blob: {error}")));
                }
            }
        }
        Ok(StoredBlob {
            hash,
            size: bytes.len() as i64,
            name: name.to_string(),
        })
    }

    async fn load(&self, hash: &str) -> AppResult<Vec<u8>> {
        let path = self
            .path_for(hash)
            .ok_or_else(|| AppError::not_found("blob not found"))?;
        tokio::fs::read(&path)
            .await
            .map_err(|_| AppError::not_found("blob not found"))
    }

    async fn load_bounded(&self, hash: &str, max_bytes: usize) -> AppResult<Vec<u8>> {
        let path = self
            .path_for(hash)
            .ok_or_else(|| AppError::not_found("blob not found"))?;
        let file = tokio::fs::File::open(&path)
            .await
            .map_err(|_| AppError::not_found("blob not found"))?;
        let metadata = file
            .metadata()
            .await
            .map_err(|_| AppError::not_found("blob not found"))?;
        if metadata.len() > max_bytes as u64 {
            return Err(AppError::internal("blob exceeds the configured read limit"));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| AppError::not_found("blob not found"))?;
        if bytes.len() > max_bytes {
            return Err(AppError::internal("blob exceeds the configured read limit"));
        }
        Ok(bytes)
    }

    async fn delete(&self, hash: &str) -> AppResult<()> {
        if let Some(path) = self.path_for(hash) {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(AppError::internal(format!(
                        "delete local blob {}: {error}",
                        path.display()
                    )))
                }
            }
        }
        Ok(())
    }

    async fn exists(&self, hash: &str) -> bool {
        self.path_for(hash).is_some_and(|p| p.exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn delete_ignores_missing_content_but_reports_storage_failures() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-local-blob-delete-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let storage = LocalBlobStorage::new(&root);
        let hash = "a".repeat(64);

        storage.delete(&hash).await.unwrap();
        let path = storage.path_for(&hash).unwrap();
        tokio::fs::create_dir_all(&path).await.unwrap();
        let error = storage.delete(&hash).await.unwrap_err();
        assert!(error.to_string().contains("delete local blob"));

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn bounded_load_rejects_before_returning_oversized_content() {
        let root = std::env::temp_dir().join(format!(
            "rsctf-local-blob-bounded-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let storage = LocalBlobStorage::new(&root);
        let stored = storage.store("test", b"four").await.unwrap();

        assert!(storage.load_bounded(&stored.hash, 3).await.is_err());
        assert_eq!(
            storage.load_bounded(&stored.hash, 4).await.unwrap(),
            b"four"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
