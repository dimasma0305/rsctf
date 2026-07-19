use super::*;

use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn account_deletion_fence_and_invite_accept_have_a_locked_handoff() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("rsctf_accept_fence_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin_pool)
        .await
        .expect("create isolated test schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated test pool");
    sqlx::raw_sql(
        r#"
        CREATE TABLE "AspNetUsers" (
          id UUID PRIMARY KEY,
          role SMALLINT NOT NULL,
          security_stamp TEXT
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create account fixture table");

    let user_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AspNetUsers" (id, role, security_stamp)
           VALUES ($1, $2, 'old-stamp')"#,
    )
    .bind(user_id)
    .bind(crate::utils::enums::Role::User as i16)
    .execute(&pool)
    .await
    .unwrap();

    let mut accepting = pool.begin().await.unwrap();
    lock_live_roster_account(&mut accepting, user_id)
        .await
        .expect("live account should pass invite authorization");
    let mut deleting = tokio::spawn({
        let pool = pool.clone();
        async move {
            sqlx::query(
                r#"UPDATE "AspNetUsers"
                      SET role = $1, security_stamp = 'new-stamp'
                    WHERE id = $2"#,
            )
            .bind(crate::utils::enums::Role::Banned as i16)
            .bind(user_id)
            .execute(&pool)
            .await
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut deleting)
            .await
            .is_err(),
        "account deletion passed an invite accept retaining FOR SHARE"
    );
    accepting.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), deleting)
        .await
        .expect("deletion remained blocked after accept committed")
        .expect("deletion task failed")
        .expect("deletion update failed");

    let mut rejected = pool.begin().await.unwrap();
    let error = lock_live_roster_account(&mut rejected, user_id)
        .await
        .expect_err("a post-fence invite accept must fail");
    assert_eq!(error.status(), axum::http::StatusCode::FORBIDDEN);
    rejected.rollback().await.unwrap();

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin_pool)
        .await
        .expect("drop isolated test schema");
}
