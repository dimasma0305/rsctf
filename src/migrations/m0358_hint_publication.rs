//! Adds an explicit organizer-controlled publication boundary for challenge hints.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS released_hint_count INTEGER;

UPDATE "GameChallenges"
   SET released_hint_count = CASE
       WHEN is_enabled AND json_typeof(hints) = 'array' THEN json_array_length(hints)
       ELSE 0
   END
 WHERE released_hint_count IS NULL;

ALTER TABLE "GameChallenges"
    ALTER COLUMN released_hint_count SET DEFAULT 0,
    ALTER COLUMN released_hint_count SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_game_challenges_released_hint_count'
           AND conrelid = '"GameChallenges"'::regclass
    ) THEN
        ALTER TABLE "GameChallenges"
            ADD CONSTRAINT ck_game_challenges_released_hint_count
            CHECK (released_hint_count >= 0);
    END IF;
END $$;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_released_hint_count;
ALTER TABLE "GameChallenges"
    DROP COLUMN IF EXISTS released_hint_count;
"#;

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
    fn live_hints_remain_published_while_staged_and_new_hints_are_drafts() {
        assert!(UP_SQL.contains("SET DEFAULT 0"));
        assert!(UP_SQL.contains("WHERE released_hint_count IS NULL"));
        assert!(UP_SQL.contains("WHEN is_enabled AND"));
        assert!(UP_SQL.contains("json_array_length(hints)"));
        assert!(UP_SQL.contains("released_hint_count >= 0"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_backfill_matches_the_existing_json_column_and_is_idempotent() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("hint_publication_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = crate::migrations::test_pg_connect_options(&database_url)
            .options([("search_path", schema.as_str())]);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE "GameChallenges" (
                id INTEGER PRIMARY KEY,
                hints JSON,
                is_enabled BOOLEAN NOT NULL
            );
            INSERT INTO "GameChallenges" (id, hints, is_enabled) VALUES
                (1, '["live one", "live two"]', TRUE),
                (2, '["staged"]', FALSE),
                (3, NULL, TRUE);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let rows = sqlx::query_as::<_, (i32, i32)>(
            r#"SELECT id, released_hint_count
                 FROM "GameChallenges" ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows, vec![(1, 2), (2, 0), (3, 0)]);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
