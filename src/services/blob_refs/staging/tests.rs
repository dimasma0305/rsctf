use super::*;
use crate::services::blob_refs::test_support::CoordinatedStorage;
use sqlx::postgres::PgPoolOptions;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

struct OneShotFailingDeleteStorage {
    failed_hash: String,
    fail_once: AtomicBool,
    deleted: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl BlobStorage for OneShotFailingDeleteStorage {
    async fn store(&self, _name: &str, _bytes: &[u8]) -> AppResult<StoredBlob> {
        Err(AppError::internal("not used"))
    }

    async fn load(&self, _hash: &str) -> AppResult<Vec<u8>> {
        Err(AppError::not_found("blob not found"))
    }

    async fn delete(&self, hash: &str) -> AppResult<()> {
        if hash == self.failed_hash && self.fail_once.swap(false, Ordering::SeqCst) {
            return Err(AppError::internal("simulated stage cleanup failure"));
        }
        self.deleted.lock().unwrap().push(hash.to_owned());
        Ok(())
    }

    async fn exists(&self, _hash: &str) -> bool {
        true
    }
}

#[test]
fn staging_limits_are_finite() {
    assert_ne!(LOCAL_STORE_JOBS, 0);
    assert!(DEPLOYMENT_STORE_JOBS > LOCAL_STORE_JOBS as i64);
    const { assert!(DEPLOYMENT_STAGE_RECORDS > DEPLOYMENT_STORE_JOBS) };
    assert!(DEPLOYMENT_STORE_BYTES >= crate::utils::upload::ASSET_TOTAL_BYTES as i64);
    assert!(STORE_DEADLINE <= Duration::from_secs(60));
}

#[test]
fn scoped_operations_are_stable_and_isolated() {
    let root = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
    assert_eq!(
        scoped_operation_id(root, "asset-upload", 0),
        scoped_operation_id(root, "asset-upload", 0)
    );
    assert_ne!(
        scoped_operation_id(root, "asset-upload", 0),
        scoped_operation_id(root, "asset-upload", 1)
    );
    assert_ne!(
        scoped_operation_id(root, "asset-upload", 0),
        scoped_operation_id(root, "challenge-import", 0)
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn expired_stage_cleanup_retains_failed_claim_and_continues_batch() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("blob_stage_cleanup_{}", Uuid::new_v4().simple());
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
            CREATE TABLE "Files" (
                id SERIAL PRIMARY KEY, hash TEXT NOT NULL UNIQUE,
                upload_time_utc TIMESTAMPTZ NOT NULL, file_size BIGINT NOT NULL,
                name TEXT NOT NULL, reference_count BIGINT NOT NULL
            );
            CREATE TABLE "Attachments" (id INTEGER PRIMARY KEY, local_file_id INTEGER);
            CREATE TABLE "Participations" (id INTEGER PRIMARY KEY, writeup_id INTEGER);
            CREATE TABLE "AdServiceSnapshots" (id INTEGER PRIMARY KEY, local_file_id INTEGER);
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
    crate::services::blob_refs::test_support::install_operation_tables(&pool).await;

    let failed_operation = Uuid::new_v4();
    let successful_operation = Uuid::new_v4();
    let failed_hash = sha256_hex(b"failed cleanup");
    let successful_hash = sha256_hex(b"successful cleanup");
    sqlx::query(
        r#"INSERT INTO "Files"
                   (hash, upload_time_utc, file_size, name, reference_count)
               VALUES ($1, clock_timestamp(), 14, 'failed.bin', 0),
                      ($2, clock_timestamp(), 18, 'successful.bin', 0)"#,
    )
    .bind(&failed_hash)
    .bind(&successful_hash)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "BlobStagingOperations"
                   (operation_id, owner_scope, owner_user_id, content_hash,
                    file_name, file_size, state, lease_expires_at_utc)
               VALUES ($1, 'cleanup-test:failed', NULL, $2, 'failed.bin', 14,
                       'Ready', clock_timestamp() - interval '3 minutes'),
                      ($3, 'cleanup-test:successful', NULL, $4, 'successful.bin', 18,
                       'Ready', clock_timestamp() - interval '2 minutes')"#,
    )
    .bind(failed_operation)
    .bind(&failed_hash)
    .bind(successful_operation)
    .bind(&successful_hash)
    .execute(&pool)
    .await
    .unwrap();
    let storage = OneShotFailingDeleteStorage {
        failed_hash: failed_hash.clone(),
        fail_once: AtomicBool::new(true),
        deleted: Mutex::new(Vec::new()),
    };

    assert_eq!(purge_expired_stages(&pool, &storage, 128).await.unwrap(), 1);
    let failed_claim: (String, bool, String) = sqlx::query_as(
        r#"SELECT state, lease_expires_at_utc > clock_timestamp(), last_error
                 FROM "BlobStagingOperations" WHERE operation_id = $1"#,
    )
    .bind(failed_operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failed_claim.0, "Failed");
    assert!(
        failed_claim.1,
        "the failed delete must retain a retry lease"
    );
    assert!(failed_claim.2.contains("simulated stage cleanup failure"));
    let successful_stage_count: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "BlobStagingOperations" WHERE operation_id = $1"#,
    )
    .bind(successful_operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        successful_stage_count, 0,
        "a later batch row must still run"
    );
    let remaining_files: Vec<String> =
        sqlx::query_scalar(r#"SELECT hash FROM "Files" ORDER BY hash"#)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(remaining_files, vec![failed_hash.clone()]);

    sqlx::query(
        r#"UPDATE "BlobStagingOperations"
                  SET lease_expires_at_utc = clock_timestamp() - interval '1 second'
                WHERE operation_id = $1"#,
    )
    .bind(failed_operation)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(purge_expired_stages(&pool, &storage, 128).await.unwrap(), 1);
    let remaining_stages: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*) FROM "BlobStagingOperations""#)
            .fetch_one(&pool)
            .await
            .unwrap();
    let remaining_files: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Files""#)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((remaining_stages, remaining_files), (0, 0));
    let deleted = storage.deleted.lock().unwrap().clone();
    assert!(deleted.contains(&successful_hash));
    assert!(deleted.contains(&failed_hash));

