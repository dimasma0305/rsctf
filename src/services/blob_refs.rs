//! Atomic metadata operations for content-addressed, ref-counted blobs.
//!
//! Every physical store/delete and matching metadata operation shares a
//! transaction-scoped PostgreSQL advisory lock keyed by content hash. This
//! makes the object store and database ordering safe across replicas without
//! relying on process-local mutexes.

use sqlx::{PgPool, Postgres, Transaction};

use crate::storage::{BlobStorage, StoredBlob};
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

mod attachments;
mod challenges;
pub use attachments::delete_orphan_attachments;
pub use challenges::{delete_challenge, delete_game_challenges, DeletedChallengeArtifacts};

const UPSERT_FILE_SQL: &str = r#"
    INSERT INTO "Files" (hash, upload_time_utc, file_size, name, reference_count)
    VALUES ($1, now(), $2, $3, 1)
    ON CONFLICT (hash) DO UPDATE
       SET reference_count = "Files".reference_count + 1
    RETURNING id
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseOutcome {
    pub found: bool,
    /// Set only when this operation released the final logical reference.
    /// A zero-reference metadata tombstone remains until the post-commit
    /// physical delete in [`purge_if_unreferenced`] succeeds.
    pub deleted_hash: Option<String>,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

async fn lock_hash(
    transaction: &mut Transaction<'_, Postgres>,
    hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(hash)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

async fn acquire_locked(
    transaction: &mut Transaction<'_, Postgres>,
    hash: &str,
    name: &str,
    size: i64,
) -> Result<i32, sqlx::Error> {
    sqlx::query_scalar::<_, i32>(UPSERT_FILE_SQL)
        .bind(hash)
        .bind(size)
        .bind(name)
        .fetch_one(&mut **transaction)
        .await
}

/// Store and acquire within a caller-owned transaction. This is used when the
/// new blob reference must commit atomically with another domain row (for
/// example, a challenge attachment link).
pub(crate) async fn store_and_acquire_in_transaction(
    storage: &dyn BlobStorage,
    transaction: &mut Transaction<'_, Postgres>,
    name: &str,
    bytes: &[u8],
) -> AppResult<(StoredBlob, i32)> {
    let expected_hash = sha256_hex(bytes);
    lock_hash(transaction, &expected_hash)
        .await
        .map_err(database_error)?;
    let blob = storage.store(name, bytes).await?;
    if blob.hash != expected_hash {
        return Err(AppError::internal(
            "blob storage returned a hash that does not match its content",
        ));
    }
    let id = acquire_locked(transaction, &blob.hash, name, blob.size)
        .await
        .map_err(database_error)?;
    Ok((blob, id))
}

/// Store bytes and add one logical reference under the same distributed hash
/// lock used by deletion. The physical write intentionally occurs while the
/// SQL transaction holds that lock, closing the store-before-metadata window.
pub async fn store_and_acquire(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    name: &str,
    bytes: &[u8],
) -> AppResult<(StoredBlob, i32)> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let stored = store_and_acquire_in_transaction(storage, &mut transaction, name, bytes).await?;
    transaction.commit().await.map_err(database_error)?;
    Ok(stored)
}

#[cfg(test)]
async fn acquire(pool: &PgPool, hash: &str, name: &str, size: i64) -> AppResult<i32> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let id = acquire_locked(&mut transaction, hash, name, size)
        .await
        .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(id)
}

