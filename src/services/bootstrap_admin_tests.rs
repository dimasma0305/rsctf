use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn setup_notice_debug_never_contains_the_credential() {
    let notice = SetupNotice {
        token: "a-private-setup-credential-for-this-test".into(),
    };
    assert_eq!(format!("{notice:?}"), "SetupNotice(<redacted>)");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn docker_bootstrap_reuses_pending_code_and_stops_after_registration() {
    let url = std::env::var("RSCTF_TEST_DATABASE_URL").expect("disposable PostgreSQL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("docker_bootstrap_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&url)
        .unwrap()
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(r#"CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);"#)
        .execute(&pool)
        .await
        .unwrap();
    // Exercise the actual forward migration, including its idempotency.
    for _ in 0..2 {
        sqlx::raw_sql(crate::migrations::m0364_bootstrap_setup::UP_SQL)
            .execute(&pool)
            .await
            .unwrap();
    }
    let (first, second) = tokio::join!(prepare(&pool, None), prepare(&pool, None));
    let notices: Vec<_> = [first.unwrap(), second.unwrap()]
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(notices.len(), 2);
    assert_eq!(
        notices[0].token, notices[1].token,
        "replicas must share one setup code"
    );
    let value = &notices[0].token;
    assert_eq!(value.len(), 64);
    assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(
        resolve_token(&pool, None).await.unwrap().as_ref(),
        Some(value)
    );
    assert_eq!(
        prepare(&pool, None).await.unwrap().unwrap().token,
        *value,
        "a restart must reuse the pending token"
    );

    let configured = "explicit-operator-bootstrap-token-32-bytes";
    let notice = prepare(&pool, Some(configured)).await.unwrap().unwrap();
    assert_eq!(notice.token, configured);
    assert_eq!(
        prepare(&pool, Some(configured))
            .await
            .unwrap()
            .unwrap()
            .token,
        configured
    );
    assert_eq!(
        resolve_token(&pool, None).await.unwrap().as_deref(),
        Some(configured)
    );
    assert!(prepare(&pool, Some("short")).await.is_err());

    let mut attempt = pool.begin().await.unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
        .bind(uuid::Uuid::new_v4())
        .execute(&mut *attempt)
        .await
        .unwrap();
    consume_token(&mut *attempt).await.unwrap();
    attempt.rollback().await.unwrap();
    assert_eq!(
        resolve_token(&pool, None).await.unwrap().as_deref(),
        Some(configured),
        "a failed registration must not consume setup"
    );

    let mut registration = pool.begin().await.unwrap();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
        .bind(uuid::Uuid::new_v4())
        .execute(&mut *registration)
        .await
        .unwrap();
    consume_token(&mut *registration).await.unwrap();
    registration.commit().await.unwrap();
    assert!(resolve_token(&pool, None).await.unwrap().is_none());
    assert!(prepare(&pool, None).await.unwrap().is_none());
    assert!(
        prepare(&pool, Some("short")).await.unwrap().is_none(),
        "existing accounts must not need setup or disclose old tokens"
    );

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .unwrap();
}