    let discard_operation = Uuid::new_v4();
    let discard_hash = sha256_hex(b"discard cleanup");
    sqlx::query(
        r#"INSERT INTO "Files"
                   (hash, upload_time_utc, file_size, name, reference_count)
               VALUES ($1, clock_timestamp(), 15, 'discard.bin', 0)"#,
    )
    .bind(&discard_hash)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "BlobStagingOperations"
                   (operation_id, owner_scope, owner_user_id, content_hash,
                    file_name, file_size, state, lease_expires_at_utc)
               VALUES ($1, 'cleanup-test:discard', NULL, $2, 'discard.bin', 15,
                       'Ready', clock_timestamp() + interval '15 minutes')"#,
    )
    .bind(discard_operation)
    .bind(&discard_hash)
    .execute(&pool)
    .await
    .unwrap();
    let discard_stage = StagedBlob {
        operation_id: discard_operation,
        owner_scope: "cleanup-test:discard".to_owned(),
        owner_user_id: None,
        blob: StoredBlob {
            hash: discard_hash.clone(),
            size: 15,
            name: "discard.bin".to_owned(),
        },
    };
    let discard_storage = OneShotFailingDeleteStorage {
        failed_hash: discard_hash,
        fail_once: AtomicBool::new(true),
        deleted: Mutex::new(Vec::new()),
    };
    assert!(
        discard_unpublished_stage(&pool, &discard_storage, &discard_stage)
            .await
            .is_err()
    );
    let retained_discard: (String, bool) = sqlx::query_as(
        r#"SELECT state, lease_expires_at_utc > clock_timestamp()
                 FROM "BlobStagingOperations" WHERE operation_id = $1"#,
    )
    .bind(discard_operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(retained_discard, ("Failed".to_owned(), true));

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn exact_replay_stores_once_and_publishes_one_reference() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("blob_stage_{}", Uuid::new_v4().simple());
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
    sqlx::query(
        r#"CREATE TABLE "Files" (
                   id SERIAL PRIMARY KEY, hash VARCHAR(64) NOT NULL UNIQUE,
                   upload_time_utc TIMESTAMPTZ NOT NULL, file_size BIGINT NOT NULL,
                   name TEXT NOT NULL, reference_count BIGINT NOT NULL
               )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TABLE "Attachments" (
               id INTEGER PRIMARY KEY, local_file_id INTEGER
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    crate::services::blob_refs::test_support::install_operation_tables(&pool).await;

    let storage = CoordinatedStorage::default();
    let owner = Uuid::new_v4();
    let operation = Uuid::new_v4();
    let first = stage_blob(
        &pool,
        &storage,
        operation,
        "asset-upload:test:0",
        Some(owner),
        "proof.bin",
        b"immutable",
    )
    .await
    .unwrap();
    let replay = stage_blob(
        &pool,
        &storage,
        operation,
        "asset-upload:test:0",
        Some(owner),
        "proof.bin",
        b"immutable",
    )
    .await
    .unwrap();
    assert_eq!(first.blob.hash, replay.blob.hash);
    assert_eq!(storage.stores.load(Ordering::SeqCst), 1);

    let mut first_publish = pool.begin().await.unwrap();
    let first_id = publish_staged_blob_for_owner(&mut first_publish, &first, "attachment:11")
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "Attachments" (id, local_file_id) VALUES (11, $1)"#)
        .bind(first_id)
        .execute(&mut *first_publish)
        .await
        .unwrap();
    first_publish.commit().await.unwrap();
    let mut replay_publish = pool.begin().await.unwrap();
    let replay_id = publish_staged_blob_for_owner(&mut replay_publish, &replay, "attachment:11")
        .await
        .unwrap();
    replay_publish.commit().await.unwrap();
    assert_eq!(first_id, replay_id);
    let references: i64 =
        sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE id = $1"#)
            .bind(first_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(references, 1);

    sqlx::query(r#"UPDATE "Attachments" SET local_file_id = NULL WHERE id = 11"#)
        .execute(&pool)
        .await
        .unwrap();
    let mut stale_owner = pool.begin().await.unwrap();
    let stale_error = publish_staged_blob_for_owner(&mut stale_owner, &replay, "attachment:11")
        .await
        .expect_err("a published receipt outlived its named attachment owner");
    assert_eq!(stale_error.status(), axum::http::StatusCode::CONFLICT);
    stale_owner.rollback().await.unwrap();
    sqlx::query(r#"UPDATE "Attachments" SET local_file_id = $1 WHERE id = 11"#)
        .bind(first_id)
        .execute(&pool)
        .await
        .unwrap();

    let mut wrong_owner = pool.begin().await.unwrap();
    let wrong_owner_error =
        publish_staged_blob_for_owner(&mut wrong_owner, &replay, "attachment:12")
            .await
            .expect_err("one upload receipt must not create a second owner");
    assert_eq!(wrong_owner_error.status(), axum::http::StatusCode::CONFLICT);
    wrong_owner.rollback().await.unwrap();

    let raced = stage_blob(
        &pool,
        &storage,
        Uuid::new_v4(),
        "asset-upload:race:0",
        Some(owner),
        "race.bin",
        b"immutable-race",
    )
    .await
    .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let publish = |scope: &'static str| {
        let pool = pool.clone();
        let staged = raced.clone();
        let barrier = barrier.clone();
        tokio::spawn(async move {
            let mut transaction = pool.begin().await.unwrap();
            barrier.wait().await;
            match publish_staged_blob_for_owner(&mut transaction, &staged, scope).await {
                Ok(file_id) => {
                    transaction.commit().await.unwrap();
                    Ok(file_id)
                }
                Err(error) => {
                    transaction.rollback().await.unwrap();
                    Err(error.status())
                }
            }
        })
    };
    let (left, right) = tokio::join!(publish("attachment:21"), publish("attachment:22"));
    let outcomes = [left.unwrap(), right.unwrap()];
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome == &&Err(axum::http::StatusCode::CONFLICT))
            .count(),
        1
    );
    let raced_references: i64 =
        sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
            .bind(&raced.blob.hash)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(raced_references, 1);

    let recovered = load_ready_upload_stage(&pool, operation, owner, &first.blob.hash)
        .await
        .unwrap();
    assert_eq!(recovered.operation_id, operation);

    let same_content = stage_blob(
        &pool,
        &storage,
        Uuid::new_v4(),
        "account-avatar",
        Some(owner),
        "proof.bin",
        b"immutable",
    )
    .await
    .unwrap();
    let mut no_op = pool.begin().await.unwrap();
    let no_op_id = consume_staged_blob_with_existing_reference(&mut no_op, &same_content)
        .await
        .unwrap();
    no_op.commit().await.unwrap();
    assert_eq!(no_op_id, first_id);
    let mut no_op_replay = pool.begin().await.unwrap();
    assert_eq!(
        consume_staged_blob_with_existing_reference(&mut no_op_replay, &same_content)
            .await
            .unwrap(),
        first_id
    );
    no_op_replay.commit().await.unwrap();
    let references_after_no_op: i64 =
        sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE id = $1"#)
            .bind(first_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(references_after_no_op, 1);

    sqlx::query(r#"UPDATE "Files" SET reference_count = 0 WHERE id = $1"#)
        .bind(first_id)
        .execute(&pool)
        .await
        .unwrap();
    let mut expired_owner_replay = pool.begin().await.unwrap();
    assert!(
        publish_staged_blob(&mut expired_owner_replay, &replay)
            .await
            .is_err(),
        "a publication receipt must not resurrect a released physical blob"
    );
    expired_owner_replay.rollback().await.unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
