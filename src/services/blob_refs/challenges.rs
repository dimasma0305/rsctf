//! Transactional cleanup for challenge-owned build archives.
//!
//! A challenge archive acquisition is one logical `Files` reference. Deleting
//! challenge owners and decrementing those references in separate transactions
//! leaks metadata on a crash and can race another replica. These helpers keep
//! both mutations under the existing per-hash advisory fences, then touch
//! physical storage only after commit and a fresh reachability check.

use std::collections::BTreeSet;

use sqlx::{Postgres, Transaction};

use crate::storage::{BlobStorage, StoredBlob};
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

use super::{acquire_locked, database_error, lock_hash, purge_if_unreferenced, release_locked};

const SELECT_ONE_SQL: &str = r#"
    SELECT original_archive_blob_path, ad_checker_image
      FROM "GameChallenges"
     WHERE id = $1
     FOR UPDATE
"#;

const DELETE_ONE_SQL: &str = r#"
    DELETE FROM "GameChallenges"
     WHERE id = $1
     RETURNING original_archive_blob_path, ad_checker_image
"#;

const SELECT_GAME_SQL: &str = r#"
    SELECT original_archive_blob_path, ad_checker_image
      FROM "GameChallenges"
     WHERE game_id = $1
     ORDER BY id
     FOR UPDATE
"#;

const DELETE_GAME_SQL: &str = r#"
    DELETE FROM "GameChallenges"
     WHERE game_id = $1
     RETURNING original_archive_blob_path, ad_checker_image
"#;

type ArtifactRow = (Option<String>, Option<String>);

/// Store (or clear) one challenge source archive while atomically swapping the
/// owning row and releasing its previous logical reference. The object write
/// occurs under the same hash fences as deletion. A post-commit purge handles
/// the old zero-reference tombstone without reopening the owner/refcount race.
pub async fn store_and_replace_challenge_archive(
    pool: &sqlx::PgPool,
    storage: &dyn BlobStorage,
    challenge_id: i32,
    archive: Option<(&str, &[u8])>,
) -> AppResult<Option<StoredBlob>> {
    let expected_hash = archive.map(|(_, bytes)| sha256_hex(bytes));
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let operation: AppResult<(Option<StoredBlob>, Option<String>)> = async {
        let old_hash = sqlx::query_scalar::<_, Option<String>>(
            r#"SELECT original_archive_blob_path
                 FROM "GameChallenges"
                WHERE id = $1
                FOR UPDATE"#,
        )
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;

        let mut hashes = std::collections::BTreeSet::new();
        if let Some(hash) = old_hash.as_deref().filter(|hash| !hash.trim().is_empty()) {
            hashes.insert(hash);
        }
        if let Some(hash) = expected_hash.as_deref() {
            hashes.insert(hash);
        }
        for hash in hashes {
            lock_hash(&mut transaction, hash)
                .await
                .map_err(database_error)?;
        }

        let stored = if let Some((name, bytes)) = archive {
            let blob = storage.store(name, bytes).await?;
            if Some(blob.hash.as_str()) != expected_hash.as_deref() {
                return Err(AppError::internal(
                    "blob storage returned a hash that does not match its content",
                ));
            }
            acquire_locked(&mut transaction, &blob.hash, name, blob.size)
                .await
                .map_err(database_error)?;
            Some(blob)
        } else {
            None
        };
        sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET original_archive_blob_path = $2
                WHERE id = $1"#,
        )
        .bind(challenge_id)
        .bind(stored.as_ref().map(|blob| blob.hash.as_str()))
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;

        let deleted_hash = if let Some(old_hash) = old_hash {
            let old_id = sqlx::query_scalar::<_, i32>(
                r#"SELECT id FROM "Files" WHERE hash = $1 FOR UPDATE"#,
            )
            .bind(&old_hash)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
            match old_id {
                Some(old_id) => {
                    release_locked(&mut transaction, old_id)
                        .await
                        .map_err(database_error)?
                        .deleted_hash
                }
                None => Some(old_hash),
            }
        } else {
            None
        };
        Ok((stored, deleted_hash))
    }
    .await;

    let (stored, deleted_hash) = match operation {
        Ok(result) => result,
        Err(error) => {
            let _ = transaction.rollback().await;
            if let Some(hash) = expected_hash.as_deref() {
                if let Err(cleanup_error) = purge_if_unreferenced(pool, storage, hash).await {
                    tracing::warn!(%cleanup_error, %hash, "failed challenge archive cleanup deferred");
                }
            }
            return Err(error);
        }
    };
    if let Err(error) = transaction.commit().await.map_err(database_error) {
        if let Some(hash) = expected_hash.as_deref() {
            if let Err(cleanup_error) = purge_if_unreferenced(pool, storage, hash).await {
                tracing::warn!(%cleanup_error, %hash, "uncertain challenge archive cleanup deferred");
            }
        }
        return Err(error);
    }
    if let Some(hash) = deleted_hash {
        if let Err(error) = purge_if_unreferenced(pool, storage, &hash).await {
            tracing::warn!(%error, %hash, "replaced challenge archive purge deferred");
        }
    }
    Ok(stored)
}

