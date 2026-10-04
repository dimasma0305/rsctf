use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::Notify;

use super::*;

pub(crate) async fn install_operation_tables(pool: &sqlx::PgPool) {
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS "BlobStagingOperations" (
            operation_id UUID PRIMARY KEY,
            owner_scope TEXT NOT NULL,
            owner_user_id UUID NULL,
            content_hash VARCHAR(64) NOT NULL,
            file_name VARCHAR(255) NOT NULL,
            file_size BIGINT NOT NULL,
            state VARCHAR(16) NOT NULL,
            created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
            lease_expires_at_utc TIMESTAMPTZ NOT NULL,
            published_at_utc TIMESTAMPTZ NULL,
            published_owner_scope TEXT NULL,
            last_error TEXT NULL
        );
        CREATE TABLE IF NOT EXISTS "BlobDeletionOperations" (
            content_hash VARCHAR(64) PRIMARY KEY,
            operation_id UUID NOT NULL,
            state VARCHAR(16) NOT NULL,
            lease_expires_at_utc TIMESTAMPTZ NOT NULL,
            updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
            last_error TEXT NULL
        );
        "#,
    )
    .execute(pool)
    .await
    .unwrap();
}

#[derive(Default)]
pub(super) struct CoordinatedStorage {
    pub(super) blobs: Mutex<HashSet<String>>,
    pub(super) stores: AtomicUsize,
    pub(super) delete_started: Notify,
    pub(super) allow_delete: Notify,
}

pub(super) struct FailingDeleteStorage;

#[async_trait]
impl BlobStorage for FailingDeleteStorage {
    async fn store(&self, _name: &str, _bytes: &[u8]) -> AppResult<StoredBlob> {
        Err(AppError::internal("not used"))
    }

    async fn load(&self, _hash: &str) -> AppResult<Vec<u8>> {
        Err(AppError::not_found("blob not found"))
    }

    async fn delete(&self, _hash: &str) -> AppResult<()> {
        Err(AppError::internal("simulated storage delete failure"))
    }

    async fn exists(&self, _hash: &str) -> bool {
        true
    }
}

impl CoordinatedStorage {
    pub(super) fn seed(&self, hash: String) {
        self.blobs.lock().unwrap().insert(hash);
    }
}

#[async_trait]
impl BlobStorage for CoordinatedStorage {
    async fn store(&self, name: &str, bytes: &[u8]) -> AppResult<StoredBlob> {
        let hash = sha256_hex(bytes);
        self.stores.fetch_add(1, Ordering::SeqCst);
        self.blobs.lock().unwrap().insert(hash.clone());
        Ok(StoredBlob {
            hash,
            size: bytes.len() as i64,
            name: name.to_string(),
        })
    }

    async fn load(&self, hash: &str) -> AppResult<Vec<u8>> {
        self.blobs
            .lock()
            .unwrap()
            .contains(hash)
            .then(Vec::new)
            .ok_or_else(|| AppError::not_found("blob not found"))
    }

    async fn delete(&self, hash: &str) -> AppResult<()> {
        self.delete_started.notify_one();
        self.allow_delete.notified().await;
        self.blobs.lock().unwrap().remove(hash);
        Ok(())
    }

    async fn exists(&self, hash: &str) -> bool {
        self.blobs.lock().unwrap().contains(hash)
    }
}
