//! Keep completed clone receipts valid after their destination event is purged.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "GameCloneOperations"
    DROP CONSTRAINT IF EXISTS ck_gamecloneoperations_terminal;

ALTER TABLE "GameCloneOperations"
    ADD CONSTRAINT ck_gamecloneoperations_terminal CHECK (
        (status = 1 AND completed_at_utc IS NOT NULL)
        OR status <> 1
    );
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Forward-only: completed clone receipts may already refer to a purged destination.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn terminal_clone_receipt_does_not_require_a_retained_destination() {
        assert!(UP_SQL.contains("DROP CONSTRAINT IF EXISTS ck_gamecloneoperations_terminal"));
        assert!(UP_SQL.contains("status = 1 AND completed_at_utc IS NOT NULL"));
        assert!(!UP_SQL.contains("destination_game_id IS NOT NULL"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn deleting_a_clone_destination_preserves_the_completed_receipt() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Games" (id INTEGER PRIMARY KEY);
            CREATE TEMP TABLE "GameCloneOperations" (
                operation_id UUID PRIMARY KEY,
                destination_game_id INTEGER REFERENCES "Games" (id) ON DELETE SET NULL,
                status SMALLINT NOT NULL,
                completed_at_utc TIMESTAMPTZ,
                CONSTRAINT ck_gamecloneoperations_terminal CHECK (
                    (status = 1 AND destination_game_id IS NOT NULL
                                AND completed_at_utc IS NOT NULL)
                    OR status <> 1
                )
            );
            "#,
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&mut *tx).await.unwrap();
        let destination = 42;
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES ($1)"#)
            .bind(destination)
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "GameCloneOperations"
                 (operation_id, destination_game_id, status, completed_at_utc)
               VALUES ($1, $2, 1, clock_timestamp())"#,
        )
        .bind(uuid::Uuid::new_v4())
        .bind(destination)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(r#"DELETE FROM "Games" WHERE id = $1"#)
            .bind(destination)
            .execute(&mut *tx)
            .await
            .unwrap();
        let retained: (i16, Option<i32>, bool) = sqlx::query_as(
            r#"SELECT status, destination_game_id, completed_at_utc IS NOT NULL
                 FROM "GameCloneOperations""#,
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(retained, (1, None, true));
        tx.rollback().await.unwrap();
    }
}
