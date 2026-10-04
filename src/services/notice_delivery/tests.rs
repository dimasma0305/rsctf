use std::sync::Arc;
use std::time::Duration;

use sea_orm::SqlxPostgresConnector;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::reconcile_once;
use crate::app_state::{AppState, SharedState};
use crate::models::internal::configs::AppConfig;
use crate::services::cache::InMemoryCache;
use crate::services::container::NoopContainerManager;
use crate::services::token::TokenService;
use crate::storage::LocalBlobStorage;

fn test_state(pool: sqlx::PgPool, storage_root: std::path::PathBuf) -> SharedState {
    AppState::new(
        SqlxPostgresConnector::from_sqlx_postgres_pool(pool),
        Arc::new(AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(LocalBlobStorage::new(storage_root)),
        TokenService::new("0123456789abcdef0123456789abcdef", 60),
        Arc::new(NoopContainerManager),
    )
}

/// Run explicitly with `RSCTF_TEST_DATABASE_URL=postgres://... cargo test
/// scheduled_notice_reconciles_after_restart_without_cross_game_delivery
/// -- --ignored --nocapture`.
#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn scheduled_notice_reconciles_after_restart_without_cross_game_delivery() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("RSCTF_TEST_DATABASE_URL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("notice_reconcile_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let scoped_schema = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let search_path = format!(r#"SET search_path TO "{scoped_schema}""#);
            Box::pin(async move {
                sqlx::query(&search_path).execute(connection).await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .unwrap();
    sqlx::raw_sql(
        r#"CREATE TABLE "GameNoticeOutbox" (
             id BIGSERIAL PRIMARY KEY,
             game_id INTEGER NOT NULL,
             notice_id INTEGER,
             operation_id UUID NOT NULL,
             event_kind SMALLINT NOT NULL,
             payload JSONB NOT NULL,
             available_at_utc TIMESTAMPTZ NOT NULL,
             claim_token UUID,
             claimed_at_utc TIMESTAMPTZ,
             delivered_at_utc TIMESTAMPTZ
           )"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let scheduled_operation = Uuid::new_v4();
    for (game_id, operation_id, seconds) in [
        (7, scheduled_operation, 3600_i32),
        (8, Uuid::new_v4(), 0_i32),
    ] {
        sqlx::query(
            r#"INSERT INTO "GameNoticeOutbox"
                 (game_id, operation_id, event_kind, payload, available_at_utc)
               VALUES ($1, $2, 0, $3, clock_timestamp() + ($4 * interval '1 second'))"#,
        )
        .bind(game_id)
        .bind(operation_id)
        .bind(serde_json::json!({ "id": game_id }))
        .bind(seconds)
        .execute(&pool)
        .await
        .unwrap();
    }

    let storage_root =
        std::env::temp_dir().join(format!("rsctf-notice-reconcile-{}", Uuid::new_v4()));
    let before_restart = test_state(pool.clone(), storage_root.clone());
    let mut game_seven = before_restart
        .events
        .subscribe_game_targets(7, &["ReceivedGameNotice"]);
    assert_eq!(reconcile_once(&before_restart).await.unwrap(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), game_seven.recv())
            .await
            .is_err()
    );
    drop(game_seven);
    drop(before_restart);

    sqlx::query(
        r#"UPDATE "GameNoticeOutbox"
              SET available_at_utc = clock_timestamp()
            WHERE operation_id = $1"#,
    )
    .bind(scheduled_operation)
    .execute(&pool)
    .await
    .unwrap();

    // A fresh AppState/EventBus models a process restart. The durable due row
    // is delivered once to its exact game and acknowledged before the next pass.
    let after_restart = test_state(pool.clone(), storage_root);
    let mut game_seven = after_restart
        .events
        .subscribe_game_targets(7, &["ReceivedGameNotice"]);
    assert_eq!(reconcile_once(&after_restart).await.unwrap(), 1);
    let delivered = tokio::time::timeout(Duration::from_secs(1), game_seven.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.game_id, Some(7));
    assert_eq!(delivered.payload, r#"{"id":7}"#);
    assert_eq!(reconcile_once(&after_restart).await.unwrap(), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), game_seven.recv())
            .await
            .is_err()
    );
    let remaining: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "GameNoticeOutbox" WHERE delivered_at_utc IS NULL"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 0);

    drop(game_seven);
    drop(after_restart);
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
