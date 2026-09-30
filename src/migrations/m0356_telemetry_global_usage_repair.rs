//! Repair the global anti-cheat telemetry budget.
//!
//! Ingest charges a game's usage row and the global row by the same amount,
//! and a telemetry purge releases both. Deleting a game removed its usage row
//! without releasing the global share, so every deleted event kept counting
//! until the 5 GiB budget filled and telemetry switched off for every future
//! event. Deletion now releases the share; this recomputes the global total
//! from the surviving per-game rows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
-- Hold ingest out while the total is recomputed, as m0284 does.
LOCK TABLE "AntiCheatTelemetryUsage" IN EXCLUSIVE MODE;
UPDATE "AntiCheatTelemetryGlobalUsage" global
   SET logical_bytes = usage.logical_bytes,
       row_count = usage.row_count,
       updated_at_utc = clock_timestamp()
  FROM (
      SELECT COALESCE(SUM(logical_bytes), 0)::bigint AS logical_bytes,
             COALESCE(SUM(row_count), 0)::bigint AS row_count
        FROM "AntiCheatTelemetryUsage"
  ) usage
 WHERE global.id = 1
   AND (global.logical_bytes, global.row_count)
       IS DISTINCT FROM (usage.logical_bytes, usage.row_count);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // A recomputed total is the correct value; there is nothing to undo.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_global_total_is_recomputed_from_per_game_usage() {
        assert!(UP_SQL.contains(r#"FROM "AntiCheatTelemetryUsage""#));
        assert!(UP_SQL.contains("WHERE global.id = 1"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_leaked_global_usage_is_repaired_and_deletion_releases_it() {
        use std::str::FromStr;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("m0356_usage_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(
                PgConnectOptions::from_str(&database_url)
                    .unwrap()
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        // Two live games, and a global total that deleted games filled up.
        sqlx::raw_sql(
            r#"CREATE TABLE "AntiCheatTelemetryUsage" (
                   game_id INTEGER PRIMARY KEY,
                   logical_bytes BIGINT NOT NULL CHECK (logical_bytes BETWEEN 0 AND 268435456),
                   row_count BIGINT NOT NULL CHECK (row_count >= 0),
                   updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
               );
               CREATE TABLE "AntiCheatTelemetryGlobalUsage" (
                   id SMALLINT PRIMARY KEY CHECK (id = 1),
                   logical_bytes BIGINT NOT NULL
                       CHECK (logical_bytes BETWEEN 0 AND 5368709120),
                   row_count BIGINT NOT NULL CHECK (row_count >= 0),
                   updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
               );
               INSERT INTO "AntiCheatTelemetryUsage" (game_id, logical_bytes, row_count)
               VALUES (1, 4096, 16), (2, 1024, 4);
               INSERT INTO "AntiCheatTelemetryGlobalUsage" (id, logical_bytes, row_count)
               VALUES (1, 5368709120, 1020);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        async fn totals(pool: &sqlx::PgPool) -> (i64, i64) {
            sqlx::query_as(
                r#"SELECT logical_bytes, row_count
                     FROM "AntiCheatTelemetryGlobalUsage" WHERE id = 1"#,
            )
            .fetch_one(pool)
            .await
            .unwrap()
        }
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        assert_eq!(totals(&pool).await, (5120, 20));
        // Removing a game's usage releases its share once, even if repeated.
        let mut connection = pool.acquire().await.unwrap();
        for _ in 0..2 {
            crate::services::event_security::release_game_usage(&mut connection, 1)
                .await
                .unwrap();
        }
        drop(connection);
        assert_eq!(totals(&pool).await, (1024, 4));
        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
