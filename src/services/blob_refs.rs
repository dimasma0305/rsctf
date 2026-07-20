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
#[cfg(test)]
mod poster_tests;
mod seaorm;
mod writeups;
pub(crate) use attachments::delete_attachment_locked;
pub use attachments::{
    delete_attachment, delete_orphan_attachments, store_and_replace_challenge_attachment,
};
pub use challenges::{
    delete_challenge, delete_game_challenges, store_and_replace_challenge_archive,
    DeletedChallengeArtifacts,
};
pub(crate) use challenges::{
    delete_challenge_locked, delete_game_challenges_locked, purge_deleted_challenge_artifacts,
};
pub(crate) use seaorm::store_and_acquire_in_seaorm_transaction;
#[cfg(test)]
use writeups::replace_writeup;
pub use writeups::{clear_game_writeups, store_and_replace_writeup};

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

/// Release a hash reference inside a caller-owned transaction after its direct
/// owner row (for example, `Games.poster_hash`) has been detached or deleted.
/// Keeping both changes in one transaction prevents a committed owner deletion
/// from leaking its logical blob reference if the metadata update fails.
pub(crate) async fn release_direct_hash_locked(
    transaction: &mut Transaction<'_, Postgres>,
    hash: &str,
) -> AppResult<ReleaseOutcome> {
    lock_hash(transaction, hash).await.map_err(database_error)?;
    let id = sqlx::query_scalar::<_, i32>(r#"SELECT id FROM "Files" WHERE hash = $1 FOR UPDATE"#)
        .bind(hash)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)?;
    match id {
        Some(id) => release_locked(transaction, id)
            .await
            .map_err(database_error),
        None => Ok(ReleaseOutcome {
            found: false,
            deleted_hash: None,
        }),
    }
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
    use crate::utils::enums::{ParticipationStatus, Role};
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
    async fn committed_game_deletion_fence_rejects_a_delayed_writeup_before_storage() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("writeup_fence_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let search_path = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(3)
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
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL,
              start_time_utc TIMESTAMPTZ NOT NULL, writeup_required BOOLEAN NOT NULL,
              writeup_deadline TIMESTAMPTZ NOT NULL
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY, deletion_pending BOOLEAN NOT NULL
            );
            CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY, role SMALLINT NOT NULL);
            CREATE TABLE "Files" (
              id SERIAL PRIMARY KEY, hash TEXT NOT NULL UNIQUE,
              upload_time_utc TIMESTAMPTZ NOT NULL, file_size BIGINT NOT NULL,
              name TEXT NOT NULL, reference_count BIGINT NOT NULL
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL,
              writeup_id INTEGER REFERENCES "Files"(id)
            );
            CREATE TABLE "UserParticipations" (
              user_id UUID NOT NULL, game_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let user_id = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "Games" VALUES
               (1, FALSE, clock_timestamp() - interval '1 hour', TRUE,
                clock_timestamp() + interval '1 hour')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(r#"INSERT INTO "Teams" VALUES (2, FALSE)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2)"#)
            .bind(user_id)
            .bind(Role::User as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Participations" VALUES (3, 1, 2, $1, NULL)"#)
            .bind(ParticipationStatus::Accepted as i16)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 1, 3)"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();

        let mut deletion = pool.begin().await.unwrap();
        sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
            .execute(&mut *deletion)
            .await
            .unwrap();
        let storage = Arc::new(CoordinatedStorage::default());
        let mut upload = tokio::spawn({
            let pool = pool.clone();
            let storage = Arc::clone(&storage);
            async move {
                store_and_replace_writeup(
                    &pool,
                    storage.as_ref(),
                    1,
                    3,
                    user_id,
                    "writeup.pdf",
                    b"%PDF-1.7",
                )
                .await
            }
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut upload)
                .await
                .is_err(),
            "writeup crossed the uncommitted game deletion fence"
        );
        deletion.commit().await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), upload)
                .await
                .unwrap()
                .unwrap()
                .is_err(),
            "writeup ignored the committed game deletion fence"
        );
        assert_eq!(storage.stores.load(Ordering::SeqCst), 0);
        let state: (i64, Option<i32>) = sqlx::query_as(
            r#"SELECT (SELECT COUNT(*) FROM "Files"), writeup_id
                 FROM "Participations" WHERE id = 3"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, (0, None));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
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
                original_archive_blob_path TEXT,
                attachment_id INTEGER REFERENCES "{schema}"."Attachments"(id)
            );
            CREATE TABLE "{schema}"."FlagContexts" (
                id INTEGER PRIMARY KEY,
                attachment_id INTEGER REFERENCES "{schema}"."Attachments"(id)
            );
            CREATE TABLE "{schema}"."ExerciseChallenges" (
                id INTEGER PRIMARY KEY,
                attachment_id INTEGER REFERENCES "{schema}"."Attachments"(id)
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

        let owned_attachment_hash = "f".repeat(64);
        let owned_attachment_file =
            acquire(&pool, &owned_attachment_hash, "owned-attachment.zip", 21)
                .await
                .unwrap();
        sqlx::query(r#"INSERT INTO "Attachments" (id, local_file_id) VALUES (2, $1)"#)
            .bind(owned_attachment_file)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" (id, attachment_id) VALUES (1, 2)"#)
            .execute(&pool)
            .await
            .unwrap();
        assert!(delete_attachment(&pool, 2).await.unwrap().is_none());
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Attachments" WHERE id = 2"#)
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        sqlx::query(r#"DELETE FROM "GameChallenges" WHERE id = 1"#)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            delete_attachment(&pool, 2).await.unwrap(),
            Some(owned_attachment_hash)
        );

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
