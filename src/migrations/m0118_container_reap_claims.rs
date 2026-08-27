//! Add a short, durable lease for bounded expired-container maintenance.
//!
//! The lease lets one maintenance pass release its row locks before Docker or
//! Kubernetes I/O. A cancelled pass naturally becomes eligible again after the
//! lease, while an explicit failure can choose a shorter retry delay.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
ALTER TABLE "Containers"
  ADD COLUMN IF NOT EXISTS reap_claim_token UUID,
  ADD COLUMN IF NOT EXISTS reap_after TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS ix_containers_expired_reap
  ON "Containers" (expect_stop_at, id)
  INCLUDE (reap_after);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"DROP INDEX IF EXISTS ix_containers_expired_reap;
                   ALTER TABLE "Containers"
                     DROP COLUMN IF EXISTS reap_claim_token,
                     DROP COLUMN IF EXISTS reap_after;"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_idempotent_and_supports_the_due_claim_plan() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("container_reap_migration_{}", uuid::Uuid::new_v4().simple());
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
            r#"CREATE TABLE "Containers" (
                 id UUID PRIMARY KEY,
                 expect_stop_at TIMESTAMPTZ NOT NULL
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let columns: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)
                 FROM information_schema.columns
                WHERE table_schema = current_schema()
                  AND table_name = 'Containers'
                  AND column_name IN ('reap_claim_token', 'reap_after')"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(columns, 2);
        let index_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM pg_indexes
                WHERE schemaname = current_schema()
                  AND indexname = 'ix_containers_expired_reap'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(index_count, 1);

        sqlx::query(
            r#"INSERT INTO "Containers" (id, expect_stop_at)
               SELECT gen_random_uuid(), clock_timestamp() - interval '1 minute'
                 FROM generate_series(1, 4096)"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("ANALYZE \"Containers\"")
            .execute(&pool)
            .await
            .unwrap();
        let plan = sqlx::query_scalar::<_, String>(
            r#"EXPLAIN (COSTS OFF)
               SELECT id FROM "Containers"
                WHERE expect_stop_at < clock_timestamp()
                  AND (reap_after IS NULL OR reap_after <= clock_timestamp())
                ORDER BY expect_stop_at, id
                LIMIT 64 FOR UPDATE SKIP LOCKED"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .join("\n");
        assert!(
            plan.contains("ix_containers_expired_reap"),
            "bounded due claim did not use its index:\n{plan}"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
