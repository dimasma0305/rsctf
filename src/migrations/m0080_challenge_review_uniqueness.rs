//! Make player challenge-review upserts race-safe.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const INDEX_NAME: &str = "ux_challenge_reviews_user_challenge";

const UP_SQL: &str = r#"
LOCK TABLE "ChallengeReviews" IN SHARE ROW EXCLUSIVE MODE;

DELETE FROM "ChallengeReviews"
 WHERE id IN (
    SELECT id
      FROM (
        SELECT id,
               row_number() OVER (
                   PARTITION BY user_id, challenge_id
                   ORDER BY submit_time_utc DESC, id DESC
               ) AS duplicate_number
          FROM "ChallengeReviews"
      ) ranked
     WHERE duplicate_number > 1
 );

CREATE UNIQUE INDEX IF NOT EXISTS ux_challenge_reviews_user_challenge
    ON "ChallengeReviews"(user_id, challenge_id);
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
            .execute_unprepared(&format!("DROP INDEX IF EXISTS {INDEX_NAME};"))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{INDEX_NAME, UP_SQL};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn deduplicates_before_installing_the_review_identity_index() {
        let delete = UP_SQL.find("DELETE FROM").unwrap();
        let create = UP_SQL.find("CREATE UNIQUE INDEX").unwrap();
        assert!(delete < create);
        assert!(UP_SQL.contains("PARTITION BY user_id, challenge_id"));
        assert!(UP_SQL.contains("ORDER BY submit_time_utc DESC, id DESC"));
        assert!(UP_SQL.contains(INDEX_NAME));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_keeps_the_latest_review_and_is_idempotent() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("review_unique_{}", uuid::Uuid::new_v4().simple());
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
            r#"
            CREATE TABLE "ChallengeReviews" (
                id SERIAL PRIMARY KEY,
                challenge_id INTEGER NOT NULL,
                user_id UUID NOT NULL,
                game_id INTEGER NOT NULL,
                rating SMALLINT NOT NULL,
                comment TEXT,
                submit_time_utc TIMESTAMPTZ NOT NULL
            );
            INSERT INTO "ChallengeReviews"
                (challenge_id, user_id, game_id, rating, comment, submit_time_utc)
            VALUES
                (7, '00000000-0000-0000-0000-000000000001', 3, 1, 'old',
                 '2025-01-01T00:00:00Z'),
                (7, '00000000-0000-0000-0000-000000000001', 3, 2, 'new',
                 '2025-01-02T00:00:00Z');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        for _ in 0..2 {
            let mut transaction = pool.begin().await.unwrap();
            sqlx::raw_sql(UP_SQL)
                .execute(&mut *transaction)
                .await
                .unwrap();
            transaction.commit().await.unwrap();
        }
        let row: (i64, Option<String>) =
            sqlx::query_as(r#"SELECT COUNT(*), MAX(comment) FROM "ChallengeReviews""#)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row, (1, Some("new".to_string())));

        let duplicate = sqlx::query(
            r#"INSERT INTO "ChallengeReviews"
                (challenge_id, user_id, game_id, rating, submit_time_utc)
               VALUES (7, '00000000-0000-0000-0000-000000000001', 3, 3, now())"#,
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(matches!(
            duplicate,
            sqlx::Error::Database(ref error)
                if error.code().as_deref() == Some("23505")
        ));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
