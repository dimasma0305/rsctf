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
    let operation_id = Uuid::new_v4();
    let model = ConfigEditModel {
        operation_id: Some(operation_id),
        expected_revision: Some(4),
        container_provider: Some(ContainerProviderInfoModel {
            provider_type: Some("Docker".to_string()),
            port_mapping_type: Some("PlatformProxy".to_string()),
            traffic_capture: false,
            kubernetes_namespace: None,
            image_pull_policy: None,
        }),
        ..ConfigEditModel::default()
    };
    let same: ConfigEditModel =
        serde_json::from_value(serde_json::to_value(&model).unwrap()).unwrap();
    assert_eq!(
        settings_request_digest(4, &model).unwrap(),
        settings_request_digest(4, &same).unwrap()
    );
    assert_ne!(
        settings_request_digest(4, &model).unwrap(),
        settings_request_digest(5, &model).unwrap()
    );

    let changed = ConfigEditModel {
        operation_id: Some(operation_id),
        expected_revision: Some(4),
        container_provider: Some(ContainerProviderInfoModel {
            port_mapping_type: Some("Default".to_string()),
            ..ContainerProviderInfoModel::default()
        }),
        ..ConfigEditModel::default()
    };
    assert_ne!(
        settings_request_digest(4, &model).unwrap(),
        settings_request_digest(4, &changed).unwrap()
    );
}

#[test]
fn request_digest_ignores_read_only_container_provider_summary() {
    let first = ConfigEditModel {
        container_provider: Some(ContainerProviderInfoModel {
            provider_type: Some("Docker".to_string()),
            port_mapping_type: Some("PlatformProxy".to_string()),
            ..ContainerProviderInfoModel::default()
        }),
        ..ConfigEditModel::default()
    };
    let second = ConfigEditModel {
        container_provider: Some(ContainerProviderInfoModel {
            provider_type: Some("Kubernetes".to_string()),
            port_mapping_type: Some("PlatformProxy".to_string()),
            traffic_capture: true,
            kubernetes_namespace: Some("ctf".to_string()),
            image_pull_policy: Some("Always".to_string()),
        }),
        ..ConfigEditModel::default()
    };
    assert_eq!(
        settings_request_digest(0, &first).unwrap(),
        settings_request_digest(0, &second).unwrap()
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
        CREATE TABLE "PlatformSettingsBrandingStaging" (
          operation_id UUID PRIMARY KEY, actor_user_id UUID,
          blob_hash TEXT NOT NULL
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
    let digest_model = ConfigEditModel {
        expected_revision: Some(0),
        global_config: Some(GlobalConfig {
            title: "new title".to_string(),
            ..GlobalConfig::default()
        }),
        ..ConfigEditModel::default()
    };
    let digest = settings_request_digest(0, &digest_model).unwrap();
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
    let operation = match admit_settings_operation(&mut replay, operation_id, actor, 0, &digest)
        .await
        .unwrap()
    {
        SettingsOperationAdmission::Replay(operation) => operation,
        SettingsOperationAdmission::Fresh => {
            panic!("committed endpoint operation was not replayed")
        }
    };
    assert_eq!(operation.actor_user_id, Some(actor));
    assert_eq!(operation.request_digest, digest);
    assert_eq!(operation.expected_revision, 0);
    assert_eq!(operation.result_revision, 1);
    replay.rollback().await.unwrap();
    assert_eq!(config_values(&pool).await, vec!["new-slogan", "new-title"]);

    for (candidate_actor, candidate_digest) in
        [(Uuid::new_v4(), digest.clone()), (actor, vec![7_u8; 32])]
    {
        let mut conflict = pool.begin().await.unwrap();
        let error = admit_settings_operation(
            &mut conflict,
            operation_id,
            candidate_actor,
            0,
            &candidate_digest,
        )
        .await
        .expect_err("operation identity must stay bound to actor and request");
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
        conflict.rollback().await.unwrap();
    }

    let mut stale = pool.begin().await.unwrap();
    let stale_error = admit_settings_operation(&mut stale, Uuid::new_v4(), actor, 0, &digest)
        .await
        .expect_err("a new operation cannot overwrite a newer settings revision");
    assert_eq!(stale_error.status(), axum::http::StatusCode::CONFLICT);
    stale.rollback().await.unwrap();

    let branding_operation = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "PlatformSettingsBrandingStaging"
             (operation_id, actor_user_id, blob_hash) VALUES ($1, $2, 'sha256:brand')"#,
    )
    .bind(branding_operation)
    .bind(actor)
    .execute(&pool)
    .await
    .unwrap();
    let mut branding = pool.begin().await.unwrap();
    assert_eq!(
        resolve_branding_hash(
            &mut branding,
            branding_operation,
            actor,
            BrandingAction::Set,
            None,
        )
        .await
        .unwrap()
        .as_deref(),
        Some("sha256:brand")
    );
    branding.rollback().await.unwrap();
    let mut wrong_branding_actor = pool.begin().await.unwrap();
    let error = resolve_branding_hash(
        &mut wrong_branding_actor,
        branding_operation,
        Uuid::new_v4(),
        BrandingAction::Set,
        None,
    )
    .await
    .expect_err("a staged logo cannot cross administrator identities");
    assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    wrong_branding_actor.rollback().await.unwrap();

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
