//! Require every successful challenge-image build to carry an immutable runtime
//! reference. Legacy successful rows that already configured a repository digest
//! are adopted; mutable-tag rows are queued for an explicit rebuild/pull.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    UPDATE "GameChallenges"
       SET build_image_digest = BTRIM(container_image)
     WHERE build_status = 1
       AND (build_image_digest IS NULL OR BTRIM(build_image_digest) = '')
       AND BTRIM(container_image) ~ '^[^[:space:]@]+@sha256:[0-9A-Fa-f]{64}$';

    UPDATE "GameChallenges"
       SET build_status = 1,
           build_image_digest = BTRIM(container_image),
           last_build_log = CONCAT_WS(E'\n', NULLIF(last_build_log, ''),
               'Legacy pre-pinned image adopted as the immutable runtime reference.')
     WHERE build_status = 0
       AND "Type" IN (1, 3, 4, 5)
       AND BTRIM(container_image) ~ '^[^[:space:]@]+@sha256:[0-9A-Fa-f]{64}$';

    UPDATE "GameChallenges"
       SET build_status = 5,
           build_image_digest = NULL,
           last_build_log = CONCAT_WS(E'\n', NULLIF(last_build_log, ''),
               'Legacy mutable image queued: rebuild to resolve an immutable image digest.')
     WHERE build_status = 0
       AND "Type" IN (1, 3, 4, 5)
       AND NULLIF(BTRIM(container_image), '') IS NOT NULL;

    UPDATE "GameChallenges"
       SET build_status = CASE
               WHEN NULLIF(BTRIM(container_image), '') IS NULL THEN 4
               ELSE 5
           END,
           build_image_digest = NULL,
           last_build_log = CONCAT_WS(E'\n', NULLIF(last_build_log, ''),
               'Legacy success invalidated: rebuild to resolve an immutable image digest.')
     WHERE build_status = 1
       AND NOT (
           COALESCE(BTRIM(build_image_digest), '') ~ '^sha256:[0-9A-Fa-f]{64}$'
           OR COALESCE(BTRIM(build_image_digest), '') ~ '^[^[:space:]@]+@sha256:[0-9A-Fa-f]{64}$'
       );

    DO $$
    BEGIN
      IF NOT EXISTS (
          SELECT 1 FROM pg_constraint
           WHERE conname = 'ck_gamechallenges_success_image_digest'
             AND conrelid = '"GameChallenges"'::regclass
      ) THEN
        ALTER TABLE "GameChallenges"
          ADD CONSTRAINT ck_gamechallenges_success_image_digest
          CHECK (
            build_status <> 1 OR
            COALESCE(BTRIM(build_image_digest), '') ~ '^sha256:[0-9A-Fa-f]{64}$' OR
            COALESCE(BTRIM(build_image_digest), '') ~ '^[^[:space:]@]+@sha256:[0-9A-Fa-f]{64}$'
          ) NOT VALID;
      END IF;
    END $$;

    ALTER TABLE "GameChallenges"
      VALIDATE CONSTRAINT ck_gamechallenges_success_image_digest;
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
        // The previous mutable tag cannot be reconstructed as a trustworthy
        // digest. Keeping the row queued is the only safe rollback behavior.
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE "GameChallenges"
                     DROP CONSTRAINT IF EXISTS ck_gamechallenges_success_image_digest;"#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn adopts_pinned_legacy_refs_and_invalidates_mutable_successes() {
        assert!(UP_SQL.contains("build_image_digest = BTRIM(container_image)"));
        assert!(UP_SQL.contains("@sha256:[0-9A-Fa-f]{64}"));
        assert!(UP_SQL.contains("ELSE 5"));
        assert!(UP_SQL.contains("WHERE build_status = 0"));
        assert!(UP_SQL.contains("\"Type\" IN (1, 3, 4, 5)"));
        assert!(UP_SQL.contains("build_image_digest = NULL"));
        assert!(UP_SQL.contains("ck_gamechallenges_success_image_digest"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn legacy_rows_are_repaired_and_the_success_invariant_is_enforced() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("immutable_images_{}", uuid::Uuid::new_v4().simple());
        let repository_digest = "registry.example/team/app@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let local_id = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."GameChallenges" (
                id INTEGER PRIMARY KEY,
                container_image TEXT,
                "Type" SMALLINT NOT NULL,
                build_status SMALLINT NOT NULL,
                build_image_digest TEXT,
                last_build_log TEXT
            );
            INSERT INTO "{schema}"."GameChallenges" VALUES
                (1, 'registry.example/team/app:latest', 1, 1, NULL, NULL),
                (2, '{repository_digest}', 1, 1, NULL, NULL),
                (3, 'rsctf/local:latest', 1, 1, '{local_id}', NULL),
                (4, NULL, 1, 1, NULL, NULL),
                (5, 'registry.example/team/legacy:latest', 1, 0, NULL, NULL),
                (6, '{repository_digest}', 1, 0, NULL, NULL);
            "#
        );
        sqlx::raw_sql(&setup).execute(&admin).await.unwrap();

        let search_path_schema = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .after_connect(move |connection, _metadata| {
                let statement = format!(r#"SET search_path TO "{search_path_schema}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let rows = sqlx::query_as::<_, (i32, i16, Option<String>)>(
            r#"SELECT id, build_status, build_image_digest
                 FROM "GameChallenges" ORDER BY id"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows[0], (1, 5, None));
        assert_eq!(rows[1], (2, 1, Some(repository_digest.to_string())));
        assert_eq!(rows[2], (3, 1, Some(local_id.to_string())));
        assert_eq!(rows[3], (4, 4, None));
        assert_eq!(rows[4], (5, 5, None));
        assert_eq!(rows[5], (6, 1, Some(repository_digest.to_string())));

        let invalid = sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET build_status = 1, build_image_digest = NULL WHERE id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(matches!(
            invalid,
            sqlx::Error::Database(error) if error.code().as_deref() == Some("23514")
        ));

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup).execute(&admin).await.unwrap();
    }
}
