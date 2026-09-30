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
    #[ignore = "requires migrated disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_leaked_global_usage_is_repaired_and_deletion_releases_it() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let game_id: i32 = sqlx::query_scalar(r#"SELECT id FROM "Games" ORDER BY id LIMIT 1"#)
            .fetch_one(&pool)
            .await
            .expect("the disposable database needs one game");
        let mut transaction = pool.begin().await.unwrap();
        async fn totals(connection: &mut sqlx::PgConnection) -> (i64, i64) {
            sqlx::query_as(
                r#"SELECT logical_bytes, row_count
                     FROM "AntiCheatTelemetryGlobalUsage" WHERE id = 1"#,
            )
            .fetch_one(connection)
            .await
            .unwrap()
        }
        // One live game's usage, and a global total filled by deleted games.
        sqlx::raw_sql(
            r#"DELETE FROM "AntiCheatTelemetryUsage";
               INSERT INTO "AntiCheatTelemetryGlobalUsage" (id) VALUES (1)
               ON CONFLICT DO NOTHING;
               UPDATE "AntiCheatTelemetryGlobalUsage"
                  SET logical_bytes = 5368709120, row_count = 1016
                WHERE id = 1;"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "AntiCheatTelemetryUsage" (game_id, logical_bytes, row_count)
               VALUES ($1, 4096, 16)"#,
        )
        .bind(game_id)
        .execute(&mut *transaction)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL)
            .execute(&mut *transaction)
            .await
            .unwrap();
        assert_eq!(totals(&mut transaction).await, (4096, 16));
        // Removing the game's usage releases its share of the global budget.
        crate::services::event_security::release_game_usage(&mut transaction, game_id)
            .await
            .unwrap();
        assert_eq!(totals(&mut transaction).await, (0, 0));
        transaction.rollback().await.unwrap();
        pool.close().await;
    }
}
