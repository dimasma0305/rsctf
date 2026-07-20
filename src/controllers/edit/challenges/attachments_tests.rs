use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::*;

struct AttachmentHarness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
}

impl AttachmentHarness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_attachment_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Files" (
              id SERIAL PRIMARY KEY,
              hash TEXT NOT NULL UNIQUE,
              upload_time_utc TIMESTAMPTZ NOT NULL DEFAULT now(),
              file_size BIGINT NOT NULL DEFAULT 1,
              name TEXT NOT NULL DEFAULT 'attachment',
              reference_count BIGINT NOT NULL
            );
            CREATE TABLE "Games" (
              id INTEGER PRIMARY KEY,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              poster_hash TEXT
            );
            CREATE TABLE "Attachments" (
              id SERIAL PRIMARY KEY,
              "Type" SMALLINT NOT NULL,
              remote_url TEXT,
              local_file_id INTEGER
            );
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY,
              game_id INTEGER NOT NULL,
              "Type" SMALLINT NOT NULL,
              deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
              attachment_id INTEGER,
              original_archive_blob_path TEXT
            );
            CREATE TABLE "FlagContexts" (
              id INTEGER PRIMARY KEY,
              flag TEXT NOT NULL DEFAULT 'flag',
              is_occupied BOOLEAN NOT NULL DEFAULT FALSE,
              challenge_id INTEGER,
              attachment_id INTEGER
            );
            CREATE TABLE "ExerciseChallenges" (
              id INTEGER PRIMARY KEY,
              attachment_id INTEGER
            );
            CREATE TABLE "Participations" (
              id INTEGER PRIMARY KEY,
              writeup_id INTEGER
            );
            CREATE TABLE "AspNetUsers" (
              id INTEGER PRIMARY KEY,
              avatar_hash TEXT
            );
            CREATE TABLE "Teams" (
              id INTEGER PRIMARY KEY,
              avatar_hash TEXT
            );
            CREATE TABLE "Configs" (
              config_key TEXT PRIMARY KEY,
              value TEXT
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        Self {
            admin,
            pool,
            schema,
        }
    }

    async fn seed(&self, game_pending: bool, challenge_pending: bool) {
        sqlx::query(r#"INSERT INTO "Games" (id, deletion_pending) VALUES (1, $1)"#)
            .bind(game_pending)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "Files" (id, hash, reference_count) VALUES (100, 'staged', 1)"#)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (id, game_id, "Type", deletion_pending)
               VALUES (11, 1, $1, $2)"#,
        )
        .bind(ChallengeType::StaticAttachment as i16)
        .bind(challenge_pending)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

async fn execute_replace(
    pool: &sqlx::PgPool,
    prepared: Option<&PreparedAttachment>,
) -> AppResult<AttachmentSwap> {
    let mut definition =
        crate::services::challenge_workloads::acquire_definition_lock(pool, 1, 11).await?;
    let result = replace_attachment_locked(definition.transaction_mut(), 1, 11, prepared).await;
    match result {
        Ok(swap) => {
            definition
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(swap)
        }
        Err(error) => {
            definition.rollback().await.unwrap();
            Err(error)
        }
    }
}

async fn execute_flag_removal(
    pool: &sqlx::PgPool,
    flag_id: i32,
) -> AppResult<Option<Option<String>>> {
    let mut definition =
        crate::services::challenge_workloads::acquire_definition_lock(pool, 1, 11).await?;
    let result = crate::controllers::edit::flags::remove_flag_locked(
        definition.transaction_mut(),
        1,
        11,
        flag_id,
    )
    .await;
    match result {
        Ok(removal) => {
            definition
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            Ok(removal)
        }
        Err(error) => {
            definition.rollback().await.unwrap();
            Err(error)
        }
    }
}

#[test]
fn remote_attachments_require_absolute_http_urls() {
    assert!(validate_remote_attachment_url("https://files.example/challenge.zip").is_ok());
    assert!(validate_remote_attachment_url("http://files.example/challenge.zip").is_ok());
    for invalid in [
        "javascript:alert(1)",
        "data:text/html,pwn",
        "/relative/file",
        "https://user:pass@files.example/file",
        "",
    ] {
        assert!(validate_remote_attachment_url(invalid).is_err());
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn pending_game_or_challenge_creates_no_attachment_or_blob_reference() {
    let harness = AttachmentHarness::new().await;
    harness.seed(false, true).await;
    let prepared = prepare_attachment(Some(FileType::Local), Some("staged".to_string()), None)
        .unwrap()
        .unwrap();

    let challenge_error = execute_replace(&harness.pool, Some(&prepared))
        .await
        .expect_err("challenge deletion fence was ignored");
    assert_eq!(challenge_error.status(), axum::http::StatusCode::CONFLICT);
    sqlx::query(r#"UPDATE "GameChallenges" SET deletion_pending = FALSE WHERE id = 11"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let game_error = execute_replace(&harness.pool, Some(&prepared))
        .await
        .expect_err("game deletion fence was ignored");
    assert_eq!(game_error.status(), axum::http::StatusCode::CONFLICT);

    let state: (i64, Option<i32>, i64) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*) FROM "Attachments"),
                  (SELECT attachment_id FROM "GameChallenges" WHERE id = 11),
                  (SELECT reference_count FROM "Files" WHERE id = 100)"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(state, (0, None, 1));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn attachment_stage_and_game_delete_fence_are_lock_ordered() {
    // Stage-first: the retained mutation row locks force the deletion marker
    // to wait. The swap commits before the fence and remains owned, never as an
    // orphan created behind an already-visible deletion marker.
    let stage_first = AttachmentHarness::new().await;
    stage_first.seed(false, false).await;
    let prepared = prepare_attachment(
        Some(FileType::Remote),
        None,
        Some("https://stage-first.example/file".to_string()),
    )
    .unwrap()
    .unwrap();
    let mut definition =
        crate::services::challenge_workloads::acquire_definition_lock(&stage_first.pool, 1, 11)
            .await
            .unwrap();
    crate::controllers::edit::challenges::reject_pending_mutation(
        &mut **definition.transaction_mut(),
        1,
        11,
    )
    .await
    .unwrap();
    let mut deletion = tokio::spawn({
        let pool = stage_first.pool.clone();
        async move {
            let mut transaction = pool.begin().await.unwrap();
            sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
                .execute(&mut *transaction)
                .await
                .unwrap();
            transaction.commit().await.unwrap();
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut deletion)
            .await
            .is_err(),
        "game deletion crossed the attachment transaction's shared row fence"
    );
    let swap = replace_attachment_locked(definition.transaction_mut(), 1, 11, Some(&prepared))
        .await
        .unwrap();
    definition.release().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), deletion)
        .await
        .expect("game deletion did not resume after attachment commit")
        .unwrap();
    let stage_state: (bool, Option<i32>, i64) = sqlx::query_as(
        r#"SELECT game.deletion_pending, challenge.attachment_id,
                  (SELECT COUNT(*)
                     FROM "Attachments" attachment
                     LEFT JOIN "GameChallenges" owner
                       ON owner.attachment_id = attachment.id
                    WHERE owner.id IS NULL)
             FROM "Games" game
             JOIN "GameChallenges" challenge ON challenge.game_id = game.id
            WHERE game.id = 1 AND challenge.id = 11"#,
    )
    .fetch_one(&stage_first.pool)
    .await
    .unwrap();
    assert_eq!(stage_state, (true, swap.attachment_id, 0));
    stage_first.cleanup().await;

    // Delete-first: whole-game deletion takes this same definition key before
    // publishing its durable marker. A late stage blocks, then observes the
    // committed marker and creates neither an Attachment nor a new blob ref.
    let delete_first = AttachmentHarness::new().await;
    delete_first.seed(false, false).await;
    let mut deletion = delete_first.pool.begin().await.unwrap();
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        &mut deletion,
        &crate::services::challenge_workloads::definition_lock_key(1, 11),
    )
    .await
    .unwrap();
    sqlx::query(r#"UPDATE "Games" SET deletion_pending = TRUE WHERE id = 1"#)
        .execute(&mut *deletion)
        .await
        .unwrap();
    let mut late_stage = tokio::spawn({
        let pool = delete_first.pool.clone();
        let prepared = prepared.clone();
        async move { execute_replace(&pool, Some(&prepared)).await }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut late_stage)
            .await
            .is_err(),
        "attachment stage crossed the game deletion definition fence"
    );
    deletion.commit().await.unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), late_stage)
        .await
        .expect("late attachment stage did not resume after deletion commit")
        .unwrap()
        .expect_err("attachment committed after the durable game deletion fence");
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    let delete_state: (i64, Option<i32>, i64) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*) FROM "Attachments"), attachment_id,
                  (SELECT reference_count FROM "Files" WHERE id = 100)
             FROM "GameChallenges" WHERE id = 11"#,
    )
    .fetch_one(&delete_first.pool)
    .await
    .unwrap();
    assert_eq!(delete_state, (0, None, 1));
    delete_first.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn old_metadata_cleanup_failure_rolls_back_the_owner_swap() {
    let harness = AttachmentHarness::new().await;
    harness.seed(false, false).await;
    sqlx::raw_sql(
        r#"
        INSERT INTO "Attachments" (id, "Type", remote_url)
        VALUES (50, 2, 'https://old.example/file');
        UPDATE "GameChallenges" SET attachment_id = 50 WHERE id = 11;
        CREATE FUNCTION reject_attachment_delete() RETURNS trigger AS $$
        BEGIN
          RAISE EXCEPTION 'injected attachment cleanup failure';
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER reject_attachment_delete
          BEFORE DELETE ON "Attachments"
          FOR EACH ROW EXECUTE FUNCTION reject_attachment_delete();
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    let prepared = prepare_attachment(
        Some(FileType::Remote),
        None,
        Some("https://new.example/file".to_string()),
    )
    .unwrap()
    .unwrap();
    execute_replace(&harness.pool, Some(&prepared))
        .await
        .expect_err("cleanup failure was reported after committing the new owner");

    let state: (Option<i32>, Vec<i32>) = (
        sqlx::query_scalar(r#"SELECT attachment_id FROM "GameChallenges" WHERE id = 11"#)
            .fetch_one(&harness.pool)
            .await
            .unwrap(),
        sqlx::query_scalar(r#"SELECT id FROM "Attachments" ORDER BY id"#)
            .fetch_all(&harness.pool)
            .await
            .unwrap(),
    );
    assert_eq!(state, (Some(50), vec![50]));
    harness.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn flag_removal_honors_deletion_fences_and_rolls_back_cleanup_failure() {
    let harness = AttachmentHarness::new().await;
    harness.seed(true, false).await;
    sqlx::raw_sql(
        r#"
        INSERT INTO "Attachments" (id, "Type", remote_url)
        VALUES (50, 2, 'https://old.example/flag');
        INSERT INTO "FlagContexts" (id, challenge_id, attachment_id)
        VALUES (70, 11, 50);
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    let pending = execute_flag_removal(&harness.pool, 70)
        .await
        .expect_err("flag removal ignored the game deletion fence");
    assert_eq!(pending.status(), axum::http::StatusCode::CONFLICT);

    sqlx::query(r#"UPDATE "Games" SET deletion_pending = FALSE WHERE id = 1"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE FUNCTION reject_flag_attachment_delete() RETURNS trigger AS $$
        BEGIN
          RAISE EXCEPTION 'injected flag attachment cleanup failure';
        END;
        $$ LANGUAGE plpgsql;
        CREATE TRIGGER reject_flag_attachment_delete
          BEFORE DELETE ON "Attachments"
          FOR EACH ROW EXECUTE FUNCTION reject_flag_attachment_delete();
        "#,
    )
    .execute(&harness.pool)
    .await
    .unwrap();
    execute_flag_removal(&harness.pool, 70)
        .await
        .expect_err("flag delete committed before attachment cleanup failed");
    let retained: (i64, i64) = sqlx::query_as(
        r#"SELECT (SELECT COUNT(*) FROM "FlagContexts" WHERE id = 70),
                  (SELECT COUNT(*) FROM "Attachments" WHERE id = 50)"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(retained, (1, 1));
    harness.cleanup().await;
}

#[derive(Default)]
struct FailingDeleteStorage(AtomicUsize);

#[async_trait]
impl crate::storage::BlobStorage for FailingDeleteStorage {
    async fn store(&self, _name: &str, _bytes: &[u8]) -> AppResult<crate::storage::StoredBlob> {
        unreachable!("purge regression never stores")
    }

    async fn load(&self, _hash: &str) -> AppResult<Vec<u8>> {
        unreachable!("purge regression never loads")
    }

    async fn delete(&self, _hash: &str) -> AppResult<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(AppError::internal("injected storage failure"))
    }

    async fn exists(&self, _hash: &str) -> bool {
        false
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn post_commit_blob_cleanup_failure_keeps_the_successful_swap_visible() {
    let harness = AttachmentHarness::new().await;
    harness.seed(false, false).await;
    sqlx::query(r#"INSERT INTO "Attachments" (id, "Type", local_file_id) VALUES (50, $1, 100)"#)
        .bind(FileType::Local as i16)
        .execute(&harness.pool)
        .await
        .unwrap();
    sqlx::query(r#"UPDATE "GameChallenges" SET attachment_id = 50 WHERE id = 11"#)
        .execute(&harness.pool)
        .await
        .unwrap();
    let prepared = prepare_attachment(
        Some(FileType::Remote),
        None,
        Some("https://new.example/file".to_string()),
    )
    .unwrap()
    .unwrap();
    let swap = execute_replace(&harness.pool, Some(&prepared))
        .await
        .expect("metadata swap should commit before physical cleanup");
    assert_eq!(swap.deleted_hash.as_deref(), Some("staged"));

    let storage = FailingDeleteStorage::default();
    purge_replaced_attachment(&harness.pool, &storage, 11, "staged").await;
    assert_eq!(storage.0.load(Ordering::SeqCst), 1);
    let state: (Option<i32>, i64, i64) = sqlx::query_as(
        r#"SELECT attachment_id,
                  (SELECT COUNT(*) FROM "Attachments" WHERE id = 50),
                  (SELECT reference_count FROM "Files" WHERE id = 100)
             FROM "GameChallenges" WHERE id = 11"#,
    )
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(state, (swap.attachment_id, 0, 0));
    harness.cleanup().await;
}
