//! Bind split-role presence to one exact rsctf build and runtime protocol.
//!
//! Rows written by a pre-migration binary receive the `legacy` sentinel. New
//! binaries never count those rows as compatible peers, so a partial rollout
//! cannot make a new web/engine replica healthy against old role owners.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    ALTER TABLE "RuntimeRoleHeartbeats"
      ADD COLUMN IF NOT EXISTS build_fingerprint TEXT NOT NULL DEFAULT 'legacy';

    UPDATE "RuntimeRoleHeartbeats"
       SET build_fingerprint = 'legacy'
     WHERE build_fingerprint IS NULL OR BTRIM(build_fingerprint) = '';

    ALTER TABLE "RuntimeRoleHeartbeats"
      ALTER COLUMN build_fingerprint SET DEFAULT 'legacy',
      ALTER COLUMN build_fingerprint SET NOT NULL;

    CREATE INDEX IF NOT EXISTS ix_runtime_role_heartbeats_build_freshness_role
        ON "RuntimeRoleHeartbeats"
           (heartbeat_at_utc DESC, build_fingerprint, role);
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
                r#"
                DROP INDEX IF EXISTS ix_runtime_role_heartbeats_build_freshness_role;
                ALTER TABLE "RuntimeRoleHeartbeats"
                  DROP COLUMN IF EXISTS build_fingerprint;
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn upgrades_legacy_rows_and_indexes_compatible_lookups() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS build_fingerprint"));
        assert!(UP_SQL.contains("DEFAULT 'legacy'"));
        assert!(UP_SQL.contains("ALTER COLUMN build_fingerprint SET NOT NULL"));
        assert!(UP_SQL.contains("heartbeat_at_utc DESC, build_fingerprint, role"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn legacy_heartbeats_are_marked_and_the_upgrade_is_idempotent() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("runtime_fingerprint_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."RuntimeRoleHeartbeats" (
                instance_id UUID PRIMARY KEY,
                role TEXT NOT NULL,
                started_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                heartbeat_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
            );
            INSERT INTO "{schema}"."RuntimeRoleHeartbeats" (instance_id, role)
            VALUES ('00000000-0000-0000-0000-000000000001', 'control');
            "#
        );
        sqlx::raw_sql(&setup).execute(&admin).await.unwrap();

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        let fingerprint: String =
            sqlx::query_scalar(r#"SELECT build_fingerprint FROM "RuntimeRoleHeartbeats" LIMIT 1"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(fingerprint, "legacy");

        let null_rejected =
            sqlx::query(r#"UPDATE "RuntimeRoleHeartbeats" SET build_fingerprint = NULL"#)
                .execute(&pool)
                .await
                .unwrap_err();
        assert!(matches!(
            null_rejected,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23502")
        ));

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup).execute(&admin).await.unwrap();
    }
}
