//! S3-backed `BlobStorage` (ported from RSCTF `Storage/S3BlobStorage.cs`).
//!
//! Uses the `object_store` crate's `AmazonS3` backend. Blobs are
//! content-addressed by SHA-256 and sharded two levels deep under a key
//! prefix, mirroring [`crate::storage::LocalBlobStorage`]:
//! `{prefix}/{aa}/{bb}/{hash}`.

use async_trait::async_trait;
use futures::StreamExt;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path as ObjectPath;
use object_store::{
    Error as ObjectStoreError, ObjectStore, ObjectStoreExt, PutOptions, PutPayload,
};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::storage::blob_storage::{BlobStorage, StoredBlob};
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

/// Default key prefix under which blobs are stored. Overridable with
/// `RSCTF_S3_PREFIX`; the actual object key is `{prefix}/{aa}/{bb}/{hash}`.
const DEFAULT_PREFIX: &str = "assets";

pub struct S3BlobStorage {
    client: AmazonS3,
    prefix: String,
    /// Startup must prove write access, not merely read an old health marker.
    /// Later readiness probes use HEAD and avoid an object write per request.
    write_verified: AtomicBool,
}

impl S3BlobStorage {
    /// Build an S3 storage backend from `RSCTF_S3_*` environment variables.
    ///
    /// Returns `Ok(None)` only when no S3 setting is present. Once any S3
    /// setting is supplied, the required values and client configuration are
    /// validated fail-closed so a typo cannot make one replica write to local
    /// disk while its peers use object storage.
    ///
    /// - `RSCTF_S3_BUCKET`     (required)
    /// - `RSCTF_S3_ACCESS_KEY` (required)
    /// - `RSCTF_S3_SECRET_KEY` (required)
    /// - `RSCTF_S3_REGION`     (optional)
    /// - `RSCTF_S3_ENDPOINT`   (optional; enables custom / MinIO-style hosts)
    /// - `RSCTF_S3_PREFIX`     (optional; defaults to `assets`)
    pub fn try_from_env() -> anyhow::Result<Option<Self>> {
        const SETTINGS: &[&str] = &[
            "RSCTF_S3_BUCKET",
            "RSCTF_S3_ACCESS_KEY",
            "RSCTF_S3_SECRET_KEY",
            "RSCTF_S3_REGION",
            "RSCTF_S3_ENDPOINT",
            "RSCTF_S3_PREFIX",
        ];
        if !SETTINGS.iter().any(|name| non_empty_env(name).is_some()) {
            return Ok(None);
        }
        let required = |name| {
            non_empty_env(name)
                .ok_or_else(|| anyhow::anyhow!("{name} is required when S3 storage is configured"))
        };
        let bucket = required("RSCTF_S3_BUCKET")?;
        let access_key = required("RSCTF_S3_ACCESS_KEY")?;
        let secret_key = required("RSCTF_S3_SECRET_KEY")?;

        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_access_key_id(access_key)
            .with_secret_access_key(secret_key);

        if let Some(region) = non_empty_env("RSCTF_S3_REGION") {
            builder = builder.with_region(region);
        }

        if let Some(endpoint) = non_empty_env("RSCTF_S3_ENDPOINT") {
            // Allow plain-HTTP endpoints so self-hosted S3 (e.g. MinIO over
            // http://) works out of the box; real AWS endpoints use https.
            let allow_http = endpoint.starts_with("http://");
            builder = builder.with_endpoint(endpoint).with_allow_http(allow_http);
        }

        let client = builder
            .build()
            .map_err(|error| anyhow::anyhow!("build S3 storage backend: {error}"))?;

        let prefix = non_empty_env("RSCTF_S3_PREFIX")
            .unwrap_or_else(|| DEFAULT_PREFIX.to_string())
            .trim_matches('/')
            .to_string();

        Ok(Some(Self {
            client,
            prefix,
            write_verified: AtomicBool::new(false),
        }))
    }

    /// Resolve the object key for a content hash. Only a well-formed hash
    /// (exactly 64 hex digits) is addressable; anything else returns `None`
    /// so a crafted value can never point outside the sharded key space.
    fn key_for(&self, hash: &str) -> Option<ObjectPath> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let shard = format!("{}/{}/{}", &hash[0..2], &hash[2..4], hash);
        let key = if self.prefix.is_empty() {
            shard
        } else {
            format!("{}/{shard}", self.prefix)
        };
        Some(ObjectPath::from(key))
    }
}

/// Read an environment variable, treating empty / whitespace-only as absent.
fn non_empty_env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// Map an `object_store::Error` to an [`AppError`], preserving not-found.
fn map_err(err: ObjectStoreError) -> AppError {
    match err {
        ObjectStoreError::NotFound { .. } => AppError::not_found("blob not found"),
        other => AppError::internal(format!("s3 storage: {other}")),
    }
}

