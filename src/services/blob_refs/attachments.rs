//! Replica-safe cleanup of attachment rows that have no domain owner.

use std::collections::{BTreeMap, BTreeSet};

use crate::storage::BlobStorage;
use crate::utils::codec::sha256_hex;
use crate::utils::enums::FileType;
use crate::utils::error::{AppError, AppResult};

use super::{acquire_locked, database_error, lock_hash, purge_if_unreferenced, release_locked};

const SELECT_ORPHANS_SQL: &str = r#"
    SELECT attachment.id, attachment.local_file_id, file.hash
      FROM "Attachments" attachment
      LEFT JOIN "Files" file ON file.id = attachment.local_file_id
     WHERE NOT EXISTS (
               SELECT 1 FROM "GameChallenges" challenge
                WHERE challenge.attachment_id = attachment.id
           )
       AND NOT EXISTS (
               SELECT 1 FROM "FlagContexts" flag
                WHERE flag.attachment_id = attachment.id
           )
       AND NOT EXISTS (
               SELECT 1 FROM "ExerciseChallenges" exercise
                WHERE exercise.attachment_id = attachment.id
           )
     ORDER BY attachment.id
     FOR UPDATE OF attachment
"#;

const DELETE_ORPHANS_SQL: &str = r#"
    DELETE FROM "Attachments" attachment
     WHERE NOT EXISTS (
               SELECT 1 FROM "GameChallenges" challenge
                WHERE challenge.attachment_id = attachment.id
           )
       AND NOT EXISTS (
               SELECT 1 FROM "FlagContexts" flag
                WHERE flag.attachment_id = attachment.id
           )
       AND NOT EXISTS (
               SELECT 1 FROM "ExerciseChallenges" exercise
                WHERE exercise.attachment_id = attachment.id
           )
     RETURNING attachment.id, attachment.local_file_id
"#;

