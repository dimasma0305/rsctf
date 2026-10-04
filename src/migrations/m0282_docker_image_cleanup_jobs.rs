//! Durable cadence, leases, and bounded candidate claims for Docker image cleanup.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "BuildImageOwnerships"
    ADD COLUMN IF NOT EXISTS cleanup_claim_token UUID NULL,
    ADD COLUMN IF NOT EXISTS cleanup_claim_until TIMESTAMPTZ NULL,
    ADD COLUMN IF NOT EXISTS cleanup_removal_started BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS cleanup_checked_at_utc TIMESTAMPTZ NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_build_image_cleanup_claim_pair'
           AND conrelid = '"BuildImageOwnerships"'::regclass
    ) THEN
        ALTER TABLE "BuildImageOwnerships"
            ADD CONSTRAINT ck_build_image_cleanup_claim_pair
            CHECK ((cleanup_claim_token IS NULL) = (cleanup_claim_until IS NULL));
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_build_image_cleanup_finalizing_claim'
           AND conrelid = '"BuildImageOwnerships"'::regclass
    ) THEN
        ALTER TABLE "BuildImageOwnerships"
            ADD CONSTRAINT ck_build_image_cleanup_finalizing_claim
            CHECK (NOT cleanup_removal_started
                   OR (cleanup_claim_token IS NOT NULL
                       AND cleanup_claim_until IS NOT NULL));
    END IF;
END
$$;

CREATE INDEX IF NOT EXISTS ix_build_image_cleanup_candidates
    ON "BuildImageOwnerships"
       (installation_scope, cleanup_claim_until, cleanup_checked_at_utc,
        (COALESCE(last_used_at_utc, updated_at_utc)), canonical_ref);

CREATE TABLE IF NOT EXISTS "ImageCleanupSchedules" (
    installation_scope TEXT PRIMARY KEY,
    next_run_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    lease_token UUID NULL,
    lease_until TIMESTAMPTZ NULL,
    last_started_at_utc TIMESTAMPTZ NULL,
    last_finished_at_utc TIMESTAMPTZ NULL,
    last_scanned BIGINT NOT NULL DEFAULT 0,
    last_claimed BIGINT NOT NULL DEFAULT 0,
    last_removed BIGINT NOT NULL DEFAULT 0,
    last_backlog BIGINT NOT NULL DEFAULT 0,
    last_duration_ms BIGINT NOT NULL DEFAULT 0,
    last_error VARCHAR(1024) NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT ck_image_cleanup_schedule_scope
        CHECK (installation_scope ~ '^[0-9a-f]{32}$'),
    CONSTRAINT ck_image_cleanup_schedule_lease_pair
        CHECK ((lease_token IS NULL) = (lease_until IS NULL)),
    CONSTRAINT ck_image_cleanup_schedule_counts
        CHECK (last_scanned >= 0 AND last_claimed >= 0 AND last_removed >= 0
               AND last_backlog >= 0 AND last_duration_ms >= 0)
);

CREATE INDEX IF NOT EXISTS ix_image_cleanup_schedule_due
    ON "ImageCleanupSchedules" (next_run_at_utc, lease_until, installation_scope);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: dropping durable cadence/claims during a rolling
        // deployment would let old replicas resume unbounded cleanup.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_adds_durable_cadence_and_indexed_candidate_claims() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"ImageCleanupSchedules\""));
        assert!(UP_SQL.contains("next_run_at_utc TIMESTAMPTZ NOT NULL"));
        assert!(UP_SQL.contains("lease_token UUID NULL"));
        assert!(UP_SQL.contains("ck_image_cleanup_schedule_lease_pair"));
        assert!(UP_SQL.contains("cleanup_checked_at_utc TIMESTAMPTZ NULL"));
        assert!(UP_SQL.contains("cleanup_removal_started BOOLEAN NOT NULL DEFAULT FALSE"));
        assert!(UP_SQL.contains("ck_build_image_cleanup_finalizing_claim"));
        assert!(UP_SQL.contains("ix_build_image_cleanup_candidates"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_idempotent_and_enforces_paired_leases() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("image_cleanup_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "BuildImageOwnerships" (
                 installation_scope TEXT NOT NULL,
                 canonical_ref TEXT NOT NULL,
                 image_id TEXT NOT NULL,
                 updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 last_used_at_utc TIMESTAMPTZ NULL,
                 PRIMARY KEY (installation_scope, canonical_ref)
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let invalid = sqlx::query(
            r#"INSERT INTO "ImageCleanupSchedules"
                 (installation_scope, lease_token)
               VALUES ($1, $2)"#,
        )
        .bind("0123456789abcdef0123456789abcdef")
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(invalid.is_err());
        let invalid_finalizing = sqlx::query(
            r#"INSERT INTO "BuildImageOwnerships"
                 (installation_scope, canonical_ref, image_id, cleanup_removal_started)
               VALUES ($1, 'docker.io/rsctf/test:latest', 'sha256:test', TRUE)"#,
        )
        .bind("0123456789abcdef0123456789abcdef")
        .execute(&pool)
        .await;
        assert!(invalid_finalizing.is_err());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
