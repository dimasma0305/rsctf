//! Keep scoped KotH tokens valid while their target is deleted.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_target;
                ALTER TABLE "KothTokens"
                  ADD CONSTRAINT fk_kothtokens_target
                  FOREIGN KEY (target_id) REFERENCES "KothTargets"(id)
                  ON DELETE CASCADE;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE "KothTokens"
                  DROP CONSTRAINT IF EXISTS fk_kothtokens_target;
                ALTER TABLE "KothTokens"
                  ADD CONSTRAINT fk_kothtokens_target
                  FOREIGN KEY (target_id) REFERENCES "KothTargets"(id)
                  ON DELETE SET NULL;
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Connection;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn target_fk_deletes_scoped_tokens() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to the migrated PostgreSQL database");
        let mut connection = sqlx::PgConnection::connect(&database_url).await.unwrap();
        let definition: String = sqlx::query_scalar(
            r#"
            SELECT pg_get_constraintdef(oid)
              FROM pg_constraint
             WHERE conrelid = '"KothTokens"'::regclass
               AND conname = 'fk_kothtokens_target'
            "#,
        )
        .fetch_one(&mut connection)
        .await
        .expect("KotH target foreign key should exist");

        assert!(
            definition.ends_with("ON DELETE CASCADE"),
            "unexpected KotH target foreign key: {definition}"
        );
    }
}
