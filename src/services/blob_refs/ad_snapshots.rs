//! Ref-counted blob ownership for immutable final A&D service snapshots.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::storage::BlobStorage;
use crate::utils::codec::sha256_hex;
use crate::utils::error::{AppError, AppResult};

use super::{acquire_locked, database_error, lock_hash, purge_if_unreferenced};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ServiceSnapshotBlob {
    pub id: i64,
    pub team_service_id: i32,
    pub hash: String,
    pub name: String,
    pub file_size: i64,
    pub source_container_id: String,
    pub captured_at_utc: DateTime<Utc>,
    pub expires_at_utc: Option<DateTime<Utc>>,
}

const SELECT_SNAPSHOT_SQL: &str = r#"
    SELECT snapshot.id, snapshot.team_service_id,
           file.hash, file.name, file.file_size,
           snapshot.source_container_id, snapshot.captured_at_utc,
           snapshot.expires_at_utc
      FROM "AdServiceSnapshots" snapshot
      JOIN "Files" file ON file.id = snapshot.local_file_id
     WHERE snapshot.team_service_id = $1
"#;

const SELECT_SNAPSHOT_FOR_UPDATE_SQL: &str = r#"
    SELECT snapshot.id, snapshot.team_service_id,
           file.hash, file.name, file.file_size,
           snapshot.source_container_id, snapshot.captured_at_utc,
           snapshot.expires_at_utc
      FROM "AdServiceSnapshots" snapshot
      JOIN "Files" file ON file.id = snapshot.local_file_id
     WHERE snapshot.team_service_id = $1
       FOR UPDATE OF snapshot, file
"#;

const SELECT_AVAILABLE_SNAPSHOT_SQL: &str = r#"
    SELECT snapshot.id, snapshot.team_service_id,
           file.hash, file.name, file.file_size,
           snapshot.source_container_id, snapshot.captured_at_utc,
           snapshot.expires_at_utc
      FROM "AdServiceSnapshots" snapshot
      JOIN "Files" file ON file.id = snapshot.local_file_id
     WHERE snapshot.team_service_id = $1
       AND (snapshot.expires_at_utc IS NULL
            OR snapshot.expires_at_utc > clock_timestamp())
       AND file.reference_count > 0
"#;

async fn select_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    team_service_id: i32,
) -> AppResult<Option<ServiceSnapshotBlob>> {
    sqlx::query_as::<_, ServiceSnapshotBlob>(SELECT_SNAPSHOT_FOR_UPDATE_SQL)
        .bind(team_service_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)
}

