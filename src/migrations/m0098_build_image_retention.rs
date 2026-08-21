//! Track the last runtime demand for installation-owned Docker build images.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
ALTER TABLE "BuildImageOwnerships"
  ADD COLUMN IF NOT EXISTS last_used_at_utc TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS ix_build_image_ownership_retention
  ON "BuildImageOwnerships"
  (installation_scope, (COALESCE(last_used_at_utc, updated_at_utc)));
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_build_image_ownership_retention;
ALTER TABLE "BuildImageOwnerships"
  DROP COLUMN IF EXISTS last_used_at_utc;
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
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_uses_build_time_until_the_first_runtime_demand() {
        assert!(UP_SQL.contains("last_used_at_utc TIMESTAMPTZ NULL"));
        assert!(UP_SQL.contains("COALESCE(last_used_at_utc, updated_at_utc)"));
        assert!(DOWN_SQL.contains("DROP COLUMN IF EXISTS last_used_at_utc"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_idempotent_and_preserves_usage_time() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("image_retention_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(&format!(r#"SET search_path TO "{schema}""#))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "BuildImageOwnerships" (
                 installation_scope TEXT NOT NULL,
                 canonical_ref TEXT NOT NULL,
                 image_id TEXT NOT NULL,
                 updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 PRIMARY KEY (installation_scope, canonical_ref)
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(
            r#"INSERT INTO "BuildImageOwnerships"
               (installation_scope, canonical_ref, image_id, last_used_at_utc)
               VALUES ($1, $2, $3, clock_timestamp())"#,
        )
        .bind("0123456789abcdef0123456789abcdef")
        .bind("docker.io/rsctf/game/challenge:latest")
        .bind(format!("sha256:{}", "a".repeat(64)))
        .execute(&pool)
        .await
        .unwrap();
        let used: bool = sqlx::query_scalar(
            r#"SELECT last_used_at_utc IS NOT NULL FROM "BuildImageOwnerships""#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(used);

        sqlx::raw_sql(DOWN_SQL).execute(&pool).await.unwrap();
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
