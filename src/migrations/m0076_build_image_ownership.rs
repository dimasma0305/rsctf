//! Persist installation-scoped ownership of mutable Docker build tags.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
 CREATE TABLE IF NOT EXISTS "BuildImageOwnerships" (
   installation_scope TEXT NOT NULL,
   canonical_ref TEXT NOT NULL,
   image_id TEXT NOT NULL,
   created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
   updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
   PRIMARY KEY (installation_scope, canonical_ref),
   CONSTRAINT ck_build_image_ownership_scope
     CHECK (installation_scope ~ '^[0-9a-f]{32}$'),
   CONSTRAINT ck_build_image_ownership_ref
     CHECK (canonical_ref<>'' AND canonical_ref=BTRIM(canonical_ref)),
   CONSTRAINT ck_build_image_ownership_id
     CHECK (image_id ~ '^sha256:[0-9A-Fa-f]{64}$')
 );
 CREATE INDEX IF NOT EXISTS ix_build_image_ownership_scope_image
   ON "BuildImageOwnerships" (installation_scope, image_id);
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
            .execute_unprepared(r#"DROP TABLE IF EXISTS "BuildImageOwnerships";"#)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn ownership_is_scoped_and_pins_each_canonical_tag_to_one_image() {
        assert!(UP_SQL.contains("PRIMARY KEY (installation_scope, canonical_ref)"));
        assert!(UP_SQL.contains("image_id ~ '^sha256:"));
        assert!(UP_SQL.contains("ix_build_image_ownership_scope_image"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_idempotent_and_rejects_invalid_ownership() {
        use std::str::FromStr;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("image_ownership_{}", uuid::Uuid::new_v4().simple());
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
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let id = format!("sha256:{}", "a".repeat(64));
        sqlx::query(
            r#"INSERT INTO "BuildImageOwnerships"
               (installation_scope, canonical_ref, image_id)
               VALUES ($1, $2, $3)"#,
        )
        .bind("0123456789abcdef0123456789abcdef")
        .bind("docker.io/rsctf/game/app:latest")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
        assert!(sqlx::query(
            r#"INSERT INTO "BuildImageOwnerships"
               (installation_scope, canonical_ref, image_id)
               VALUES ('invalid', 'docker.io/rsctf/game/other:latest', $1)"#,
        )
        .bind(&id)
        .execute(&pool)
        .await
        .is_err());

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