/// Challenge-owned artifacts returned by the authoritative `DELETE ..
/// RETURNING`. Archive hashes deliberately retain duplicates: two challenges
/// pointing at the same content acquired two references and release two.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeletedChallengeArtifacts {
    pub deleted: u64,
    pub archive_hashes: Vec<String>,
    pub checker_revisions: Vec<String>,
    /// Challenge and flag hand-out attachment rows made unreachable by the
    /// owner deletion. Post-commit cleanup may retry them independently.
    pub attachment_ids: Vec<i32>,
}

async fn lock_archive_hashes(
    transaction: &mut Transaction<'_, Postgres>,
    rows: &[ArtifactRow],
) -> AppResult<()> {
    // Lock unique hashes in lexical order only to avoid deadlocks. Reference
    // releases below still iterate every returned owner and are not distinct.
    let hashes = rows
        .iter()
        .filter_map(|(hash, _)| hash.as_deref())
        .filter(|hash| !hash.trim().is_empty())
        .collect::<BTreeSet<_>>();
    for hash in hashes {
        lock_hash(transaction, hash).await.map_err(database_error)?;
    }
    Ok(())
}

async fn release_archive_hashes(
    transaction: &mut Transaction<'_, Postgres>,
    rows: &[ArtifactRow],
) -> AppResult<()> {
    for hash in rows
        .iter()
        .filter_map(|(hash, _)| hash.as_deref())
        .filter(|hash| !hash.trim().is_empty())
    {
        let file_id =
            sqlx::query_scalar::<_, i32>(r#"SELECT id FROM "Files" WHERE hash = $1 FOR UPDATE"#)
                .bind(hash)
                .fetch_optional(&mut **transaction)
                .await
                .map_err(database_error)?;
        if let Some(file_id) = file_id {
            release_locked(transaction, file_id)
                .await
                .map_err(database_error)?;
        }
    }
    Ok(())
}

fn artifacts(rows: Vec<ArtifactRow>, mut attachment_ids: Vec<i32>) -> DeletedChallengeArtifacts {
    let deleted = rows.len() as u64;
    let mut archive_hashes = Vec::new();
    let mut checker_revisions = Vec::new();
    for (archive, checker) in rows {
        if let Some(archive) = archive.filter(|value| !value.trim().is_empty()) {
            archive_hashes.push(archive);
        }
        if let Some(checker) = checker.filter(|value| !value.trim().is_empty()) {
            checker_revisions.push(checker);
        }
    }
    attachment_ids.sort_unstable();
    attachment_ids.dedup();
    DeletedChallengeArtifacts {
        deleted,
        archive_hashes,
        checker_revisions,
        attachment_ids,
    }
}

async fn purge_archives(pool: &sqlx::PgPool, storage: &dyn BlobStorage, hashes: &[String]) {
    // Purging each content hash once is sufficient after every logical release
    // committed. This deduplication is physical cleanup only, never refcounting.
    for hash in hashes.iter().collect::<BTreeSet<_>>() {
        if let Err(error) = purge_if_unreferenced(pool, storage, hash).await {
            tracing::warn!(%error, %hash, "challenge build archive purge deferred");
        }
    }
}

/// Delete one challenge and its flag rows while atomically releasing its build
/// archive reference. Immutable checker directories are returned only as audit
/// evidence; delayed checker GC decides when an unreachable revision is safe.
pub async fn delete_challenge(
    pool: &sqlx::PgPool,
    storage: &dyn BlobStorage,
    challenge_id: i32,
) -> AppResult<DeletedChallengeArtifacts> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let result = delete_challenge_locked(&mut transaction, challenge_id).await?;
    transaction.commit().await.map_err(database_error)?;

    purge_deleted_challenge_artifacts(pool, storage, &result).await;
    Ok(result)
}

