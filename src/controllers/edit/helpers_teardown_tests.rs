use std::str::FromStr;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use super::destroy_test_container_with;
use crate::utils::error::{AppError, AppResult};

struct Harness {
    admin: sqlx::PgPool,
    pool: sqlx::PgPool,
    schema: String,
    container_id: uuid::Uuid,
}

impl Harness {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_test_teardown_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "Containers" (id UUID PRIMARY KEY, container_id TEXT NOT NULL);
            CREATE TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, test_container_id UUID REFERENCES "Containers"(id)
            );
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let container_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Containers" VALUES ($1, 'runtime-test')"#)
            .bind(container_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "GameChallenges" VALUES (7, $1)"#)
            .bind(container_id)
            .execute(&pool)
            .await
            .unwrap();
        Self {
            admin,
            pool,
            schema,
            container_id,
        }
    }

    async fn retained(&self) -> (i64, Option<uuid::Uuid>) {
        sqlx::query_as(
            r#"SELECT (SELECT COUNT(*) FROM "Containers"), test_container_id
                 FROM "GameChallenges" WHERE id = 7"#,
        )
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn failed_test_backend_destroy_retains_both_retry_owners() {
    let harness = Harness::new().await;
    let failure: AppResult<()> = Err(AppError::internal("injected destroy failure"));
    assert!(destroy_test_container_with(
        &harness.pool,
        7,
        harness.container_id,
        "runtime-test",
        async { failure },
    )
    .await
    .is_err());
    assert_eq!(harness.retained().await, (1, Some(harness.container_id)));

    destroy_test_container_with(
        &harness.pool,
        7,
        harness.container_id,
        "runtime-test",
        async { Ok(()) },
    )
    .await
    .unwrap();
    assert_eq!(harness.retained().await, (0, None));
    harness.cleanup().await;
}
