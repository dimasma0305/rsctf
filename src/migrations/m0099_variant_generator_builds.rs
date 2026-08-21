//! Auto-build trusted `generator/Dockerfile` provenance sources while keeping
//! the author-facing manifest free of deployment-generated image identities.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS variant_generator_build_context_subdir TEXT NULL,
    ADD COLUMN IF NOT EXISTS variant_generator_build_status SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS variant_generator_last_build_log TEXT NULL;

ALTER TABLE "GameChallenges"
    ALTER COLUMN variant_generator_build_status SET DEFAULT 0,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_config,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_generator_build;

ALTER TABLE "GameChallenges"
    ADD CONSTRAINT ck_game_challenges_variant_generator_build CHECK (
        (
            variant_generator_build_context_subdir IS NULL
            AND variant_generator_build_status = 0
        )
        OR (
            variant_generator_build_context_subdir IS NOT NULL
            AND variant_generator_build_context_subdir = 'generator'
            AND variant_generator_build_status IN (1, 2, 3, 5, 6)
            AND (
                (
                    variant_generator_build_status = 1
                    AND variant_generator_image IS NOT NULL
                    AND variant_generator_digest IS NOT NULL
                    AND variant_generator_image = variant_generator_digest
                    AND variant_generator_digest ~ '^sha256:[0-9a-f]{64}$'
                )
                OR (
                    variant_generator_build_status <> 1
                    AND variant_generator_image IS NULL
                    AND variant_generator_digest IS NULL
                )
            )
        )
    ),
    ADD CONSTRAINT ck_game_challenges_variant_config CHECK (
        variant_mode = 0
        OR (
            variant_generator_build_context_subdir IS NULL
            AND variant_generator_image IS NOT NULL
            AND variant_generator_digest IS NOT NULL
            AND LENGTH(BTRIM(variant_generator_image)) BETWEEN 1 AND 512
            AND variant_generator_digest ~ '^sha256:[0-9a-f]{64}$'
        )
        OR (
            variant_generator_build_context_subdir IS NOT NULL
            AND variant_generator_build_context_subdir = 'generator'
        )
    );
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_config,
    DROP CONSTRAINT IF EXISTS ck_game_challenges_variant_generator_build;

UPDATE "GameChallenges"
   SET variant_mode = 0,
       variant_generator_image = NULL,
       variant_generator_digest = NULL
 WHERE variant_generator_build_context_subdir IS NOT NULL;

ALTER TABLE "GameChallenges"
    DROP COLUMN IF EXISTS variant_generator_last_build_log,
    DROP COLUMN IF EXISTS variant_generator_build_status,
    DROP COLUMN IF EXISTS variant_generator_build_context_subdir;

ALTER TABLE "GameChallenges"
    ADD CONSTRAINT ck_game_challenges_variant_config CHECK (
        variant_mode = 0
        OR (
            variant_generator_image IS NOT NULL
            AND variant_generator_digest IS NOT NULL
            AND LENGTH(BTRIM(variant_generator_image)) BETWEEN 1 AND 512
            AND variant_generator_digest ~ '^sha256:[0-9a-f]{64}$'
        )
    );
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
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_builds_are_queued_without_pretending_a_mutable_tag_is_immutable() {
        assert!(UP_SQL.contains("variant_generator_build_context_subdir = 'generator'"));
        assert!(UP_SQL.contains("variant_generator_build_status <> 1"));
        assert!(UP_SQL.contains("variant_generator_image IS NULL"));
        assert!(UP_SQL.contains("variant_generator_image = variant_generator_digest"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_idempotent_and_enforces_generator_publication_state() {
        use sqlx::postgres::PgPoolOptions;

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!(
            "variant_generator_migration_{}",
            uuid::Uuid::new_v4().simple()
        );
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(&format!(r#"SET search_path TO "{schema}""#))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "GameChallenges" (
                 id SERIAL PRIMARY KEY,
                 variant_mode SMALLINT NOT NULL DEFAULT 0,
                 variant_generator_image TEXT NULL,
                 variant_generator_digest TEXT NULL
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (variant_mode, variant_generator_build_context_subdir,
                  variant_generator_build_status)
               VALUES (1, 'generator', 5)"#,
        )
        .execute(&pool)
        .await
        .expect("queued source build has no invented identity");
        assert!(sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (variant_mode, variant_generator_build_context_subdir,
                  variant_generator_build_status)
               VALUES (1, 'generator', 1)"#,
        )
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (variant_mode, variant_generator_build_status)
               VALUES (0, 5)"#,
        )
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (variant_mode, variant_generator_build_context_subdir,
                  variant_generator_build_status)
               VALUES (0, 'generator', 1)"#,
        )
        .execute(&pool)
        .await
        .is_err());
        assert!(sqlx::query(
            r#"INSERT INTO "GameChallenges"
                 (variant_mode, variant_generator_image)
               VALUES (1, 'registry.example/generator:latest')"#,
        )
        .execute(&pool)
        .await
        .is_err());

        sqlx::raw_sql(DOWN_SQL).execute(&pool).await.unwrap();
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
