//! Bind each managed KotH reporter credential to the callback routing contract
//! that selected its crash-recoverable workload identity.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "KothTargetReporters"
    ADD COLUMN IF NOT EXISTS routing_revision VARCHAR(16);

UPDATE "KothTargetReporters"
   SET routing_revision = 'legacy'
 WHERE routing_revision IS NULL;

ALTER TABLE "KothTargetReporters"
    ALTER COLUMN routing_revision SET NOT NULL;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "KothTargetReporters"
    DROP COLUMN IF EXISTS routing_revision;
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
    use super::UP_SQL;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn existing_credentials_are_fenced_as_legacy_before_not_null() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS routing_revision VARCHAR(16)"));
        assert!(UP_SQL.contains("SET routing_revision = 'legacy'"));
        assert!(UP_SQL.contains("ALTER COLUMN routing_revision SET NOT NULL"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_backfills_existing_rows_and_is_idempotent() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TEMP TABLE "KothTargetReporters" (
                   cycle_id BIGINT PRIMARY KEY
               );
               INSERT INTO "KothTargetReporters" VALUES (41);"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let revision: String = sqlx::query_scalar(
            r#"SELECT routing_revision
                 FROM "KothTargetReporters"
                WHERE cycle_id = 41"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(revision, "legacy");
        let not_null: bool = sqlx::query_scalar(
            r#"SELECT attnotnull
                 FROM pg_attribute
                WHERE attrelid = 'pg_temp."KothTargetReporters"'::regclass
                  AND attname = 'routing_revision'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(not_null);
    }
}
