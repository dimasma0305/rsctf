//! Deployment-wide admission for repository scans. Existing scan leases remain
//! valid during a rolling upgrade; newly claimed work owns one global slot and
//! one canonical repository-host slot until completion or lease expiry.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "RepoBindings"
    ADD COLUMN IF NOT EXISTS scan_host_key TEXT NULL,
    ADD COLUMN IF NOT EXISTS scan_slot SMALLINT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = '"RepoBindings"'::regclass
           AND conname = 'ck_repobindings_scan_admission'
    ) THEN
        ALTER TABLE "RepoBindings"
            ADD CONSTRAINT ck_repobindings_scan_admission CHECK (
                (scan_host_key IS NULL AND scan_slot IS NULL)
                OR (
                    scan_host_key IS NOT NULL
                    AND scan_slot IS NOT NULL
                    AND scan_lease_token IS NOT NULL
                    AND scan_lease_until IS NOT NULL
                    AND scan_slot BETWEEN 0 AND 1
                    AND OCTET_LENGTH(scan_host_key) BETWEEN 1 AND 255
                )
            );
    END IF;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS ux_repobindings_live_scan_slot
    ON "RepoBindings" (scan_slot)
    WHERE scan_slot IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS ux_repobindings_live_scan_host
    ON "RepoBindings" (scan_host_key)
    WHERE scan_host_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_repobindings_scan_lease_expiry
    ON "RepoBindings" (scan_lease_until, id)
    WHERE scan_lease_token IS NOT NULL;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn scan_admission_is_bounded_and_forward_only() {
        assert!(UP_SQL.contains("scan_slot BETWEEN 0 AND 1"));
        assert!(UP_SQL.contains("ux_repobindings_live_scan_slot"));
        assert!(UP_SQL.contains("ux_repobindings_live_scan_host"));
        assert!(UP_SQL.contains("ix_repobindings_scan_lease_expiry"));
        assert!(UP_SQL.contains("scan_host_key IS NOT NULL"));
        assert!(UP_SQL.contains("scan_slot IS NOT NULL"));
        assert!(!UP_SQL.contains("DROP "));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn forward_migration_is_idempotent_and_enforces_global_admission() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("rsctf_m0239_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "RepoBindings" (
                 id INTEGER PRIMARY KEY,
                 scan_lease_token UUID,
                 scan_lease_until TIMESTAMPTZ
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(
            r#"INSERT INTO "RepoBindings"
                 (id, scan_lease_token, scan_lease_until, scan_host_key, scan_slot)
               VALUES (1, $1, clock_timestamp() + INTERVAL '15 minutes',
                       'github.com', 0)"#,
        )
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        let duplicate_slot = sqlx::query(
            r#"INSERT INTO "RepoBindings"
                 (id, scan_lease_token, scan_lease_until, scan_host_key, scan_slot)
               VALUES (2, $1, clock_timestamp() + INTERVAL '15 minutes',
                       'gitlab.com', 0)"#,
        )
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(duplicate_slot.is_err(), "one slot cannot have two owners");

        let duplicate_host = sqlx::query(
            r#"INSERT INTO "RepoBindings"
                 (id, scan_lease_token, scan_lease_until, scan_host_key, scan_slot)
               VALUES (3, $1, clock_timestamp() + INTERVAL '15 minutes',
                       'github.com', 1)"#,
        )
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(duplicate_host.is_err(), "one host cannot have two owners");

        let incomplete = sqlx::query(
            r#"INSERT INTO "RepoBindings"
                 (id, scan_host_key, scan_slot)
               VALUES (4, 'codeberg.org', 1)"#,
        )
        .execute(&pool)
        .await;
        assert!(
            incomplete.is_err(),
            "an admission must be backed by a lease record"
        );
        let half_admission = sqlx::query(
            r#"INSERT INTO "RepoBindings"
                 (id, scan_lease_token, scan_lease_until, scan_slot)
               VALUES (5, $1, clock_timestamp() + INTERVAL '15 minutes', 1)"#,
        )
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(
            half_admission.is_err(),
            "host and global admission must be owned together"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
