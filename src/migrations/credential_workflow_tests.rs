//! Real PostgreSQL coverage for the three credential/clone workflow schemas.

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn workflow_migrations_are_idempotent_and_enforce_bounded_identity() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("credential_workflows_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect isolated schema");
    sqlx::raw_sql(
        r#"CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
           CREATE TABLE "Games" (id SERIAL PRIMARY KEY);"#,
    )
    .execute(&pool)
    .await
    .expect("create referenced identity fixtures");

    for _ in 0..2 {
        sqlx::raw_sql(super::m0300_game_clone_operations::UP_SQL)
            .execute(&pool)
            .await
            .expect("apply clone workflow schema");
        sqlx::raw_sql(super::m0301_admin_credential_jobs::UP_SQL)
            .execute(&pool)
            .await
            .expect("apply admin credential schema");
        sqlx::raw_sql(super::m0302_credential_mutation_recovery::UP_SQL)
            .execute(&pool)
            .await
            .expect("apply credential mutation schema");
    }

    let admin_id = Uuid::new_v4();
    sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
        .bind(admin_id)
        .execute(&pool)
        .await
        .expect("insert admin fixture");
    let operation_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "AdminCredentialJobs"
             (operation_id, requested_by, request_digest, row_count)
           VALUES ($1, $2, $3, 200)"#,
    )
    .bind(operation_id)
    .bind(admin_id)
    .bind(vec![7_u8; 32])
    .execute(&pool)
    .await
    .expect("maximum bounded import is accepted");
    assert!(sqlx::query(
        r#"INSERT INTO "AdminCredentialJobs"
                 (operation_id, requested_by, request_digest, row_count)
               VALUES ($1, $2, $3, 201)"#,
    )
    .bind(Uuid::new_v4())
    .bind(admin_id)
    .bind(vec![8_u8; 32])
    .execute(&pool)
    .await
    .is_err());
    assert!(sqlx::query(
        r#"INSERT INTO "AdminCredentialJobs"
                 (operation_id, requested_by, request_digest, row_count)
               VALUES ($1, $2, $3, 1)"#,
    )
    .bind(operation_id)
    .bind(admin_id)
    .bind(vec![9_u8; 32])
    .execute(&pool)
    .await
    .is_err());

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