async fn release_locked(
    transaction: &mut Transaction<'_, Postgres>,
    id: i32,
) -> Result<ReleaseOutcome, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, i64)>(
        r#"SELECT hash, reference_count
             FROM "Files"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some((hash, reference_count)) = row else {
        return Ok(ReleaseOutcome {
            found: false,
            deleted_hash: None,
        });
    };

    if reference_count > 1 {
        sqlx::query(
            r#"UPDATE "Files"
                  SET reference_count = reference_count - 1
                WHERE id = $1"#,
        )
        .bind(id)
        .execute(&mut **transaction)
        .await?;
        Ok(ReleaseOutcome {
            found: true,
            deleted_hash: None,
        })
    } else {
        let has_owner: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "Attachments" WHERE local_file_id = $1)
                   OR EXISTS(SELECT 1 FROM "Participations" WHERE writeup_id = $1)
                   OR EXISTS(SELECT 1 FROM "AspNetUsers" WHERE avatar_hash = $2)
                   OR EXISTS(SELECT 1 FROM "Teams" WHERE avatar_hash = $2)
                   OR EXISTS(SELECT 1 FROM "Games" WHERE poster_hash = $2)
                   OR EXISTS(
                        SELECT 1 FROM "Configs"
                         WHERE config_key IN (
                               'GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash'
                         )
                           AND value = $2
                   )
                   OR EXISTS(
                        SELECT 1 FROM "GameChallenges"
                         WHERE original_archive_blob_path = $2
                   )"#,
        )
        .bind(id)
        .bind(&hash)
        .fetch_one(&mut **transaction)
        .await?;
        if has_owner {
            sqlx::query(r#"UPDATE "Files" SET reference_count = 1 WHERE id = $1"#)
                .bind(id)
                .execute(&mut **transaction)
                .await?;
            return Ok(ReleaseOutcome {
                found: true,
                deleted_hash: None,
            });
        }
        // Keep a durable zero-reference tombstone until physical deletion has
        // succeeded. If this process crashes after the owning row commits but
        // before object storage is touched, singleton maintenance can retry by
        // scanning these rows. A concurrent acquire runs under the same hash
        // lock and atomically raises this back to one before recreating bytes.
        sqlx::query(r#"UPDATE "Files" SET reference_count = 0 WHERE id = $1"#)
            .bind(id)
            .execute(&mut **transaction)
            .await?;
        Ok(ReleaseOutcome {
            found: true,
            deleted_hash: Some(hash),
        })
    }
}

