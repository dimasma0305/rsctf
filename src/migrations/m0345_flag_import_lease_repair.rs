//! Repair flag-import lease state for databases that applied the original
//! m0309 schema before durable lease recovery was added to that migration.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "FlagImportOperations"
    ADD COLUMN IF NOT EXISTS lease_token UUID NULL;

-- Existing rows predate lease ownership. Give each one a stable, non-secret
-- token so the current recovery path can either replay a completed result or
-- reclaim an expired pending operation without a nullable runtime contract.
UPDATE "FlagImportOperations"
   SET lease_token = md5(
       challenge_id::TEXT || ':' || operation_id::TEXT || ':rsctf-flag-import-repair'
   )::UUID
 WHERE lease_token IS NULL;

ALTER TABLE "FlagImportOperations"
    ALTER COLUMN lease_token SET NOT NULL;

ALTER TABLE "FlagImportOperations"
    DROP CONSTRAINT IF EXISTS "FlagImportOperations_state_check";
ALTER TABLE "FlagImportOperations"
    DROP CONSTRAINT IF EXISTS ck_flag_import_operation_result;

ALTER TABLE "FlagImportOperations"
    ADD CONSTRAINT "FlagImportOperations_state_check"
    CHECK (state IN (0, 1, 2));
ALTER TABLE "FlagImportOperations"
    ADD CONSTRAINT ck_flag_import_operation_result
    CHECK (
        (state = 0 AND completed_at_utc IS NULL
                   AND inserted_count IS NULL AND duplicate_count IS NULL)
        OR (state = 1 AND completed_at_utc IS NOT NULL
                      AND inserted_count IS NOT NULL AND duplicate_count IS NOT NULL)
        OR (state = 2 AND completed_at_utc IS NOT NULL
                      AND inserted_count IS NULL AND duplicate_count IS NULL)
    );

CREATE TABLE IF NOT EXISTS "FlagImportSlots" (
    slot_id        SMALLINT PRIMARY KEY CHECK (slot_id BETWEEN 0 AND 3),
    lease_token    UUID NULL,
    expires_at_utc TIMESTAMPTZ NULL,
    CHECK ((lease_token IS NULL) = (expires_at_utc IS NULL))
);
INSERT INTO "FlagImportSlots" (slot_id)
VALUES (0), (1), (2), (3)
ON CONFLICT DO NOTHING;

CREATE INDEX IF NOT EXISTS ix_flag_import_slot_expiry
    ON "FlagImportSlots" (expires_at_utc, slot_id);

DROP INDEX IF EXISTS ix_flag_import_operations_retention;
CREATE INDEX ix_flag_import_operations_retention
    ON "FlagImportOperations" (completed_at_utc, challenge_id, operation_id)
    WHERE state IN (1, 2);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    // This migration converges historical schemas on the already-supported
    // runtime contract. Reintroducing the incompatible shape is not safe.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::UP_SQL;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    #[test]
    fn forward_repair_restores_every_flag_import_lease_contract() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS lease_token UUID"));
        assert!(UP_SQL.contains("ALTER COLUMN lease_token SET NOT NULL"));
        assert!(UP_SQL.contains("state IN (0, 1, 2)"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"FlagImportSlots\""));
        assert!(UP_SQL.contains("WHERE state IN (1, 2)"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn forward_repair_upgrades_the_original_schema_idempotently() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_flag_lease_repair_{}", uuid::Uuid::new_v4().simple());
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
            CREATE TABLE "FlagImportOperations" (
                challenge_id INTEGER NOT NULL,
                operation_id UUID NOT NULL,
                actor_user_id UUID NOT NULL,
                request_digest BYTEA NOT NULL CHECK (OCTET_LENGTH(request_digest) = 32),
                state SMALLINT NOT NULL DEFAULT 0 CHECK (state IN (0, 1)),
                inserted_count INTEGER NULL,
                duplicate_count INTEGER NULL,
                lease_expires_at_utc TIMESTAMPTZ NOT NULL,
                completed_at_utc TIMESTAMPTZ NULL,
                created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                staged_attachment_ids INTEGER[] NOT NULL DEFAULT '{}',
                PRIMARY KEY (challenge_id, operation_id),
                CONSTRAINT ck_flag_import_operation_result CHECK (
                    (state = 0 AND completed_at_utc IS NULL
                               AND inserted_count IS NULL AND duplicate_count IS NULL)
                    OR (state = 1 AND completed_at_utc IS NOT NULL
                                  AND inserted_count IS NOT NULL AND duplicate_count IS NOT NULL)
                )
            );
            CREATE INDEX ix_flag_import_operations_retention
                ON "FlagImportOperations" (completed_at_utc, challenge_id, operation_id)
                WHERE state = 1;
            INSERT INTO "FlagImportOperations"
                (challenge_id, operation_id, actor_user_id, request_digest, state,
                 lease_expires_at_utc)
            VALUES
                (1, '00000000-0000-0000-0000-000000000001',
                 '00000000-0000-0000-0000-000000000002', decode(repeat('00', 32), 'hex'),
                 0, clock_timestamp() - INTERVAL '1 minute');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        for _ in 0..2 {
            sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        }

        let repaired: (bool, i64) = sqlx::query_as(
            r#"SELECT lease_token IS NOT NULL,
                      (SELECT COUNT(*)::BIGINT FROM "FlagImportSlots")
                 FROM "FlagImportOperations"
                WHERE challenge_id = 1"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(repaired, (true, 4));

        sqlx::query(
            r#"UPDATE "FlagImportOperations"
                  SET state = 2, completed_at_utc = clock_timestamp()
                WHERE challenge_id = 1"#,
        )
        .execute(&pool)
        .await
        .expect("repaired operations accept the runtime failure state");
        let invalid_state =
            sqlx::query(r#"UPDATE "FlagImportOperations" SET state = 3 WHERE challenge_id = 1"#)
                .execute(&pool)
                .await
                .unwrap_err();
        assert_eq!(
            invalid_state
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("23514")
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