/// Atomically replace (or clear) the attachment owned by one challenge. The
/// owner FK swap, old Attachment deletion, and both Files reference mutations
/// commit together; physical purges remain safe, retryable post-commit work.
pub async fn store_and_replace_challenge_attachment(
    pool: &sqlx::PgPool,
    storage: &dyn BlobStorage,
    challenge_id: i32,
    artifact: Option<(&str, &[u8])>,
    replace_existing: bool,
) -> AppResult<()> {
    let expected_hash = artifact.map(|(_, bytes)| sha256_hex(bytes));
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let operation: AppResult<Option<String>> = async {
        let current = sqlx::query_as::<_, (Option<i32>, Option<i32>, Option<String>)>(
            r#"SELECT challenge.attachment_id, attachment.local_file_id, file.hash
                 FROM "GameChallenges" challenge
                 LEFT JOIN "Attachments" attachment
                   ON attachment.id = challenge.attachment_id
                 LEFT JOIN "Files" file ON file.id = attachment.local_file_id
                WHERE challenge.id = $1
                FOR UPDATE OF challenge"#,
        )
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
        let (old_attachment_id, _old_file_id, old_hash) = current;
        let already_applied = match artifact {
            Some(_) => {
                old_attachment_id.is_some() && old_hash.as_deref() == expected_hash.as_deref()
            }
            None => old_attachment_id.is_none(),
        };
        if already_applied {
            return Ok(None);
        }
        if old_attachment_id.is_some() && !replace_existing {
            return Err(AppError::conflict(
                "challenge attachment was populated concurrently",
            ));
        }

        let mut hashes = BTreeSet::new();
        hashes.extend(old_hash.iter().map(String::as_str));
        hashes.extend(expected_hash.iter().map(String::as_str));
        for hash in hashes {
            lock_hash(&mut transaction, hash)
                .await
                .map_err(database_error)?;
        }

        let new_attachment_id = if let Some((name, bytes)) = artifact {
            let blob = storage.store(name, bytes).await?;
            if Some(blob.hash.as_str()) != expected_hash.as_deref() {
                return Err(AppError::internal(
                    "blob storage returned a hash that does not match its content",
                ));
            }
            let file_id = acquire_locked(&mut transaction, &blob.hash, name, blob.size)
                .await
                .map_err(database_error)?;
            Some(
                sqlx::query_scalar::<_, i32>(
                    r#"INSERT INTO "Attachments" ("Type", remote_url, local_file_id)
                       VALUES ($1, NULL, $2)
                       RETURNING id"#,
                )
                .bind(FileType::Local as i16)
                .bind(file_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?,
            )
        } else {
            None
        };

        sqlx::query(r#"UPDATE "GameChallenges" SET attachment_id = $2 WHERE id = $1"#)
            .bind(challenge_id)
            .bind(new_attachment_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;

        let released_file = match old_attachment_id {
            Some(old_attachment_id) => sqlx::query_scalar::<_, Option<i32>>(
                r#"DELETE FROM "Attachments" attachment
                    WHERE attachment.id = $1
                      AND NOT EXISTS (
                            SELECT 1 FROM "GameChallenges" challenge
                             WHERE challenge.attachment_id = attachment.id
                      )
                      AND NOT EXISTS (
                            SELECT 1 FROM "FlagContexts" flag
                             WHERE flag.attachment_id = attachment.id
                      )
                      AND NOT EXISTS (
                            SELECT 1 FROM "ExerciseChallenges" exercise
                             WHERE exercise.attachment_id = attachment.id
                      )
                    RETURNING attachment.local_file_id"#,
            )
            .bind(old_attachment_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?
            .flatten(),
            None => None,
        };
        match released_file {
            Some(file_id) => Ok(release_locked(&mut transaction, file_id)
                .await
                .map_err(database_error)?
                .deleted_hash),
            None => Ok(None),
        }
    }
    .await;

    let deleted_hash = match operation {
        Ok(deleted_hash) => deleted_hash,
        Err(error) => {
            let _ = transaction.rollback().await;
            if let Some(hash) = expected_hash.as_deref() {
                let _ = purge_if_unreferenced(pool, storage, hash).await;
            }
            return Err(error);
        }
    };
    if let Err(error) = transaction.commit().await.map_err(database_error) {
        if let Some(hash) = expected_hash.as_deref() {
            let _ = purge_if_unreferenced(pool, storage, hash).await;
        }
        return Err(error);
    }
    if let Some(hash) = deleted_hash {
        if let Err(error) = purge_if_unreferenced(pool, storage, &hash).await {
            tracing::warn!(%error, %hash, "replaced challenge attachment purge deferred");
        }
    }
    Ok(())
}

/// Remove unowned attachment rows and consume exactly one blob reference for
/// each returned local attachment in the same transaction. Physical deletion
/// occurs only after commit; zero-reference `Files` tombstones make failures
/// and process crashes retryable by maintenance.
pub async fn delete_orphan_attachments(
    pool: &sqlx::PgPool,
    storage: &dyn BlobStorage,
) -> AppResult<u64> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let selected = sqlx::query_as::<_, (i32, Option<i32>, Option<String>)>(SELECT_ORPHANS_SQL)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
    let file_hashes = selected
        .iter()
        .filter_map(|(_, file_id, hash)| Some(((*file_id)?, hash.clone()?)))
        .collect::<BTreeMap<_, _>>();
    for hash in file_hashes.values().collect::<BTreeSet<_>>() {
        lock_hash(&mut transaction, hash)
            .await
            .map_err(database_error)?;
    }

    // Re-evaluate owner reachability in the authoritative DELETE. A concurrent
    // link that appeared after the candidate scan therefore prevents both row
    // deletion and the matching reference decrement.
    let deleted = sqlx::query_as::<_, (i32, Option<i32>)>(DELETE_ORPHANS_SQL)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
    let mut purge_hashes = BTreeSet::new();
    for (_, file_id) in &deleted {
        let Some(file_id) = file_id else {
            continue;
        };
        if let Some(hash) = file_hashes.get(file_id) {
            release_locked(&mut transaction, *file_id)
                .await
                .map_err(database_error)?;
            purge_hashes.insert(hash.clone());
        }
    }
    transaction.commit().await.map_err(database_error)?;

    for hash in purge_hashes {
        if let Err(error) = purge_if_unreferenced(pool, storage, &hash).await {
            tracing::warn!(%error, %hash, "orphan attachment blob purge deferred");
        }
    }
    Ok(deleted.len() as u64)
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
    fn authoritative_delete_rechecks_owners_and_returns_each_file_reference() {
        assert!(DELETE_ORPHANS_SQL.contains("RETURNING attachment.id, attachment.local_file_id"));
        assert!(DELETE_ORPHANS_SQL.contains("GameChallenges"));
        assert!(DELETE_ORPHANS_SQL.contains("FlagContexts"));
        assert!(DELETE_ORPHANS_SQL.contains("ExerciseChallenges"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn orphan_delete_consumes_only_the_returned_attachment_reference() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("orphan_attachments_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."Files" (
                id INTEGER PRIMARY KEY, hash TEXT NOT NULL UNIQUE,
                reference_count BIGINT NOT NULL
            );
            CREATE TABLE "{schema}"."Attachments" (id INTEGER PRIMARY KEY, local_file_id INTEGER);
            CREATE TABLE "{schema}"."Participations" (id INTEGER PRIMARY KEY, writeup_id INTEGER);
            CREATE TABLE "{schema}"."AspNetUsers" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Teams" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "{schema}"."Games" (id INTEGER PRIMARY KEY, poster_hash TEXT);
            CREATE TABLE "{schema}"."Configs" (config_key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE "{schema}"."GameChallenges" (
                id INTEGER PRIMARY KEY, attachment_id INTEGER,
                original_archive_blob_path TEXT
            );
            CREATE TABLE "{schema}"."FlagContexts" (id INTEGER PRIMARY KEY, attachment_id INTEGER);
            CREATE TABLE "{schema}"."ExerciseChallenges" (id INTEGER PRIMARY KEY, attachment_id INTEGER);
            INSERT INTO "{schema}"."Files" VALUES
                (1, 'orphan', 1), (2, 'owned', 1);
            INSERT INTO "{schema}"."Attachments" VALUES (1, 1), (2, 2);
            INSERT INTO "{schema}"."GameChallenges" VALUES (1, 2, NULL);
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
        assert_eq!(delete_orphan_attachments(&pool, &storage).await.unwrap(), 1);
        assert_eq!(storage.0.load(Ordering::SeqCst), 1);
        let attachments: Vec<i32> =
            sqlx::query_scalar(r#"SELECT id FROM "Attachments" ORDER BY id"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        let files: Vec<(String, i64)> =
            sqlx::query_as(r#"SELECT hash, reference_count FROM "Files" ORDER BY hash"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(attachments, vec![2]);
        assert_eq!(files, vec![("owned".to_string(), 1)]);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
