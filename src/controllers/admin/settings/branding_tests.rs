use std::collections::HashSet;
use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;

use super::clear_branding_hashes;
use crate::storage::{BlobStorage, StoredBlob};
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

#[derive(Default)]
struct MemoryStorage {
    blobs: Mutex<HashSet<String>>,
}

#[async_trait]
impl BlobStorage for MemoryStorage {
    async fn store(&self, name: &str, bytes: &[u8]) -> AppResult<StoredBlob> {
        let hash = sha256_hex(bytes);
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
        self.blobs.lock().unwrap().remove(hash);
        Ok(())
    }

    async fn exists(&self, hash: &str) -> bool {
        self.blobs.lock().unwrap().contains(hash)
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn branding_delete_atomically_tombstones_once_and_retry_purges() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("admin_branding_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |connection, _| {
            let statement = format!(r#"SET search_path TO "{search_path}""#);
            Box::pin(async move {
                sqlx::query(&statement).execute(connection).await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .unwrap();

    sqlx::raw_sql(
        r#"
        CREATE TABLE "Files" (
          id SERIAL PRIMARY KEY, hash TEXT NOT NULL UNIQUE,
          upload_time_utc TIMESTAMPTZ NOT NULL, file_size BIGINT NOT NULL,
          name TEXT NOT NULL, reference_count BIGINT NOT NULL
        );
        CREATE TABLE "Attachments" (id INTEGER PRIMARY KEY, local_file_id INTEGER);
        CREATE TABLE "Participations" (id INTEGER PRIMARY KEY, writeup_id INTEGER);
        CREATE TABLE "AspNetUsers" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
        CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
        CREATE TABLE "Games" (id INTEGER PRIMARY KEY, poster_hash TEXT);
        CREATE TABLE "Configs" (
          config_key TEXT PRIMARY KEY, value TEXT, cache_keys TEXT
        );
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, original_archive_blob_path TEXT
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let storage = MemoryStorage::default();
    let (branding, _) =
        crate::services::blob_refs::store_and_acquire(&pool, &storage, "branding.png", b"branding")
            .await
            .unwrap();
    for key in ["GlobalConfig:LogoHash", "GlobalConfig:FaviconHash"] {
        sqlx::query(
            r#"INSERT INTO "Configs" (config_key, value, cache_keys)
               VALUES ($1, $2, NULL)"#,
        )
        .bind(key)
        .bind(&branding.hash)
        .execute(&pool)
        .await
        .unwrap();
    }

    // Force the logical release to fail after both config writes. The owner
    // changes and reference decrement must roll back as one unit.
    sqlx::query(
        r#"ALTER TABLE "Files" ADD CONSTRAINT branding_reject_tombstone
           CHECK (reference_count > 0)"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(clear_branding_hashes(&pool).await.is_err());
    assert_eq!(
        config_values(&pool).await,
        [Some(branding.hash.clone()), Some(branding.hash.clone())]
    );
    assert_eq!(reference_count(&pool, &branding.hash).await, 1);
    sqlx::query(r#"ALTER TABLE "Files" DROP CONSTRAINT branding_reject_tombstone"#)
        .execute(&pool)
        .await
        .unwrap();

    let released = clear_branding_hashes(&pool).await.unwrap();
    assert_eq!(released.len(), 1);
    assert!(released.contains(&branding.hash));
    assert_eq!(config_values(&pool).await, [None, None]);
    assert_eq!(reference_count(&pool, &branding.hash).await, 0);
    assert!(storage.exists(&branding.hash).await);

    // Simulate a retry after committing the logical deletion but crashing
    // before object storage was purged. The retry must not release it twice.
    assert!(clear_branding_hashes(&pool).await.unwrap().is_empty());
    assert_eq!(reference_count(&pool, &branding.hash).await, 0);
    assert!(storage.exists(&branding.hash).await);

    assert_eq!(
        crate::services::blob_refs::purge_pending(&pool, &storage, 10)
            .await
            .unwrap(),
        1
    );
    assert!(!storage.exists(&branding.hash).await);
    assert_eq!(file_count(&pool, &branding.hash).await, 0);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

async fn reference_count(pool: &sqlx::PgPool, hash: &str) -> i64 {
    sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
        .bind(hash)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn config_values(pool: &sqlx::PgPool) -> Vec<Option<String>> {
    sqlx::query_scalar(
        r#"SELECT value FROM "Configs"
           WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
           ORDER BY config_key"#,
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn file_count(pool: &sqlx::PgPool, hash: &str) -> i64 {
    sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Files" WHERE hash = $1"#)
        .bind(hash)
        .fetch_one(pool)
        .await
        .unwrap()
}