#[async_trait]
impl BlobStorage for S3BlobStorage {
    async fn health(&self) -> AppResult<()> {
        let key = ObjectPath::from(if self.prefix.is_empty() {
            ".rsctf-health".to_string()
        } else {
            format!("{}/.rsctf-health", self.prefix)
        });
        if !self.write_verified.load(Ordering::Acquire) {
            self.client
                .put_opts(
                    &key,
                    PutPayload::from(Vec::<u8>::new()),
                    PutOptions::default(),
                )
                .await
                .map_err(map_err)?;
            self.write_verified.store(true, Ordering::Release);
        }
        self.client.head(&key).await.map_err(map_err)?;
        Ok(())
    }

    async fn store(&self, name: &str, bytes: &[u8]) -> AppResult<StoredBlob> {
        let hash = sha256_hex(bytes);
        let key = self
            .key_for(&hash)
            .expect("sha256_hex always yields 64 hex digits");

        let payload = PutPayload::from(bytes.to_vec());
        self.client
            .put_opts(&key, payload, PutOptions::default())
            .await
            .map_err(map_err)?;

        Ok(StoredBlob {
            hash,
            size: bytes.len() as i64,
            name: name.to_string(),
        })
    }

    async fn load(&self, hash: &str) -> AppResult<Vec<u8>> {
        let key = self
            .key_for(hash)
            .ok_or_else(|| AppError::not_found("blob not found"))?;

        let result = self.client.get(&key).await.map_err(map_err)?;
        let bytes = result.bytes().await.map_err(map_err)?;
        Ok(bytes.to_vec())
    }

    async fn load_bounded(&self, hash: &str, max_bytes: usize) -> AppResult<Vec<u8>> {
        let key = self
            .key_for(hash)
            .ok_or_else(|| AppError::not_found("blob not found"))?;
        let metadata = self.client.head(&key).await.map_err(map_err)?;
        if metadata.size > max_bytes as u64 {
            return Err(AppError::internal("blob exceeds the configured read limit"));
        }
        let result = self.client.get(&key).await.map_err(map_err)?;
        let mut stream = result.into_stream();
        let mut bytes = Vec::with_capacity(metadata.size as usize);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(map_err)?;
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(AppError::internal("blob exceeds the configured read limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn delete(&self, hash: &str) -> AppResult<()> {
        let Some(key) = self.key_for(hash) else {
            return Ok(());
        };
        match self.client.delete(&key).await {
            Ok(()) => Ok(()),
            // Deletion is idempotent — a missing object is not an error.
            Err(ObjectStoreError::NotFound { .. }) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }

    async fn exists(&self, hash: &str) -> bool {
        match self.key_for(hash) {
            Some(key) => self.client.head(&key).await.is_ok(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_storage(prefix: &str) -> S3BlobStorage {
        S3BlobStorage {
            client: AmazonS3Builder::new()
                .with_bucket_name("rsctf-test")
                .with_region("us-east-1")
                .with_access_key_id("test")
                .with_secret_access_key("test")
                .build()
                .unwrap(),
            prefix: prefix.to_string(),
            write_verified: AtomicBool::new(false),
        }
    }

    #[test]
    fn empty_prefix_does_not_create_an_absolute_object_key() {
        let hash = "aabb".to_string() + &"c".repeat(60);
        assert_eq!(
            test_storage("").key_for(&hash).unwrap().as_ref(),
            format!("aa/bb/{hash}")
        );
        assert_eq!(
            test_storage("assets").key_for(&hash).unwrap().as_ref(),
            format!("assets/aa/bb/{hash}")
        );
    }

    /// Round-trips a blob against a live S3-compatible endpoint.
    ///
    /// Ignored by default: requires real infrastructure. To run it, export a
    /// MinIO / S3 config and invoke explicitly:
    ///
    /// ```sh
    /// export RSCTF_S3_BUCKET=rsctf-test
    /// export RSCTF_S3_ENDPOINT=http://127.0.0.1:9000
    /// export RSCTF_S3_REGION=us-east-1
    /// export RSCTF_S3_ACCESS_KEY=minioadmin
    /// export RSCTF_S3_SECRET_KEY=minioadmin
    /// cargo test s3_round_trip -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "requires a live S3-compatible endpoint; see RSCTF_S3_* env setup"]
    async fn s3_round_trip() {
        let storage = S3BlobStorage::try_from_env()
            .expect("valid RSCTF_S3_* configuration")
            .expect("RSCTF_S3_* env must be set");
        let data = b"hello rsctf s3";

        let stored = storage.store("greeting.txt", data).await.unwrap();
        assert_eq!(stored.size, data.len() as i64);
        assert!(storage.exists(&stored.hash).await);

        let loaded = storage.load(&stored.hash).await.unwrap();
        assert_eq!(loaded, data);

        storage.delete(&stored.hash).await.unwrap();
        assert!(!storage.exists(&stored.hash).await);
        // Deleting a missing blob is a no-op.
        storage.delete(&stored.hash).await.unwrap();
    }
}
