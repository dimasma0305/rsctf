//! Ported from RSCTF `Storage/Interface/IBlobStorage.cs`.

use async_trait::async_trait;

use crate::utils::error::AppResult;

/// Metadata for a stored blob (mirrors RSCTF `LocalFile`).
#[derive(Debug, Clone)]
pub struct StoredBlob {
    pub hash: String,
    pub size: i64,
    pub name: String,
}

#[async_trait]
pub trait BlobStorage: Send + Sync {
    /// Verify that this replica can reach and use the configured backend.
    /// Implementations should keep this probe cheap because readiness caches it
    /// only briefly. The default preserves compatibility for test doubles.
    async fn health(&self) -> AppResult<()> {
        Ok(())
    }
    /// Store `bytes` under its content hash; returns the blob metadata.
    async fn store(&self, name: &str, bytes: &[u8]) -> AppResult<StoredBlob>;
    /// Read a blob back by hash.
    async fn load(&self, hash: &str) -> AppResult<Vec<u8>>;
    /// Read a blob only when its stored representation is within `max_bytes`.
    ///
    /// Real storage backends override this to check metadata before allocating.
    /// The default preserves compatibility for small test doubles.
    async fn load_bounded(&self, hash: &str, max_bytes: usize) -> AppResult<Vec<u8>> {
        let bytes = self.load(hash).await?;
        if bytes.len() > max_bytes {
            return Err(crate::utils::error::AppError::internal(
                "blob exceeds the configured read limit",
            ));
        }
        Ok(bytes)
    }
    /// Delete a blob by hash (idempotent).
    async fn delete(&self, hash: &str) -> AppResult<()>;
    /// Whether a blob with this hash exists.
    async fn exists(&self, hash: &str) -> bool;
}
