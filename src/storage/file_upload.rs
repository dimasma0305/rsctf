//! Bounded two-pass content verification for immutable file publication.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use crate::storage::StoredBlob;
use crate::utils::error::{AppError, AppResult};

pub(super) const READ_CHUNK_BYTES: usize = 128 * 1024;
pub(super) const MULTIPART_CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;
static UPLOAD_SLOTS: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)));

pub(super) fn io_error(error: std::io::Error) -> AppError {
    AppError::internal(format!("blob file upload: {error}"))
}

pub(super) struct FileUpload {
    file: tokio::fs::File,
    pub(super) hash: String,
    pub(super) size: u64,
    written: u64,
    digest: Sha256,
    verified: bool,
    _permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
}

impl FileUpload {
    pub(super) async fn prepare(file: tokio::fs::File, max_bytes: u64) -> AppResult<Self> {
        let permit = UPLOAD_SLOTS
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::too_many_requests(1))?;
        if max_bytes == 0 || max_bytes > MAX_FILE_UPLOAD_BYTES {
            return Err(AppError::bad_request(
                "File upload limit must be between 1 and 536870912 bytes",
            ));
        }
        let file = file.into_std().await;
        // Hash one sequential file on the blocking pool, instead of scheduling
        // one blocking read per chunk and hashing an entire file on Tokio.
        // Moving admission into this task also bounds work after cancellation.
        tokio::task::spawn_blocking(move || Self::hash_file(file, max_bytes, permit))
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
    }

    fn hash_file(
        mut file: std::fs::File,
        max_bytes: u64,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> AppResult<Self> {
        use std::io::{Read, Seek};
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(AppError::bad_request("Blob upload requires a regular file"));
        }
        if metadata.len() > max_bytes {
            return Err(AppError::payload_too_large("File exceeds its upload limit"));
        }
        file.rewind().map_err(io_error)?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0; READ_CHUNK_BYTES];
        let mut size = 0;
        loop {
            let count = file.read(&mut buffer).map_err(io_error)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            if size > max_bytes {
                return Err(AppError::payload_too_large(
                    "File grew beyond its upload limit",
                ));
            }
            digest.update(&buffer[..count]);
        }
        file.rewind().map_err(io_error)?;
        Ok(Self {
            file: tokio::fs::File::from_std(file),
            hash: hex::encode(digest.finalize()),
            size,
            written: 0,
            digest: Sha256::new(),
            verified: false,
            _permit: std::sync::Arc::new(permit),
        })
    }

    /// EOF verifies the second pass before a backend may commit its object.
    /// Read full chunks to preserve S3's minimum size for every non-final part.
    pub(super) async fn next_chunk(&mut self, capacity: usize) -> AppResult<Option<Bytes>> {
        if !(1..=MULTIPART_CHUNK_BYTES).contains(&capacity) {
            return Err(AppError::internal("Invalid file upload chunk size"));
        }
        if self.verified {
            return Ok(None);
        }
        let mut bytes = vec![0; capacity];
        let mut used = 0;
        let mut eof = false;
        while used < capacity {
            let count = self.file.read(&mut bytes[used..]).await.map_err(io_error)?;
            if count == 0 {
                eof = true;
                break;
            }
            self.written += count as u64;
            if self.written > self.size {
                return Err(AppError::conflict("File grew during blob upload"));
            }
            used += count;
        }
        bytes.truncate(used);
        if used >= 1024 * 1024 {
            let mut digest = std::mem::take(&mut self.digest);
            let permit = self._permit.clone();
            (self.digest, bytes) = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                digest.update(&bytes);
                (digest, bytes)
            })
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        } else {
            // At most 128 KiB on the local path; large multipart hashing is
            // kept off the runtime above.
            self.digest.update(&bytes);
        }
        if eof {
            if self.written != self.size || hex::encode(self.digest.clone().finalize()) != self.hash
            {
                return Err(AppError::conflict("File changed during blob upload"));
            }
            self.verified = true;
        }
        Ok((used != 0).then(|| Bytes::from(bytes)))
    }

    pub(super) fn metadata(&self, name: &str) -> AppResult<StoredBlob> {
        if !self.verified {
            return Err(AppError::internal(
                "File upload was not verified before publication",
            ));
        }
        Ok(self.existing_metadata(name))
    }

    /// Only after the backend has proved a complete object at this hash/size.
    /// No bytes from the source's second pass are published on a dedup hit:
    /// return the already-stored snapshot identified by the first-pass digest.
    pub(super) fn existing_metadata(&self, name: &str) -> StoredBlob {
        StoredBlob {
            hash: self.hash.clone(),
            size: self.size as i64,
            name: name.into(),
        }
    }
}

#[cfg(test)]
mod tests;
