use super::*;

use std::collections::HashSet;
use std::sync::Mutex;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;

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
async fn game_delete_releases_and_purges_its_poster_reference_atomically() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("poster_delete_{}", uuid::Uuid::new_v4().simple());
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
        CREATE TABLE "Configs" (config_key TEXT PRIMARY KEY, value TEXT);
        CREATE TABLE "GameChallenges" (
          id INTEGER PRIMARY KEY, original_archive_blob_path TEXT
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let storage = MemoryStorage::default();
    let poster_bytes = b"game-poster";
    let (poster, _) = store_and_acquire(&pool, &storage, "poster", poster_bytes)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Games" (id, poster_hash) VALUES (2, $1)"#)
        .bind(&poster.hash)
        .execute(&pool)
        .await
        .unwrap();

    let mut transaction = pool.begin().await.unwrap();
    sqlx::query(r#"DELETE FROM "Games" WHERE id = 2"#)
        .execute(&mut *transaction)
        .await
        .unwrap();
    let released = release_direct_hash_locked(&mut transaction, &poster.hash)
        .await
        .unwrap();
    assert_eq!(released.deleted_hash.as_deref(), Some(poster.hash.as_str()));
    transaction.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#,)
            .bind(&poster.hash)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    assert!(purge_if_unreferenced(&pool, &storage, &poster.hash)
        .await
        .unwrap());
    assert!(!storage.exists(&poster.hash).await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Files" WHERE hash = $1"#)
            .bind(&poster.hash)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
