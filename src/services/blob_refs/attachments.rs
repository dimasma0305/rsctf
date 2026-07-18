//! Replica-safe cleanup of attachment rows that have no domain owner.

use std::collections::{BTreeMap, BTreeSet};

use crate::storage::BlobStorage;
use crate::utils::error::AppResult;

use super::{database_error, lock_hash, purge_if_unreferenced, release_locked};

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