/// Delete one challenge inside the caller's already-fenced definition
/// transaction. The final evidence predicate and owner/refcount release can
/// therefore commit as one indivisible operation.
pub(crate) async fn delete_challenge_locked(
    transaction: &mut Transaction<'_, Postgres>,
    challenge_id: i32,
) -> AppResult<DeletedChallengeArtifacts> {
    let selected = sqlx::query_as::<_, ArtifactRow>(SELECT_ONE_SQL)
        .bind(challenge_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    lock_archive_hashes(transaction, &selected).await?;
    let attachment_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT attachment_id
             FROM "GameChallenges"
            WHERE id = $1 AND attachment_id IS NOT NULL
            UNION ALL
           SELECT attachment_id
             FROM "FlagContexts"
            WHERE challenge_id = $1 AND attachment_id IS NOT NULL"#,
    )
    .bind(challenge_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(r#"DELETE FROM "FlagContexts" WHERE challenge_id = $1"#)
        .bind(challenge_id)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    let deleted = sqlx::query_as::<_, ArtifactRow>(DELETE_ONE_SQL)
        .bind(challenge_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    release_archive_hashes(transaction, &deleted).await?;
    Ok(artifacts(deleted, attachment_ids))
}

/// Delete all challenges in one game for a repository re-scan. The returned
/// archive list is intentionally not distinct because each deleted owner
/// consumes exactly one acquired reference.
pub async fn delete_game_challenges(
    pool: &sqlx::PgPool,
    storage: &dyn BlobStorage,
    game_id: i32,
) -> AppResult<DeletedChallengeArtifacts> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let result = delete_game_challenges_locked(&mut transaction, game_id).await?;
    transaction.commit().await.map_err(database_error)?;

    purge_deleted_challenge_artifacts(pool, storage, &result).await;
    Ok(result)
}

/// Delete every challenge owner inside the caller's game-deletion transaction.
/// This keeps archive refcounts and the final `Games` delete all-or-nothing.
pub(crate) async fn delete_game_challenges_locked(
    transaction: &mut Transaction<'_, Postgres>,
    game_id: i32,
) -> AppResult<DeletedChallengeArtifacts> {
    let selected = sqlx::query_as::<_, ArtifactRow>(SELECT_GAME_SQL)
        .bind(game_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    lock_archive_hashes(transaction, &selected).await?;
    let attachment_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT attachment_id
             FROM "GameChallenges"
            WHERE game_id = $1 AND attachment_id IS NOT NULL
            UNION ALL
           SELECT flag.attachment_id
             FROM "FlagContexts" flag
             JOIN "GameChallenges" challenge ON challenge.id = flag.challenge_id
            WHERE challenge.game_id = $1 AND flag.attachment_id IS NOT NULL"#,
    )
    .bind(game_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"DELETE FROM "FlagContexts" flag
            USING "GameChallenges" challenge
            WHERE flag.challenge_id = challenge.id
              AND challenge.game_id = $1"#,
    )
    .bind(game_id)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    let deleted = sqlx::query_as::<_, ArtifactRow>(DELETE_GAME_SQL)
        .bind(game_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(database_error)?;
    release_archive_hashes(transaction, &deleted).await?;
    Ok(artifacts(deleted, attachment_ids))
}

pub(crate) async fn purge_deleted_challenge_artifacts(
    pool: &sqlx::PgPool,
    storage: &dyn BlobStorage,
    result: &DeletedChallengeArtifacts,
) {
    purge_archives(pool, storage, &result.archive_hashes).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingStorage(AtomicUsize);

    #[async_trait]
    impl BlobStorage for CountingStorage {
        async fn store(&self, _name: &str, _bytes: &[u8]) -> AppResult<crate::storage::StoredBlob> {
            unreachable!("cleanup test never stores")
        }

        async fn load(&self, _hash: &str) -> AppResult<Vec<u8>> {
            unreachable!("cleanup test never loads")
        }

        async fn delete(&self, _hash: &str) -> AppResult<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn exists(&self, _hash: &str) -> bool {
            false
        }
    }

    #[test]
    fn owner_deletes_return_both_artifacts_without_distinct_refcounts() {
        for sql in [DELETE_ONE_SQL, DELETE_GAME_SQL] {
            assert!(sql.contains("RETURNING original_archive_blob_path, ad_checker_image"));
            assert!(!sql.to_ascii_uppercase().contains("DISTINCT"));
        }
    }

    #[test]
    fn duplicate_archives_remain_duplicate_logical_releases() {
        let rows = vec![
            (Some("same".to_string()), Some("checker-a".to_string())),
            (Some("same".to_string()), Some("checker-b".to_string())),
        ];
        let result = artifacts(rows, vec![9, 7, 9]);
        assert_eq!(result.archive_hashes, vec!["same", "same"]);
        assert_eq!(result.checker_revisions, vec!["checker-a", "checker-b"]);
        assert_eq!(result.attachment_ids, vec![7, 9]);
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn game_delete_releases_duplicate_archive_owners_once_each() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("challenge_artifacts_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."Files" (
                id SERIAL PRIMARY KEY, hash TEXT NOT NULL UNIQUE,
                upload_time_utc TIMESTAMPTZ NOT NULL DEFAULT now(),
                file_size BIGINT NOT NULL DEFAULT 1, name TEXT NOT NULL DEFAULT '',
                reference_count BIGINT NOT NULL
            );
            CREATE TABLE "{schema}"."Attachments" (id INTEGER PRIMARY KEY, local_file_id INTEGER);
            CREATE TABLE "{schema}"."Participations" (id INTEGER PRIMARY KEY, writeup_id INTEGER);
            CREATE TABLE "{schema}"."AspNetUsers" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Teams" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Games" (id INTEGER PRIMARY KEY, poster_hash TEXT);
            CREATE TABLE "{schema}"."Configs" (config_key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE "{schema}"."GameChallenges" (
                id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
                original_archive_blob_path TEXT, ad_checker_image TEXT,
                attachment_id INTEGER
            );
            CREATE TABLE "{schema}"."FlagContexts" (
                id INTEGER PRIMARY KEY, challenge_id INTEGER, attachment_id INTEGER
            );
            INSERT INTO "{schema}"."Files" (hash, reference_count)
            VALUES ('archive', 2);
            INSERT INTO "{schema}"."GameChallenges" VALUES
                (1, 7, 'archive', '/checkers/a', NULL),
                (2, 7, 'archive', '/checkers/b', NULL);
            INSERT INTO "{schema}"."FlagContexts" VALUES (1, 1, NULL), (2, 2, NULL);
            "#
        );
        sqlx::raw_sql(&setup).execute(&admin).await.unwrap();

        let search_path = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let sql = format!(r#"SET search_path TO "{search_path}""#);
                Box::pin(async move {
                    sqlx::query(&sql).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();
        let storage = CountingStorage::default();
        let result = delete_game_challenges(&pool, &storage, 7).await.unwrap();
        assert_eq!(result.deleted, 2);
        assert_eq!(result.archive_hashes, vec!["archive", "archive"]);
        assert_eq!(result.checker_revisions, vec!["/checkers/a", "/checkers/b"]);
        assert_eq!(storage.0.load(Ordering::SeqCst), 1);
        let files: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Files""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        let challenges: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "GameChallenges""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        let flags: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "FlagContexts""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((files, challenges, flags), (0, 0, 0));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
