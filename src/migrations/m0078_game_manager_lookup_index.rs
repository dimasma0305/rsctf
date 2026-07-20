//! Make exact game-manager membership atomic and fast.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const INDEX_NAME: &str = "ux_gamemanagers_game_user";
const UP_SQL: &str = r#"
-- Freeze both grant writers and parent deletion while legacy rows are cleaned
-- and the foreign keys are installed. Otherwise a live parent delete could
-- create a fresh orphan between the cleanup DELETE and ALTER TABLE validation.
LOCK TABLE "Games", "AspNetUsers", "GameManagers" IN SHARE MODE;

DELETE FROM "GameManagers" duplicate
 USING "GameManagers" keeper
 WHERE duplicate.game_id = keeper.game_id
   AND duplicate.user_id = keeper.user_id
   AND duplicate.id > keeper.id;

DELETE FROM "GameManagers" manager
 WHERE NOT EXISTS (
         SELECT 1 FROM "Games" game WHERE game.id = manager.game_id
       )
    OR NOT EXISTS (
         SELECT 1 FROM "AspNetUsers" account WHERE account.id = manager.user_id
       );

CREATE UNIQUE INDEX IF NOT EXISTS ux_gamemanagers_game_user
  ON "GameManagers" (game_id, user_id);

DO $migration$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'fk_gamemanagers_game'
       AND conrelid = '"GameManagers"'::regclass
  ) THEN
    ALTER TABLE "GameManagers"
      ADD CONSTRAINT fk_gamemanagers_game
      FOREIGN KEY (game_id) REFERENCES "Games" (id) ON DELETE CASCADE;
  END IF;

  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conname = 'fk_gamemanagers_user'
       AND conrelid = '"GameManagers"'::regclass
  ) THEN
    ALTER TABLE "GameManagers"
      ADD CONSTRAINT fk_gamemanagers_user
      FOREIGN KEY (user_id) REFERENCES "AspNetUsers" (id) ON DELETE CASCADE;
  END IF;
END;
$migration$;
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
            .execute_unprepared(&format!(
                r#"ALTER TABLE "GameManagers"
                     DROP CONSTRAINT IF EXISTS fk_gamemanagers_user,
                     DROP CONSTRAINT IF EXISTS fk_gamemanagers_game;
                   DROP INDEX IF EXISTS {INDEX_NAME};"#
            ))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{INDEX_NAME, UP_SQL};

    #[test]
    fn membership_lookup_deduplicates_by_lowest_id_before_adding_unique_index() {
        assert!(UP_SQL
            .contains("LOCK TABLE \"Games\", \"AspNetUsers\", \"GameManagers\" IN SHARE MODE"));
        assert!(UP_SQL.contains("DELETE FROM \"GameManagers\" duplicate"));
        assert!(UP_SQL.contains("duplicate.id > keeper.id"));
        assert!(UP_SQL.contains("DELETE FROM \"GameManagers\" manager"));
        assert!(UP_SQL.contains("SELECT 1 FROM \"Games\" game"));
        assert!(UP_SQL.contains("SELECT 1 FROM \"AspNetUsers\" account"));
        assert!(UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS"));
        assert!(UP_SQL.contains("ON \"GameManagers\" (game_id, user_id)"));
        assert!(
            UP_SQL.contains("FOREIGN KEY (game_id) REFERENCES \"Games\" (id) ON DELETE CASCADE")
        );
        assert!(UP_SQL
            .contains("FOREIGN KEY (user_id) REFERENCES \"AspNetUsers\" (id) ON DELETE CASCADE"));
        assert!(UP_SQL.contains("conrelid = '\"GameManagers\"'::regclass"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_deduplicates_deterministically_and_is_idempotent() {
        use std::str::FromStr;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("game_manager_index_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
               CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "GameManagers" (
                 id SERIAL PRIMARY KEY,
                 game_id INTEGER NOT NULL,
                 user_id UUID NOT NULL
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let user_id = uuid::Uuid::new_v4();
        let other_user_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES (7), (8)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1), ($2)"#)
            .bind(user_id)
            .bind(other_user_id)
            .execute(&pool)
            .await
            .unwrap();
        for id in [12, 3, 8] {
            sqlx::query(r#"INSERT INTO "GameManagers" (id, game_id, user_id) VALUES ($1, 7, $2)"#)
                .bind(id)
                .bind(user_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(
            r#"INSERT INTO "GameManagers" (id, game_id, user_id) VALUES
                 (30, 999, $1),
                 (31, 8, $2),
                 (32, 8, $1)"#,
        )
        .bind(user_id)
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let index_is_unique: bool = sqlx::query_scalar(
            r#"SELECT i.indisunique
                 FROM pg_class table_relation
                 JOIN pg_index i ON i.indrelid = table_relation.oid
                 JOIN pg_class index_relation ON index_relation.oid = i.indexrelid
                WHERE table_relation.relnamespace = current_schema()::regnamespace
                  AND table_relation.relname = 'GameManagers'
                  AND index_relation.relname = $1"#,
        )
        .bind(INDEX_NAME)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(index_is_unique);

        let retained_ids: Vec<i32> = sqlx::query_scalar(
            r#"SELECT id FROM "GameManagers" WHERE game_id = 7 AND user_id = $1"#,
        )
        .bind(user_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(retained_ids, vec![3]);
        let retained_all: Vec<i32> =
            sqlx::query_scalar(r#"SELECT id FROM "GameManagers" ORDER BY id"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(retained_all, vec![3, 32]);

        let duplicate =
            sqlx::query(r#"INSERT INTO "GameManagers" (id, game_id, user_id) VALUES (20, 7, $1)"#)
                .bind(user_id)
                .execute(&pool)
                .await
                .unwrap_err();
        assert!(matches!(
            duplicate,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23505")
        ));

        sqlx::query(r#"DELETE FROM "Games" WHERE id = 8"#)
            .execute(&pool)
            .await
            .unwrap();
        let after_game_delete: i64 =
            sqlx::query_scalar(r#"SELECT count(*) FROM "GameManagers" WHERE game_id = 8"#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after_game_delete, 0);

        sqlx::query(r#"DELETE FROM "AspNetUsers" WHERE id = $1"#)
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        let after_user_delete: i64 =
            sqlx::query_scalar(r#"SELECT count(*) FROM "GameManagers" WHERE user_id = $1"#)
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after_user_delete, 0);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
