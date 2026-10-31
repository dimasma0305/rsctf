use std::sync::Arc;

use super::*;
use crate::services::cache::InMemoryCache;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn newly_issued_sessions_work_immediately_after_a_cached_credential_rotation() {
    let db = crate::utils::test_database::TestDatabase::new("auth_generation").await;
    sqlx::raw_sql(
        r#"CREATE TABLE "AspNetUsers" (
        id UUID PRIMARY KEY, role SMALLINT NOT NULL, user_name TEXT, security_stamp TEXT
    )"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let id = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" VALUES ($1, $2, 'fixture', 'stamp-0')"#)
        .bind(id)
        .bind(Role::User as i16)
        .execute(&db.pool)
        .await
        .unwrap();
    let root = std::env::temp_dir().join(format!("auth-generation-{id}"));
    let state = crate::app_state::AppState::new(
        sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(db.pool.clone()),
        Arc::new(crate::models::internal::configs::AppConfig::default()),
        Arc::new(InMemoryCache::new()),
        Arc::new(crate::storage::LocalBlobStorage::new(root.clone())),
        crate::services::token::TokenService::new("auth-generation-test-0123456789abcdef", 60),
        Arc::new(crate::services::container::NoopContainerManager),
    );
    let mut stamp = "stamp-0".to_string();
    let mut same_window_checks = 0;
    for generation in 1..=8 {
        let window = live_authorization_window();
        authenticate_live_identity(&state, id, &stamp)
            .await
            .unwrap();
        let next = format!("stamp-{generation}");
        sqlx::query(r#"UPDATE "AspNetUsers" SET security_stamp = $2 WHERE id = $1"#)
            .bind(id)
            .bind(&next)
            .execute(&db.pool)
            .await
            .unwrap();
        // Exercise the same public resolver used by HTTP and WebSocket auth.
        let result = authenticate_live_identity(&state, id, &next).await;
        if window == live_authorization_window() {
            same_window_checks += 1;
            assert!(
                result.is_ok(),
                "fresh credential rejected by previous session's cache: {result:?}"
            );
        }
        stamp = next;
    }
    assert!(
        same_window_checks > 0,
        "fixture never exercised a warm cache window"
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(matches!(
        authenticate_live_identity(&state, id, "stamp-0").await,
        Err(AppError::Unauthorized)
    ));
    sqlx::query(
        r#"UPDATE "AspNetUsers" SET role = $2, security_stamp = 'banned-stamp' WHERE id = $1"#,
    )
    .bind(id)
    .bind(Role::Banned as i16)
    .execute(&db.pool)
    .await
    .unwrap();
    assert!(matches!(
        authenticate_live_identity(&state, id, "banned-stamp").await,
        Err(AppError::Forbidden)
    ));
    drop(state);
    db.close().await;
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
}
