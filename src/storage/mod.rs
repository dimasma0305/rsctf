pub mod blob_storage;
pub mod local_blob_storage;
pub mod s3_blob_storage;

use std::str::FromStr;
use std::sync::Arc;

pub use blob_storage::{BlobStorage, StoredBlob};
pub use local_blob_storage::LocalBlobStorage;
pub use s3_blob_storage::S3BlobStorage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StorageBackend {
    Auto,
    Local,
    S3,
}

impl FromStr for StorageBackend {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "local" => Ok(Self::Local),
            "s3" => Ok(Self::S3),
            _ => anyhow::bail!(
                "invalid RSCTF_STORAGE_BACKEND {value:?}; expected auto, local, or s3"
            ),
        }
    }
}

/// Select the blob backend once at startup.
///
/// `RSCTF_STORAGE_BACKEND` accepts `auto` (the default), `local`, or `s3`.
/// Auto uses S3 when any `RSCTF_S3_*` setting is present; a partial S3
/// configuration is an error rather than a silent fallback to replica-local
/// disk. Local storage remains the zero-configuration single-node default.
pub fn from_env(local_root: impl Into<std::path::PathBuf>) -> anyhow::Result<Arc<dyn BlobStorage>> {
    let backend = std::env::var("RSCTF_STORAGE_BACKEND")
        .unwrap_or_else(|_| "auto".to_string())
        .parse::<StorageBackend>()?;
    match backend {
        StorageBackend::Auto => match S3BlobStorage::try_from_env()? {
            Some(storage) => Ok(Arc::new(storage)),
            None => Ok(Arc::new(LocalBlobStorage::new(local_root))),
        },
        StorageBackend::Local => Ok(Arc::new(LocalBlobStorage::new(local_root))),
        StorageBackend::S3 => S3BlobStorage::try_from_env()?
            .map(|storage| Arc::new(storage) as Arc<dyn BlobStorage>)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "RSCTF_STORAGE_BACKEND=s3 requires RSCTF_S3_BUCKET, RSCTF_S3_ACCESS_KEY, and RSCTF_S3_SECRET_KEY"
                )
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::StorageBackend;

    #[test]
    fn storage_backend_parsing_is_explicit_and_case_insensitive() {
        assert_eq!(
            "auto".parse::<StorageBackend>().unwrap(),
            StorageBackend::Auto
        );
        assert_eq!(
            " LOCAL ".parse::<StorageBackend>().unwrap(),
            StorageBackend::Local
        );
        assert_eq!("S3".parse::<StorageBackend>().unwrap(), StorageBackend::S3);
        assert!("filesystem".parse::<StorageBackend>().is_err());
        assert!("".parse::<StorageBackend>().is_err());
    }
}