/// Store one immutable snapshot and acquire exactly one `Files` reference.
///
/// `None` means the configured retention deadline elapsed before capture. The
/// per-service advisory lock makes concurrent maintenance/API capture attempts
/// converge on the first committed blob without acquiring duplicate references.
pub async fn store_service_snapshot(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    team_service_id: i32,
    source_container_id: &str,
    expires_at_utc: Option<DateTime<Utc>>,
    name: &str,
    bytes: &[u8],
) -> AppResult<Option<ServiceSnapshotBlob>> {
    let expected_hash = sha256_hex(bytes);
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    let operation: AppResult<Option<ServiceSnapshotBlob>> = async {
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            &mut transaction,
            &format!("ad-service-snapshot:{team_service_id}"),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        let source_exists = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "AdTeamServices"
                    WHERE id = $1 AND container_id = $2
               )"#,
        )
        .bind(team_service_id)
        .bind(source_container_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        if !source_exists {
            return Err(AppError::conflict(
                "A&D service backend changed before snapshot publication",
            ));
        }

        let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(database_error)?;
        if let Some(existing) = select_snapshot(&mut transaction, team_service_id).await? {
            return Ok(existing
                .expires_at_utc
                .is_none_or(|expires| expires > now)
                .then_some(existing));
        }
        if expires_at_utc.is_some_and(|expires| expires <= now) {
            return Ok(None);
        }

        lock_hash(&mut transaction, &expected_hash)
            .await
            .map_err(database_error)?;
        let stored = storage.store(name, bytes).await?;
        if stored.hash != expected_hash {
            return Err(AppError::internal(
                "blob storage returned a hash that does not match its content",
            ));
        }
        let file_id = acquire_locked(&mut transaction, &stored.hash, &stored.name, stored.size)
            .await
            .map_err(database_error)?;
        let id = sqlx::query_scalar::<_, i64>(
            r#"INSERT INTO "AdServiceSnapshots"
                    (team_service_id, local_file_id, source_container_id,
                     captured_at_utc, expires_at_utc)
               VALUES ($1, $2, $3, clock_timestamp(), $4)
            RETURNING id"#,
        )
        .bind(team_service_id)
        .bind(file_id)
        .bind(source_container_id)
        .bind(expires_at_utc)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let snapshot = sqlx::query_as::<_, ServiceSnapshotBlob>(&format!(
            "{SELECT_SNAPSHOT_SQL} AND snapshot.id = $2"
        ))
        .bind(team_service_id)
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        Ok(Some(snapshot))
    }
    .await;

    let snapshot = match operation {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = transaction.rollback().await;
            if let Err(cleanup_error) = purge_if_unreferenced(pool, storage, &expected_hash).await {
                tracing::warn!(
                    %cleanup_error,
                    hash = %expected_hash,
                    "failed A&D snapshot rollback cleanup deferred"
                );
            }
            return Err(error);
        }
    };
    if let Err(error) = transaction.commit().await.map_err(database_error) {
        if let Err(cleanup_error) = purge_if_unreferenced(pool, storage, &expected_hash).await {
            tracing::warn!(
                %cleanup_error,
                hash = %expected_hash,
                "uncertain A&D snapshot cleanup deferred"
            );
        }
        return Err(error);
    }
    Ok(snapshot)
}

pub async fn load_service_snapshot(
    pool: &PgPool,
    team_service_id: i32,
) -> AppResult<Option<ServiceSnapshotBlob>> {
    sqlx::query_as::<_, ServiceSnapshotBlob>(SELECT_AVAILABLE_SNAPSHOT_SQL)
        .bind(team_service_id)
        .fetch_optional(pool)
        .await
        .map_err(database_error)
}

pub async fn available_service_snapshots(
    pool: &PgPool,
    team_service_ids: &[i32],
) -> AppResult<HashSet<i32>> {
    if team_service_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT snapshot.team_service_id
             FROM "AdServiceSnapshots" snapshot
             JOIN "Files" file ON file.id = snapshot.local_file_id
            WHERE snapshot.team_service_id = ANY($1)
              AND (snapshot.expires_at_utc IS NULL
                   OR snapshot.expires_at_utc > clock_timestamp())
              AND file.reference_count > 0"#,
    )
    .bind(team_service_ids)
    .fetch_all(pool)
    .await
    .map_err(database_error)?;
    Ok(ids.into_iter().collect())
}