/// Delete one attachment and release its local blob in the same transaction.
/// Locking the attachment row makes concurrent idempotent deletes consume the
/// reference exactly once.
pub async fn delete_attachment(pool: &PgPool, attachment_id: i32) -> AppResult<Option<String>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let file_id = sqlx::query_as::<_, (Option<i32>,)>(
        r#"SELECT local_file_id
             FROM "Attachments"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(attachment_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    let Some((file_id,)) = file_id else {
        transaction.commit().await.map_err(database_error)?;
        return Ok(None);
    };

    let file = match file_id {
        Some(id) => sqlx::query_scalar::<_, String>(r#"SELECT hash FROM "Files" WHERE id = $1"#)
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .map(|hash| (id, hash)),
        None => None,
    };
    if let Some((_, hash)) = &file {
        lock_hash(&mut transaction, hash)
            .await
            .map_err(database_error)?;
    }
    sqlx::query(r#"DELETE FROM "Attachments" WHERE id = $1"#)
        .bind(attachment_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
    let deleted_hash = match file {
        Some((id, _)) => {
            release_locked(&mut transaction, id)
                .await
                .map_err(database_error)?
                .deleted_hash
        }
        None => None,
    };
    transaction.commit().await.map_err(database_error)?;
    Ok(deleted_hash)
}

/// Clear and release every writeup for one game exactly once, even when two
/// admin replicas issue the cleanup concurrently.
pub async fn clear_game_writeups(pool: &PgPool, game_id: i32) -> AppResult<Vec<String>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let file_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT writeup_id
             FROM "Participations"
            WHERE game_id = $1 AND writeup_id IS NOT NULL
            ORDER BY id
            FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(database_error)?;
    if file_ids.is_empty() {
        transaction.commit().await.map_err(database_error)?;
        return Ok(Vec::new());
    }

    let mut files =
        sqlx::query_as::<_, (i32, String)>(r#"SELECT id, hash FROM "Files" WHERE id = ANY($1)"#)
            .bind(&file_ids)
            .fetch_all(&mut *transaction)
            .await
            .map_err(database_error)?;
    files.sort_unstable_by(|left, right| left.1.cmp(&right.1));
    files.dedup_by_key(|file| file.0);
    for (_, hash) in &files {
        lock_hash(&mut transaction, hash)
            .await
            .map_err(database_error)?;
    }
    sqlx::query(
        r#"UPDATE "Participations"
              SET writeup_id = NULL
            WHERE game_id = $1 AND writeup_id IS NOT NULL"#,
    )
    .bind(game_id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;

    let mut releases = std::collections::BTreeMap::<i32, usize>::new();
    for file_id in file_ids {
        *releases.entry(file_id).or_default() += 1;
    }
    let mut deleted_hashes = Vec::new();
    for (file_id, count) in releases {
        for _ in 0..count {
            if let Some(hash) = release_locked(&mut transaction, file_id)
                .await
                .map_err(database_error)?
                .deleted_hash
            {
                deleted_hashes.push(hash);
            }
        }
    }
    transaction.commit().await.map_err(database_error)?;
    Ok(deleted_hashes)
}

/// Release one reference selected by its content hash.
pub async fn release_by_hash(pool: &PgPool, hash: &str) -> AppResult<ReleaseOutcome> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let id = sqlx::query_scalar::<_, i32>(r#"SELECT id FROM "Files" WHERE hash = $1 FOR UPDATE"#)
        .bind(hash)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
    let outcome = match id {
        Some(id) => release_locked(&mut transaction, id)
            .await
            .map_err(database_error)?,
        None => ReleaseOutcome {
            found: false,
            deleted_hash: None,
        },
    };
    transaction.commit().await.map_err(database_error)?;
    Ok(outcome)
}

/// Release a direct hash owner (avatar, poster, build archive, or branding)
/// and purge legacy untracked content when no durable owner remains.
pub async fn release_and_purge(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    hash: &str,
) -> AppResult<bool> {
    let outcome = release_by_hash(pool, hash).await?;
    if outcome.found && outcome.deleted_hash.is_none() {
        return Ok(false);
    }
    purge_if_unreferenced(pool, storage, hash).await
}

async fn lock_writeup_hashes(
    transaction: &mut Transaction<'_, Postgres>,
    participation_id: i32,
    new_hash: &str,
) -> AppResult<Option<(i32, String)>> {
    let current = sqlx::query_as::<_, (Option<i32>,)>(
        r#"SELECT writeup_id
             FROM "Participations"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(participation_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("Participation not found"))?;

    let old = match current.0 {
        Some(id) => sqlx::query_scalar::<_, String>(r#"SELECT hash FROM "Files" WHERE id = $1"#)
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(database_error)?
            .map(|old_hash| (id, old_hash)),
        None => None,
    };

    // Every multi-hash operation locks in lexical order, preventing two
    // replacements that swap hashes from deadlocking.
    let mut hashes = vec![new_hash];
    if let Some((_, old_hash)) = &old {
        hashes.push(old_hash);
    }
    hashes.sort_unstable();
    hashes.dedup();
    for hash in hashes {
        lock_hash(transaction, hash).await.map_err(database_error)?;
    }
    Ok(old)
}

async fn replace_writeup_locked(
    transaction: &mut Transaction<'_, Postgres>,
    participation_id: i32,
    old: Option<(i32, String)>,
    hash: &str,
    name: &str,
    size: i64,
) -> Result<Option<String>, sqlx::Error> {
    let new_id = acquire_locked(transaction, hash, name, size).await?;
    sqlx::query(
        r#"UPDATE "Participations"
              SET writeup_id = $2
            WHERE id = $1"#,
    )
    .bind(participation_id)
    .bind(new_id)
    .execute(&mut **transaction)
    .await?;

    match old {
        Some((old_id, _)) => Ok(release_locked(transaction, old_id).await?.deleted_hash),
        None => Ok(None),
    }
}

/// Atomically replace a participation's writeup reference and return the old
/// hash only when its final metadata row was removed.
///
/// The participation row is locked and re-read inside the transaction, so two
/// simultaneous uploads from separate replicas release the actual predecessor
/// exactly once rather than both acting on stale request context.
#[cfg(test)]
async fn replace_writeup(
    pool: &PgPool,
    participation_id: i32,
    hash: &str,
    name: &str,
    size: i64,
) -> AppResult<Option<String>> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let old = lock_writeup_hashes(&mut transaction, participation_id, hash).await?;
    let deleted_hash =
        replace_writeup_locked(&mut transaction, participation_id, old, hash, name, size)
            .await
            .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(deleted_hash)
}

/// Store and atomically replace a participation writeup under the distributed
/// content-hash lock. The physical write, metadata upsert, FK swap, and old
/// reference release are ordered as one replica-safe operation.
pub async fn store_and_replace_writeup(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    participation_id: i32,
    name: &str,
    bytes: &[u8],
) -> AppResult<(StoredBlob, Option<String>)> {
    let expected_hash = sha256_hex(bytes);
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let old = lock_writeup_hashes(&mut transaction, participation_id, &expected_hash).await?;
    let blob = storage.store(name, bytes).await?;
    if blob.hash != expected_hash {
        return Err(AppError::internal(
            "blob storage returned a hash that does not match its content",
        ));
    }
    let deleted_hash = replace_writeup_locked(
        &mut transaction,
        participation_id,
        old,
        &blob.hash,
        name,
        blob.size,
    )
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok((blob, deleted_hash))
}

/// Delete physical content only when a fresh post-commit query confirms that
/// no metadata or direct-hash owner currently references it. Returns whether
/// deletion ran.
pub async fn purge_if_unreferenced(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    hash: &str,
) -> AppResult<bool> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    lock_hash(&mut transaction, hash)
        .await
        .map_err(database_error)?;
    let still_referenced: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
                    SELECT 1 FROM "Files"
                     WHERE hash = $1 AND reference_count > 0
               )
               OR EXISTS(
                    SELECT 1 FROM "Attachments" attachment
                    JOIN "Files" file ON file.id = attachment.local_file_id
                     WHERE file.hash = $1
               )
               OR EXISTS(
                    SELECT 1 FROM "Participations" participation
                    JOIN "Files" file ON file.id = participation.writeup_id
                     WHERE file.hash = $1
               )
               OR EXISTS(SELECT 1 FROM "AspNetUsers" WHERE avatar_hash = $1)
               OR EXISTS(SELECT 1 FROM "Teams" WHERE avatar_hash = $1)
               OR EXISTS(SELECT 1 FROM "Games" WHERE poster_hash = $1)
               OR EXISTS(
                    SELECT 1 FROM "Configs"
                     WHERE config_key IN ('GlobalConfig:LogoHash', 'GlobalConfig:FaviconHash')
                       AND value = $1
               )
               OR EXISTS(
                    SELECT 1 FROM "GameChallenges"
                     WHERE original_archive_blob_path = $1
               )"#,
    )
    .bind(hash)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if still_referenced {
        transaction.commit().await.map_err(database_error)?;
        return Ok(false);
    }
    storage.delete(hash).await?;
    sqlx::query(
        r#"DELETE FROM "Files"
            WHERE hash = $1 AND reference_count <= 0"#,
    )
    .bind(hash)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(true)
}

/// Retry a bounded batch of durable zero-reference blob tombstones.
///
/// The final-release transaction intentionally leaves these rows behind until
/// object storage acknowledges deletion. Running this from singleton
/// maintenance closes the commit-to-object-delete crash window without a
/// separate work queue or schema migration.
pub async fn purge_pending(pool: &PgPool, storage: &dyn BlobStorage, limit: i64) -> AppResult<u64> {
    let hashes = sqlx::query_scalar::<_, String>(
        r#"SELECT hash FROM "Files"
            WHERE reference_count <= 0
            ORDER BY id
            LIMIT $1"#,
    )
    .bind(limit.clamp(1, 256))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    let mut purged = 0;
    for hash in hashes {
        purged += u64::from(purge_if_unreferenced(pool, storage, &hash).await?);
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sqlx::postgres::PgPoolOptions;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct CoordinatedStorage {
        blobs: Mutex<HashSet<String>>,
        stores: AtomicUsize,
        delete_started: Notify,
        allow_delete: Notify,
    }

    struct FailingDeleteStorage;

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
        fn seed(&self, hash: String) {
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

    #[test]
    fn acquisition_is_an_atomic_conflict_increment() {
        assert!(UPSERT_FILE_SQL.contains("ON CONFLICT (hash) DO UPDATE"));
        assert!(UPSERT_FILE_SQL.contains("\"Files\".reference_count + 1"));
        assert!(UPSERT_FILE_SQL.contains("RETURNING id"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn concurrent_acquire_release_and_writeup_replace_preserve_one_reference() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("blob_refs_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."Files" (
                id SERIAL PRIMARY KEY,
                hash TEXT NOT NULL,
                upload_time_utc TIMESTAMPTZ NOT NULL,
                file_size BIGINT NOT NULL,
                name TEXT NOT NULL,
                reference_count BIGINT NOT NULL
            );
            CREATE UNIQUE INDEX ux_files_hash ON "{schema}"."Files"(hash);
            CREATE TABLE "{schema}"."Participations" (
                id INTEGER PRIMARY KEY,
                game_id INTEGER NOT NULL DEFAULT 1,
                writeup_id INTEGER REFERENCES "{schema}"."Files"(id)
            );
            CREATE TABLE "{schema}"."Attachments" (
                id INTEGER PRIMARY KEY,
                local_file_id INTEGER REFERENCES "{schema}"."Files"(id)
            );
            CREATE TABLE "{schema}"."AspNetUsers" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Teams" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Games" (id INTEGER PRIMARY KEY, poster_hash TEXT);
            CREATE TABLE "{schema}"."Configs" (config_key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE "{schema}"."GameChallenges" (
                id INTEGER PRIMARY KEY,
                original_archive_blob_path TEXT
            );
            "#
        );
        sqlx::raw_sql(&setup)
            .execute(&admin)
            .await
            .expect("create isolated blob schema");

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(32)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .expect("connect isolated blob pool");

        let hash = "a".repeat(64);
        let acquisitions = (0..64)
            .map(|_| {
                let pool = pool.clone();
                let hash = hash.clone();
                tokio::spawn(async move { acquire(&pool, &hash, "same.pdf", 10).await.unwrap() })
            })
            .collect::<Vec<_>>();
        let mut ids = Vec::new();
        for acquisition in acquisitions {
            ids.push(acquisition.await.expect("join acquisition"));
        }
        assert!(ids.iter().all(|id| *id == ids[0]));
        let count: i64 =
            sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
                .bind(&hash)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 64);

        let releases = (0..64)
            .map(|_| {
                let pool = pool.clone();
                let hash = hash.clone();
                tokio::spawn(async move { release_by_hash(&pool, &hash).await.unwrap() })
            })
            .collect::<Vec<_>>();
        let mut final_deletes = 0;
        for release in releases {
            let outcome = release.await.expect("join release");
            assert!(outcome.found);
            final_deletes += usize::from(outcome.deleted_hash.is_some());
        }
        assert_eq!(final_deletes, 1);
        let rows: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "Files"
                WHERE hash = $1 AND reference_count = 0"#,
        )
        .bind(&hash)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 1);

        // Physical deletion must be acknowledged before the durable zero-ref
        // tombstone is removed. A transient RWX/S3 failure remains retryable.
        let failed_hash = "d".repeat(64);
        acquire(&pool, &failed_hash, "retry.bin", 1).await.unwrap();
        release_by_hash(&pool, &failed_hash).await.unwrap();
        assert!(
            purge_if_unreferenced(&pool, &FailingDeleteStorage, &failed_hash)
                .await
                .is_err()
        );
        let pending: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "Files"
                WHERE hash = $1 AND reference_count = 0"#,
        )
        .bind(&failed_hash)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, 1);

        let old_hash = "b".repeat(64);
        let old_id = acquire(&pool, &old_hash, "old.pdf", 12).await.unwrap();
        sqlx::query(r#"INSERT INTO "Participations" (id, writeup_id) VALUES (1, $1)"#)
            .bind(old_id)
            .execute(&pool)
            .await
            .unwrap();
        let replacement_hash = "c".repeat(64);
        let replacements = (0..32)
            .map(|_| {
                let pool = pool.clone();
                let hash = replacement_hash.clone();
                tokio::spawn(async move {
                    replace_writeup(&pool, 1, &hash, "replacement.pdf", 14)
                        .await
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        for replacement in replacements {
            replacement.await.expect("join replacement");
        }
        let rows = sqlx::query_as::<_, (String, i64)>(
            r#"SELECT hash, reference_count FROM "Files"
                WHERE reference_count > 0 ORDER BY hash"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows, vec![(replacement_hash.clone(), 1)]);

        // A generic hash release cannot remove metadata that a participation
        // still owns. Concurrent game cleanup then detaches and consumes that
        // writeup exactly once.
        let guarded = release_by_hash(&pool, &replacement_hash).await.unwrap();
        assert!(guarded.found);
        assert!(guarded.deleted_hash.is_none());
        let cleaners = (0..2)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move { clear_game_writeups(&pool, 1).await.unwrap() })
            })
            .collect::<Vec<_>>();
        let mut cleared = Vec::new();
        for cleaner in cleaners {
            cleared.extend(cleaner.await.expect("join writeup cleaner"));
        }
        assert_eq!(cleared, vec![replacement_hash]);

        let attachment_hash = "e".repeat(64);
        let attachment_file = acquire(&pool, &attachment_hash, "attachment.zip", 20)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Attachments" (id, local_file_id) VALUES (1, $1)"#)
            .bind(attachment_file)
            .execute(&pool)
            .await
            .unwrap();
        let guarded = release_by_hash(&pool, &attachment_hash).await.unwrap();
        assert!(guarded.deleted_hash.is_none());
        let attachment_deletes = (0..2)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move { delete_attachment(&pool, 1).await.unwrap() })
            })
            .collect::<Vec<_>>();
        let mut deleted = Vec::new();
        for task in attachment_deletes {
            deleted.extend(task.await.expect("join attachment delete"));
        }
        assert_eq!(deleted, vec![attachment_hash]);

        // Force deletion to pause while holding the distributed hash lock.
        // A correct uploader cannot enter storage.store until deletion finishes;
        // once it does, it recreates the physical object before committing the
        // canonical metadata row.
        let storage = Arc::new(CoordinatedStorage::default());
        let bytes = b"delete-versus-store".to_vec();
        let coordinated_hash = sha256_hex(&bytes);
        storage.seed(coordinated_hash.clone());
        let delete_task = {
            let pool = pool.clone();
            let storage = storage.clone();
            let hash = coordinated_hash.clone();
            tokio::spawn(async move {
                purge_if_unreferenced(&pool, storage.as_ref(), &hash)
                    .await
                    .unwrap()
            })
        };
        storage.delete_started.notified().await;
        let upload_task = {
            let pool = pool.clone();
            let storage = storage.clone();
            tokio::spawn(async move {
                store_and_acquire(&pool, storage.as_ref(), "race.bin", &bytes)
                    .await
                    .unwrap()
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(storage.stores.load(Ordering::SeqCst), 0);
        storage.allow_delete.notify_one();
        assert!(delete_task.await.expect("join coordinated deletion"));
        upload_task.await.expect("join coordinated upload");
        assert_eq!(storage.stores.load(Ordering::SeqCst), 1);
        assert!(storage.exists(&coordinated_hash).await);
        let refs: i64 =
            sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
                .bind(&coordinated_hash)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(refs, 1);

        let direct_bytes = b"direct-owner";
        let direct_hash = sha256_hex(direct_bytes);
        storage.seed(direct_hash.clone());
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id, avatar_hash) VALUES (1, $1)"#)
            .bind(&direct_hash)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            !purge_if_unreferenced(&pool, storage.as_ref(), &direct_hash)
                .await
                .unwrap()
        );
        assert!(storage.exists(&direct_hash).await);
        sqlx::query(r#"UPDATE "AspNetUsers" SET avatar_hash = NULL WHERE id = 1"#)
            .execute(&pool)
            .await
            .unwrap();
        storage.allow_delete.notify_one();
        assert!(purge_if_unreferenced(&pool, storage.as_ref(), &direct_hash)
            .await
            .unwrap());
        assert!(!storage.exists(&direct_hash).await);

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup)
            .execute(&admin)
            .await
            .expect("drop isolated blob schema");
    }
}
