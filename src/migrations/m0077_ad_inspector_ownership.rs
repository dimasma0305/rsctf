//! Bind every short-lived A&D inspector container to exactly one team service.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
 ALTER TABLE "Containers"
   ADD COLUMN IF NOT EXISTS ad_team_service_id INTEGER;

 CREATE UNIQUE INDEX IF NOT EXISTS ux_containers_ad_team_service
   ON "Containers" (ad_team_service_id)
   WHERE ad_team_service_id IS NOT NULL;

 DO $migration$
 BEGIN
   IF NOT EXISTS (
     SELECT 1 FROM pg_constraint
      WHERE conname = 'fk_containers_ad_team_service'
        AND conrelid = '"Containers"'::regclass
   ) THEN
     ALTER TABLE "Containers"
       ADD CONSTRAINT fk_containers_ad_team_service
       FOREIGN KEY (ad_team_service_id)
       REFERENCES "AdTeamServices" (id)
       ON DELETE CASCADE;
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
            .execute_unprepared(
                r#"ALTER TABLE "Containers"
                     DROP CONSTRAINT IF EXISTS fk_containers_ad_team_service;
                   DROP INDEX IF EXISTS ux_containers_ad_team_service;
                   ALTER TABLE "Containers"
                     DROP COLUMN IF EXISTS ad_team_service_id;"#,
            )
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
    fn inspector_owner_is_unique_and_service_deletion_revokes_the_row() {
        assert!(UP_SQL.contains("UNIQUE INDEX"));
        assert!(UP_SQL.contains("WHERE ad_team_service_id IS NOT NULL"));
        assert!(UP_SQL.contains("ON DELETE CASCADE"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_idempotent_unique_and_cascading() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("ad_inspector_{}", uuid::Uuid::new_v4().simple());
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
            r#"CREATE TABLE "AdTeamServices" (id INTEGER PRIMARY KEY);
               CREATE TABLE "Containers" (id UUID PRIMARY KEY);"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(r#"INSERT INTO "AdTeamServices" (id) VALUES (7)"#)
            .execute(&pool)
            .await
            .unwrap();
        let first = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Containers" (id, ad_team_service_id) VALUES ($1, 7)"#)
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        let duplicate =
            sqlx::query(r#"INSERT INTO "Containers" (id, ad_team_service_id) VALUES ($1, 7)"#)
                .bind(uuid::Uuid::new_v4())
                .execute(&pool)
                .await
                .unwrap_err();
        assert!(matches!(
            duplicate,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23505")
        ));

        sqlx::query(r#"DELETE FROM "AdTeamServices" WHERE id = 7"#)
            .execute(&pool)
            .await
            .unwrap();
        let remaining: i64 =
            sqlx::query_scalar(r#"SELECT count(*) FROM "Containers" WHERE id = $1"#)
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