/// Release a bounded batch of expired snapshot owners. The migration's delete
/// trigger decrements the matching `Files` reference for both explicit expiry
/// and any future parent-row cascade; physical storage is purged after commit.
pub async fn purge_expired_service_snapshots(
    pool: &PgPool,
    storage: &dyn BlobStorage,
    limit: i64,
) -> AppResult<u64> {
    let candidates = sqlx::query_as::<_, (i64, String)>(
        r#"SELECT snapshot.id, file.hash
             FROM "AdServiceSnapshots" snapshot
             JOIN "Files" file ON file.id = snapshot.local_file_id
            WHERE snapshot.expires_at_utc <= clock_timestamp()
            ORDER BY snapshot.expires_at_utc, snapshot.id
            LIMIT $1"#,
    )
    .bind(limit.clamp(1, 256))
    .fetch_all(pool)
    .await
    .map_err(database_error)?;

    let mut purged = 0;
    for (snapshot_id, hash) in candidates {
        let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
            .await
            .map_err(database_error)?;
        lock_hash(&mut transaction, &hash)
            .await
            .map_err(database_error)?;
        let deleted = sqlx::query(
            r#"DELETE FROM "AdServiceSnapshots"
                WHERE id = $1 AND expires_at_utc <= clock_timestamp()"#,
        )
        .bind(snapshot_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?
        .rows_affected();
        transaction.commit().await.map_err(database_error)?;
        if deleted == 0 {
            continue;
        }
        purged += 1;
        if let Err(error) = purge_if_unreferenced(pool, storage, &hash).await {
            tracing::warn!(%error, %hash, "expired A&D snapshot blob purge deferred");
        }
    }
    Ok(purged)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use uuid::Uuid;

    use super::*;
    use crate::storage::LocalBlobStorage;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn final_snapshot_is_idempotent_downloadable_and_expired_exactly_once() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("ad_snapshots_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Files" (
                id SERIAL PRIMARY KEY,
                hash TEXT NOT NULL UNIQUE,
                upload_time_utc TIMESTAMPTZ NOT NULL,
                file_size BIGINT NOT NULL,
                name TEXT NOT NULL,
                reference_count BIGINT NOT NULL
            );
            CREATE TABLE "AdTeamServices" (
                id INTEGER PRIMARY KEY,
                container_id TEXT
            );
            CREATE TABLE "AdServiceSnapshots" (
                id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
                team_service_id INTEGER NOT NULL UNIQUE,
                local_file_id INTEGER NOT NULL REFERENCES "Files"(id),
                source_container_id TEXT NOT NULL,
                captured_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                expires_at_utc TIMESTAMPTZ,
                CHECK (expires_at_utc IS NULL OR expires_at_utc > captured_at_utc)
            );
            CREATE FUNCTION rsctf_release_ad_service_snapshot_file()
            RETURNS TRIGGER LANGUAGE plpgsql AS $$
            BEGIN
                UPDATE "Files"
                   SET reference_count = GREATEST(reference_count - 1, 0)
                 WHERE id = OLD.local_file_id;
                RETURN OLD;
            END $$;
            CREATE TRIGGER trg_ad_service_snapshot_release
            AFTER DELETE ON "AdServiceSnapshots"
            FOR EACH ROW
            EXECUTE FUNCTION rsctf_release_ad_service_snapshot_file();
            CREATE TABLE "Attachments" (id INTEGER PRIMARY KEY, local_file_id INTEGER);
            CREATE TABLE "Participations" (id INTEGER PRIMARY KEY, writeup_id INTEGER);
            CREATE TABLE "AspNetUsers" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "Teams" (id INTEGER PRIMARY KEY, avatar_hash TEXT);
            CREATE TABLE "Games" (id INTEGER PRIMARY KEY, poster_hash TEXT);
            CREATE TABLE "Configs" (config_key TEXT PRIMARY KEY, value TEXT);
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY, original_archive_blob_path TEXT
            );
            INSERT INTO "AdTeamServices" VALUES (7, 'runtime-7');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let root =
            std::env::temp_dir().join(format!("rsctf-ad-snapshot-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let storage = LocalBlobStorage::new(&root);
        let bytes = b"final service filesystem";
        let expires = Utc::now() + chrono::Duration::days(7);
        let first = store_service_snapshot(
            &pool,
            &storage,
            7,
            "runtime-7",
            Some(expires),
            "service-7.tar",
            bytes,
        )
        .await
        .unwrap()
        .unwrap();
        let second = store_service_snapshot(
            &pool,
            &storage,
            7,
            "runtime-7",
            Some(expires),
            "service-7.tar",
            bytes,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(storage.load(&first.hash).await.unwrap(), bytes);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#,)
                .bind(&first.hash)
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );

        sqlx::query(
            r#"UPDATE "AdServiceSnapshots"
                  SET captured_at_utc = now() - interval '2 days',
                      expires_at_utc = now() - interval '1 day'
                WHERE id = $1"#,
        )
        .bind(first.id)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            purge_expired_service_snapshots(&pool, &storage, 16)
                .await
                .unwrap(),
            1
        );
        assert!(load_service_snapshot(&pool, 7).await.unwrap().is_none());
        assert!(!storage.exists(&first.hash).await);
        assert_eq!(
            sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM "Files""#)
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
        drop(storage);
        std::fs::remove_dir_all(root).unwrap();
    }
}
