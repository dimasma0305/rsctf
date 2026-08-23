use std::collections::BTreeMap;

use sqlx::postgres::PgPoolOptions;

use super::*;
use crate::services::cache::InMemoryCache;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn config_is_default_off_atomic_and_secret_preserving() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("donations_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let search_path = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(2)
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
        r#"CREATE TABLE "Configs" (
             config_key TEXT PRIMARY KEY,
             value TEXT,
             cache_keys JSONB
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let cache = InMemoryCache::new();
    let empty = DonationSettings::load(&pool).await.unwrap();
    assert!(!empty.active());
    let missing_key = DonationConfig {
        enabled: true,
        provider: DonationProvider::Trakteer,
        donate_url: Some("https://trakteer.id/tcp1p/tip".to_owned()),
        api_key: None,
        has_api_key: false,
    };
    assert!(matches!(
        validate_config(&pool, &missing_key).await,
        Err(AppError::BadRequest(_))
    ));
    save_config(
        &pool,
        &cache,
        DonationConfig {
            enabled: true,
            provider: DonationProvider::Trakteer,
            donate_url: Some("https://trakteer.id/tcp1p/tip".to_owned()),
            api_key: Some("first-secret".to_owned()),
            has_api_key: false,
        },
    )
    .await
    .unwrap();
    validate_config(
        &pool,
        &DonationConfig {
            enabled: true,
            provider: DonationProvider::Trakteer,
            donate_url: Some("https://trakteer.id/tcp1p/tip".to_owned()),
            api_key: Some(String::new()),
            has_api_key: true,
        },
    )
    .await
    .unwrap();
    save_config(
        &pool,
        &cache,
        DonationConfig {
            enabled: true,
            provider: DonationProvider::Trakteer,
            donate_url: Some("https://trakteer.id/tcp1p/tip".to_owned()),
            api_key: Some(String::new()),
            has_api_key: true,
        },
    )
    .await
    .unwrap();

    let rows: BTreeMap<String, Option<String>> = sqlx::query_as::<_, (String, Option<String>)>(
        r#"SELECT config_key, value FROM "Configs" ORDER BY config_key"#,
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    assert_eq!(
        rows.get(API_KEY).and_then(|value| value.as_deref()),
        Some("first-secret")
    );
    let projection = admin_config(&rows);
    assert!(projection.enabled);
    assert!(projection.has_api_key);
    assert!(projection.api_key.is_none());
    assert_eq!(
        projection.donate_url.as_deref(),
        Some("https://trakteer.id/tcp1p/tip")
    );

    let invalid = save_config(
        &pool,
        &cache,
        DonationConfig {
            enabled: true,
            provider: DonationProvider::Trakteer,
            donate_url: Some("https://example.com/not-trakteer".to_owned()),
            api_key: Some("header\ninjection".to_owned()),
            has_api_key: true,
        },
    )
    .await;
    assert!(matches!(invalid, Err(AppError::BadRequest(_))));
    assert_eq!(
        DonationSettings::load(&pool)
            .await
            .unwrap()
            .api_key
            .as_deref(),
        Some("first-secret")
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
