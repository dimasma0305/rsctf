//! Add a durable authorization fence for multi-stage game deletion.
//!
//! Runtime teardown can outlive the interval between an event's scheduled
//! start and the wall clock. Once a pre-start, evidence-free delete commits
//! this marker, retries may finish after that instant without treating time
//! passage itself as new competition history.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    ALTER TABLE "Games"
      ADD COLUMN IF NOT EXISTS deletion_pending BOOLEAN;
    UPDATE "Games"
       SET deletion_pending = FALSE
     WHERE deletion_pending IS NULL;
    ALTER TABLE "Games"
      ALTER COLUMN deletion_pending SET DEFAULT FALSE,
      ALTER COLUMN deletion_pending SET NOT NULL;
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
            .execute_unprepared(r#"ALTER TABLE "Games" DROP COLUMN IF EXISTS deletion_pending;"#)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::UP_SQL;

    #[test]
    fn migration_backfills_before_enforcing_the_default_and_not_null() {
        let add = UP_SQL.find("ADD COLUMN IF NOT EXISTS").unwrap();
        let backfill = UP_SQL.find("SET deletion_pending = FALSE").unwrap();
        let not_null = UP_SQL.find("SET NOT NULL").unwrap();
        assert!(add < backfill && backfill < not_null);
        assert!(UP_SQL.contains("SET DEFAULT FALSE"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn legacy_games_are_backfilled_and_new_games_default_to_not_pending() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("game_deletion_fence_{}", uuid::Uuid::new_v4().simple());
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
            r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
               INSERT INTO "Games" (id) VALUES (1);"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES (2)"#)
            .execute(&pool)
            .await
            .unwrap();
        let states: Vec<(i32, bool)> =
            sqlx::query_as(r#"SELECT id, deletion_pending FROM "Games" ORDER BY id"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(states, vec![(1, false), (2, false)]);
        let null_rejected =
            sqlx::query(r#"INSERT INTO "Games" (id, deletion_pending) VALUES (3, NULL)"#)
                .execute(&pool)
                .await
                .unwrap_err();
        assert!(matches!(
            null_rejected,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23502")
        ));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
