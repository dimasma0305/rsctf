use super::test_support::{CoordinatedStorage, FailingDeleteStorage};
use super::*;
use crate::utils::enums::{ParticipationStatus, Role};
use sqlx::postgres::PgPoolOptions;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[test]
fn acquisition_is_an_atomic_conflict_increment() {
    assert!(UPSERT_FILE_SQL.contains("ON CONFLICT (hash) DO UPDATE"));
    assert!(UPSERT_FILE_SQL.contains("\"Files\".reference_count + 1"));
    assert!(UPSERT_FILE_SQL.contains("RETURNING id"));
}

#[test]
fn direct_owner_swaps_lock_distinct_hashes_in_canonical_order() {
    assert_eq!(
        canonical_hash_order(["bbbb", "aaaa", "bbbb", "cccc"]),
        vec!["aaaa", "bbbb", "cccc"]
    );
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
          start_time_utc TIMESTAMPTZ NOT NULL, end_time_utc TIMESTAMPTZ NOT NULL,
          writeup_required BOOLEAN NOT NULL,
          writeup_deadline TIMESTAMPTZ NOT NULL
        );
        CREATE TABLE "Teams" (
          id INTEGER PRIMARY KEY, captain_id UUID NOT NULL,
          deletion_pending BOOLEAN NOT NULL
        );
        CREATE TABLE "TeamMembers" (
          team_id INTEGER NOT NULL, user_id UUID NOT NULL,
          PRIMARY KEY (team_id, user_id)
        );
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY, role SMALLINT NOT NULL,
          email_confirmed BOOLEAN NOT NULL, security_stamp TEXT
        );
        CREATE TABLE "Files" (
          id SERIAL PRIMARY KEY, hash TEXT NOT NULL UNIQUE,
          upload_time_utc TIMESTAMPTZ NOT NULL, file_size BIGINT NOT NULL,
          name TEXT NOT NULL, reference_count BIGINT NOT NULL
        );
        CREATE TABLE "AdServiceSnapshots" (
          id BIGSERIAL PRIMARY KEY, local_file_id INTEGER NOT NULL
        );
        CREATE TABLE "Participations" (
          id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL, status SMALLINT NOT NULL,
          writeup_id INTEGER REFERENCES "Files"(id)
        );
        CREATE TABLE "UserParticipations" (
          user_id UUID NOT NULL, game_id INTEGER NOT NULL,
          team_id INTEGER NOT NULL, participation_id INTEGER NOT NULL
        );
        CREATE TABLE "IdentityObservations" (
          user_id UUID NOT NULL, game_id INTEGER,
          team_id INTEGER, participation_id INTEGER,
          observed_at_utc TIMESTAMPTZ NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    test_support::install_operation_tables(&pool).await;
    let user_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "Games" VALUES
           (1, FALSE, clock_timestamp() - interval '1 hour',
            clock_timestamp() + interval '1 hour', TRUE,
            clock_timestamp() + interval '1 hour')"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"INSERT INTO "Teams" VALUES (2, $1, FALSE)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2, TRUE, 'stamp')"#)
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
    sqlx::query(r#"INSERT INTO "UserParticipations" VALUES ($1, 1, 2, 3)"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO "IdentityObservations"
             (user_id, game_id, team_id, participation_id, observed_at_utc)
           VALUES ($1, 1, 2, 3, clock_timestamp())"#,
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();

    let storage = Arc::new(CoordinatedStorage::default());
    let caller = crate::services::live_roster::LiveParticipationIdentity {
        user_id,
        expected_security_stamp: "stamp",
        game_id: 1,
        team_id: 2,
        participation_id: 3,
    };
    sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = 'rotated' WHERE id = $1"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        store_and_replace_writeup(&pool, storage.as_ref(), caller, "writeup.pdf", b"%PDF-1.7",)
            .await
            .is_err()
    );
    sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET security_stamp = 'stamp', email_confirmed = FALSE
            WHERE id = $1"#,
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        store_and_replace_writeup(&pool, storage.as_ref(), caller, "writeup.pdf", b"%PDF-1.7",)
            .await
            .is_err()
    );
    sqlx::query(r#"UPDATE "AspNetUsers" SET email_confirmed = TRUE WHERE id = $1"#)
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(storage.stores.load(Ordering::SeqCst), 0);

    let mut deletion = pool.begin().await.unwrap();
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
        .execute(&mut *deletion)
        .await
        .unwrap();
    let mut upload = tokio::spawn({
        let pool = pool.clone();
        let storage = Arc::clone(&storage);
        async move {
            store_and_replace_writeup(&pool, storage.as_ref(), caller, "writeup.pdf", b"%PDF-1.7")
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
        CREATE TABLE "{schema}"."AdServiceSnapshots" (
            id BIGSERIAL PRIMARY KEY, local_file_id INTEGER NOT NULL
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
    test_support::install_operation_tables(&pool).await;

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
    let count: i64 = sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
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
    let failed_operation: (String, bool, bool) = sqlx::query_as(
        r#"SELECT state, lease_expires_at_utc <= clock_timestamp(),
                  last_error IS NOT NULL
             FROM "BlobDeletionOperations" WHERE content_hash = $1"#,
    )
    .bind(&failed_hash)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failed_operation, ("Failed".to_owned(), true, true));

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
    let owned_attachment_file = acquire(&pool, &owned_attachment_hash, "owned-attachment.zip", 21)
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

    // Force deletion to pause after claiming its durable lease. A correct
    // uploader fails fast before storage.store, then succeeds on retry once
    // deletion has finalized the lease and canonical metadata tombstone.
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
    assert!(
        store_and_acquire(&pool, storage.as_ref(), "race.bin", &bytes)
            .await
            .is_err()
    );
    assert_eq!(storage.stores.load(Ordering::SeqCst), 0);
    storage.allow_delete.notify_one();
    assert!(delete_task.await.expect("join coordinated deletion"));
    store_and_acquire(&pool, storage.as_ref(), "race.bin", &bytes)
        .await
        .unwrap();
    assert_eq!(storage.stores.load(Ordering::SeqCst), 1);
    assert!(storage.exists(&coordinated_hash).await);
    let refs: i64 = sqlx::query_scalar(r#"SELECT reference_count FROM "Files" WHERE hash = $1"#)
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
