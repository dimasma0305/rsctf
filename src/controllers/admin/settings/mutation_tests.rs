use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::*;

#[test]
fn byte_budgets_count_utf8_not_characters() {
    assert!(validate_text(&"é".repeat(64), "title", 128, false).is_ok());
    assert_eq!(
        validate_text(&"é".repeat(65), "title", 128, false)
            .unwrap_err()
            .status(),
        axum::http::StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[test]
fn request_digest_is_stable_but_revision_and_value_sensitive() {
    let updates = BTreeMap::from([
        ("GlobalConfig:Slogan".to_string(), Some("two".to_string())),
        ("GlobalConfig:Title".to_string(), Some("one".to_string())),
    ]);
    let same = updates.clone().into_iter().rev().collect();
    assert_eq!(
        settings_request_digest(4, BrandingAction::Keep, &updates).unwrap(),
        settings_request_digest(4, BrandingAction::Keep, &same).unwrap()
    );
    assert_ne!(
        settings_request_digest(4, BrandingAction::Keep, &updates).unwrap(),
        settings_request_digest(5, BrandingAction::Keep, &updates).unwrap()
    );
}

#[test]
fn email_domains_are_canonical_and_bounded() {
    assert_eq!(
        canonical_email_domains(" Example.COM,ctf.test\nexample.com ").unwrap(),
        "ctf.test\nexample.com"
    );
    let too_many = (0..129)
        .map(|index| format!("{index}.test"))
        .collect::<Vec<_>>()
        .join(",");
    assert!(canonical_email_domains(&too_many).is_err());
}

#[test]
fn security_snapshot_is_invalidated_once_only_after_commit() {
    let source = include_str!("mutation.rs");
    let update = source
        .split_once("pub async fn update_config(")
        .expect("settings update handler")
        .1;
    let commit = update
        .find("transaction.commit().await.map_err(database_error)?;")
        .expect("settings transaction commit");
    let invalidation = update
        .find("crate::services::captcha::invalidate_settings_snapshot();")
        .expect("post-commit captcha invalidation");
    assert!(commit < invalidation);
    assert_eq!(
        update
            .matches("crate::services::captcha::invalidate_settings_snapshot();")
            .count(),
        1
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn failed_nth_write_rolls_back_and_completed_operation_replays() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("platform_settings_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TABLE "Configs" (
          config_key TEXT PRIMARY KEY, value TEXT, cache_keys JSONB
        );
        CREATE TABLE "PlatformSettingsState" (
          singleton SMALLINT PRIMARY KEY, revision BIGINT NOT NULL,
          updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
        );
        CREATE TABLE "PlatformSettingsOperations" (
          operation_id UUID PRIMARY KEY, actor_user_id UUID,
          request_digest BYTEA NOT NULL, expected_revision BIGINT NOT NULL,
          result_revision BIGINT NOT NULL, branding_hash TEXT,
          completed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
        );
        INSERT INTO "PlatformSettingsState" VALUES (1, 0, clock_timestamp());
        INSERT INTO "Configs" VALUES
          ('GlobalConfig:Title', 'old-title', NULL),
          ('GlobalConfig:Slogan', 'old-slogan', NULL);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let updates = BTreeMap::from([
        (
            "GlobalConfig:Slogan".to_string(),
            Some("new-slogan".to_string()),
        ),
        (
            "GlobalConfig:Title".to_string(),
            Some("new-title".to_string()),
        ),
    ]);

    let mut failed = pool.begin().await.unwrap();
    assert_eq!(lock_settings_revision(&mut failed).await.unwrap(), 0);
    write_config_updates(&mut failed, &updates).await.unwrap();
    sqlx::query("SELECT 1 / 0")
        .execute(&mut *failed)
        .await
        .expect_err("injected final write failure must abort the candidate revision");
    failed.rollback().await.unwrap();
    let old = config_values(&pool).await;
    assert_eq!(old, vec!["old-slogan", "old-title"]);

    let operation_id = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let digest = settings_request_digest(0, BrandingAction::Keep, &updates).unwrap();
    let mut committed = pool.begin().await.unwrap();
    assert_eq!(lock_settings_revision(&mut committed).await.unwrap(), 0);
    write_config_updates(&mut committed, &updates)
        .await
        .unwrap();
    let revision: i64 = sqlx::query_scalar(
        r#"UPDATE "PlatformSettingsState" SET revision = revision + 1
            WHERE singleton = 1 RETURNING revision"#,
    )
    .fetch_one(&mut *committed)
    .await
    .unwrap();
    sqlx::query(
        r#"INSERT INTO "PlatformSettingsOperations"
             (operation_id, actor_user_id, request_digest, expected_revision,
              result_revision, branding_hash)
           VALUES ($1, $2, $3, 0, $4, NULL)"#,
    )
    .bind(operation_id)
    .bind(actor)
    .bind(&digest)
    .bind(revision)
    .execute(&mut *committed)
    .await
    .unwrap();
    committed.commit().await.unwrap();

    let mut replay = pool.begin().await.unwrap();
    assert_eq!(lock_settings_revision(&mut replay).await.unwrap(), 1);
    let operation = load_operation(&mut replay, operation_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operation.actor_user_id, Some(actor));
    assert_eq!(operation.request_digest, digest);
    assert_eq!(operation.expected_revision, 0);
    assert_eq!(operation.result_revision, 1);
    replay.rollback().await.unwrap();
    assert_eq!(config_values(&pool).await, vec!["new-slogan", "new-title"]);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

async fn config_values(pool: &sqlx::PgPool) -> Vec<String> {
    sqlx::query_scalar(
        r#"SELECT value FROM "Configs"
            WHERE config_key IN ('GlobalConfig:Title', 'GlobalConfig:Slogan')
            ORDER BY config_key"#,
    )
    .fetch_all(pool)
    .await
    .unwrap()
}
