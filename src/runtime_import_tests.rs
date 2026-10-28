//! Registered import consumers recover after durable side effects commit but
//! before the terminal receipt. HTTP admission/replay exercises the real router.
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use sea_orm::SqlxPostgresConnector;
use sea_orm_migration::MigratorTrait;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use super::*;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::internal::configs::AppConfig;

#[path = "runtime_import_tests/repository.rs"]
mod repository;

async fn request(
    router: &axum::Router,
    actor: Option<&CurrentUser>,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", content_type)
        .header("idempotency-key", Uuid::new_v4().to_string())
        .body(Body::from(body))
        .unwrap();
    if let Some(actor) = actor {
        request.extensions_mut().insert(actor.clone());
    }
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 128 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn multipart(operation: Uuid) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("challenge.yaml", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"name: Role recovery\ntype: StaticAttachment\ncategory: Misc\nprovide: handout.txt\nflags:\n  - flag{role_recovery}\n").unwrap();
    zip.start_file("handout.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"durable handout").unwrap();
    let archive = zip.finish().unwrap().into_inner();
    let mut body = format!("--role-fixture\r\nContent-Disposition: form-data; name=\"operationId\"\r\n\r\n{operation}\r\n--role-fixture\r\nContent-Disposition: form-data; name=\"archive\"; filename=\"challenge.zip\"\r\nContent-Type: application/zip\r\n\r\n").into_bytes();
    body.extend(archive);
    body.extend_from_slice(b"\r\n--role-fixture--\r\n");
    body
}

async fn import(
    router: &axum::Router,
    actor: Option<&CurrentUser>,
    game: i32,
    operation: Uuid,
) -> (StatusCode, Value) {
    request(
        router,
        actor,
        "POST",
        &format!("/api/edit/games/{game}/challenges/import"),
        "multipart/form-data; boundary=role-fixture",
        multipart(operation),
    )
    .await
}

async fn status(pool: &sqlx::PgPool, job: Uuid) -> (i16, Option<Value>, Option<String>) {
    sqlx::query_as(r#"SELECT status, result, error FROM "ChallengeImportJobs" WHERE id = $1"#)
        .bind(job)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn wait_status(pool: &sqlx::PgPool, job: Uuid, expected: i16) -> Option<Value> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (actual, result, error) = status(pool, job).await;
            assert_ne!(actual, 3, "import failed: {error:?}");
            if actual == expected {
                return result;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("registered import consumer did not progress")
}

fn state(pool: sqlx::PgPool, root: &Path, role: RuntimeRole) -> SharedState {
    let mut config = AppConfig::default();
    config.runtime_role = role;
    config.storage_root = root.to_string_lossy().into_owned();
    crate::app_state::AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(pool),
        Arc::new(config),
        Arc::new(services::cache::InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.join("blobs"))),
        services::token::TokenService::new("runtime-import-test-0123456789abcdef", 60),
        Arc::new(services::container::NoopContainerManager),
    )
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn registered_import_roles_recover_committed_side_effects_and_replay_http_receipts() {
    let db = crate::utils::test_database::TestDatabase::new("role_imports").await;
    crate::migrations::Migrator::up(
        &SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone()),
        None,
    )
    .await
    .unwrap();
    let root = std::env::temp_dir().join(format!("role-imports-{}", Uuid::new_v4()));
    let actor = CurrentUser {
        id: Uuid::new_v4(),
        role: crate::utils::enums::Role::Admin,
        name: "fixture".into(),
        security_stamp: "fixture".into(),
    };
    sqlx::query(
        r#"INSERT INTO "AspNetUsers"
        (id, user_name, normalized_user_name, email_confirmed, phone_number_confirmed,
         two_factor_enabled, lockout_enabled, access_failed_count, role, ip,
         last_signed_in_utc, last_visited_utc, register_time_utc, bio, real_name,
         std_number, exercise_visible)
        VALUES ($1, 'fixture', 'FIXTURE', TRUE, FALSE, FALSE, FALSE, 0, 1, '',
            now(), now(), now(), '', '', '', TRUE)"#,
    )
    .bind(actor.id)
    .execute(&db.pool)
    .await
    .unwrap();
    for role in [
        RuntimeRole::All,
        RuntimeRole::Control,
        RuntimeRole::Engine,
        RuntimeRole::Development,
    ] {
        let api = state(db.pool.clone(), &root, RuntimeRole::Web);
        let router = crate::controllers::edit::router().with_state(api.clone());
        let now = chrono::Utc::now();
        let (code, game) = request(
            &router,
            Some(&actor),
            "POST",
            "/api/edit/games",
            "application/json",
            serde_json::to_vec(
                &json!({"title": format!("Import {role:?}"), "hidden": true, "practiceMode": false,
                "start": (now + chrono::Duration::hours(1)).timestamp_millis(),
                "end": (now + chrono::Duration::hours(2)).timestamp_millis(),
                "writeupDeadline": (now + chrono::Duration::hours(3)).timestamp_millis()}),
            )
            .unwrap(),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "{game}");
        let game_id = game["id"].as_i64().unwrap() as i32;
        let operation = Uuid::new_v4();
        let (code, _) = import(&router, None, game_id, operation).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED);
        let outsider = CurrentUser {
            id: Uuid::new_v4(),
            role: crate::utils::enums::Role::User,
            ..actor.clone()
        };
        let (code, _) = import(&router, Some(&outsider), game_id, operation).await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        let (code, admitted) = import(&router, Some(&actor), game_id, operation).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{admitted}");
        let job: Uuid = admitted["jobId"].as_str().unwrap().parse().unwrap();
        assert_eq!(status(&db.pool, job).await.0, 0);

        // Hold the event until the worker claims, then hold only its receipt
        // row. Domain publication can complete, but final acknowledgement cannot.
        let mut game_fence = db.pool.begin().await.unwrap();
        sqlx::query(r#"SELECT id FROM "Games" WHERE id = $1 FOR UPDATE"#)
            .bind(game_id)
            .execute(&mut *game_fence)
            .await
            .unwrap();
        let worker = *for_role(role)
            .iter()
            .find(|worker| **worker == DurableWorker::ChallengeImport)
            .unwrap();
        let consumer = state(db.pool.clone(), &root, role);
        let (stop, signal) = watch::channel(false);
        let handle = worker.start(consumer.clone(), signal);
        wait_status(&db.pool, job, 1).await;
        let mut receipt = db.pool.begin().await.unwrap();
        sqlx::query(r#"SELECT id FROM "ChallengeImportJobs" WHERE id = $1 FOR UPDATE"#)
            .bind(job)
            .execute(&mut *receipt)
            .await
            .unwrap();
        game_fence.commit().await.unwrap();
        let challenge_id = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let id = sqlx::query_scalar::<_, i32>(r#"SELECT id FROM "GameChallenges"
                    WHERE game_id = $1 AND attachment_id IS NOT NULL AND original_archive_blob_path IS NOT NULL"#)
                    .bind(game_id).fetch_optional(&db.pool).await.unwrap();
                if let Some(id) = id { break id; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("import did not publish artifacts before its receipt");
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        drop(stop);
        sqlx::query(r#"UPDATE "ChallengeImportJobs" SET lease_expires_at = clock_timestamp() - interval '1 second'
            WHERE id = $1"#).bind(job).execute(&mut *receipt).await.unwrap();
        receipt.commit().await.unwrap();

        let (stop, signal) = watch::channel(false);
        let recovered = worker.start(consumer.clone(), signal);
        let result = wait_status(&db.pool, job, 2).await.unwrap();
        assert_eq!(result["failed"], 0, "{result}");
        assert_eq!(
            result["imported"], 0,
            "recovery created a duplicate: {result}"
        );
        assert_eq!(result["updated"], 1, "{result}");
        let (code, replay) = import(&router, Some(&actor), game_id, operation).await;
        assert_eq!(code, StatusCode::ACCEPTED, "{replay}");
        assert_eq!(replay["jobId"], admitted["jobId"]);
        assert_eq!(replay["status"], "Succeeded");
        assert_eq!(replay["result"], result);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), recovered)
            .await
            .unwrap()
            .unwrap();
        let counts: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT
            (SELECT count(*) FROM "GameChallenges" WHERE game_id = $1),
            (SELECT count(*) FROM "FlagContexts" WHERE challenge_id = $2),
            (SELECT count(*) FROM "ChallengeImportJobs" WHERE game_id = $1)"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(counts, (1, 1, 1));
        let handout: (String, i64) = sqlx::query_as(r#"SELECT file.hash, file.reference_count
            FROM "GameChallenges" challenge JOIN "Attachments" attachment ON attachment.id = challenge.attachment_id
            JOIN "Files" file ON file.id = attachment.local_file_id WHERE challenge.id = $1"#)
            .bind(challenge_id).fetch_one(&db.pool).await.unwrap();
        assert_eq!(
            api.storage.load(&handout.0).await.unwrap(),
            b"durable handout"
        );
        // Identical handouts intentionally deduplicate across the four games;
        // each currently published attachment acquires exactly one reference.
        let owners: i64 = sqlx::query_scalar(
            r#"SELECT count(*) FROM "Attachments" attachment
            JOIN "Files" file ON file.id = attachment.local_file_id WHERE file.hash = $1"#,
        )
        .bind(&handout.0)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(handout.1, owners);
        repository::assert_registered_push_replay(&consumer, &root, game_id, challenge_id).await;
        assert_eq!(
            sqlx::query_scalar::<_, i32>(
                r#"SELECT attempts FROM "ChallengeImportJobs" WHERE id = $1"#
            )
            .bind(job)
            .fetch_one(&db.pool)
            .await
            .unwrap(),
            2
        );
    }
    db.close().await;
    tokio::fs::remove_dir_all(root).await.unwrap();
}
