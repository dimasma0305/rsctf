//! Ported from RSCTF `Storage/Interface/IBlobStorage.cs`.

use std::io;
use std::ops::Range;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;

use crate::utils::error::AppResult;

/// Metadata for a stored blob (mirrors RSCTF `LocalFile`).
#[derive(Debug, Clone)]
pub struct StoredBlob {
    pub hash: String,
    pub size: i64,
    pub name: String,
}

/// A fallible byte stream returned by storage backends for large downloads.
/// Keeping this type backend-neutral lets the HTTP layer forward blobs without
/// first collecting their complete contents in memory.
pub type BlobByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send + 'static>>;

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
    /// Return the stored byte length without loading the blob body.
    ///
    /// The default preserves compatibility for small in-memory test doubles;
    /// production backends override it with a metadata lookup.
    async fn size(&self, hash: &str) -> AppResult<u64> {
        Ok(self.load(hash).await?.len() as u64)
    }
    /// Stream an exclusive byte range from a blob.
    ///
    /// Production backends override this to avoid buffering. The default is
    /// intentionally simple for test doubles and validates the requested range
    /// before taking a slice.
    async fn stream_range(&self, hash: &str, range: Range<u64>) -> AppResult<BlobByteStream> {
        let bytes = self.load(hash).await?;
        let start = usize::try_from(range.start)
            .map_err(|_| crate::utils::error::AppError::not_found("blob not found"))?;
        let end = usize::try_from(range.end)
            .map_err(|_| crate::utils::error::AppError::not_found("blob not found"))?;
        let slice = bytes
            .get(start..end)
            .ok_or_else(|| crate::utils::error::AppError::not_found("blob not found"))?;
        let chunk = Bytes::copy_from_slice(slice);
        Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
    }
    /// Delete a blob by hash (idempotent).
    async fn delete(&self, hash: &str) -> AppResult<()>;
    /// Whether a blob with this hash exists.
    async fn exists(&self, hash: &str) -> bool;
}
