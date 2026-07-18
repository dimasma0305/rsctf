//! Allow successful trusted-worker builds to persist their immutable,
//! worker-scoped local image identity without weakening the existing digest
//! requirement for other runtimes.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    ALTER TABLE "GameChallenges"
        DROP CONSTRAINT IF EXISTS ck_gamechallenges_success_image_digest;

    ALTER TABLE "GameChallenges"
        ADD CONSTRAINT ck_gamechallenges_success_image_digest
        CHECK (
            build_status <> 1 OR
            COALESCE(BTRIM(build_image_digest), '') ~ '^sha256:[0-9A-Fa-f]{64}$' OR
            COALESCE(BTRIM(build_image_digest), '') ~ '^[^[:space:]@]+@sha256:[0-9A-Fa-f]{64}$' OR
            COALESCE(BTRIM(build_image_digest), '') ~ '^worker://[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/sha256:[0-9A-Fa-f]{64}$'
        ) NOT VALID;

    ALTER TABLE "GameChallenges"
        VALIDATE CONSTRAINT ck_gamechallenges_success_image_digest;
"#;

const DOWN_SQL: &str = r#"
    UPDATE "GameChallenges"
       SET build_status = 5,
           build_image_digest = NULL,
           last_build_log = CONCAT_WS(E'\n', NULLIF(last_build_log, ''),
               'Worker-local build queued after rolling back worker-local digest support.')
     WHERE build_status = 1
       AND COALESCE(BTRIM(build_image_digest), '') ~ '^worker://[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/sha256:[0-9A-Fa-f]{64}$';

    ALTER TABLE "GameChallenges"
        DROP CONSTRAINT IF EXISTS ck_gamechallenges_success_image_digest;

    ALTER TABLE "GameChallenges"
        ADD CONSTRAINT ck_gamechallenges_success_image_digest
        CHECK (
            build_status <> 1 OR
            COALESCE(BTRIM(build_image_digest), '') ~ '^sha256:[0-9A-Fa-f]{64}$' OR
            COALESCE(BTRIM(build_image_digest), '') ~ '^[^[:space:]@]+@sha256:[0-9A-Fa-f]{64}$'
        ) NOT VALID;

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
        manager
            .get_connection()
            .execute_unprepared(DOWN_SQL)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DOWN_SQL, UP_SQL};
    use sqlx::postgres::PgPoolOptions;

    const WORKER_IMAGE: &str = "worker://018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb/sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn replaces_the_shipped_constraint_with_worker_local_digest_support() {
        assert!(UP_SQL.contains("DROP CONSTRAINT IF EXISTS"));
        assert!(UP_SQL.contains("^sha256:[0-9A-Fa-f]{64}$"));
        assert!(UP_SQL.contains("@sha256:[0-9A-Fa-f]{64}$"));
        assert!(UP_SQL.contains(
            "^worker://[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/sha256:[0-9A-Fa-f]{64}$"
        ));
        assert!(UP_SQL.contains("VALIDATE CONSTRAINT"));
        assert!(DOWN_SQL.contains("SET build_status = 5"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn accepts_only_canonical_immutable_worker_local_images() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("worker_local_digest_{}", uuid::Uuid::new_v4().simple());
        let setup = format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."GameChallenges" (
                id INTEGER PRIMARY KEY,
                build_status SMALLINT NOT NULL,
                build_image_digest TEXT,
                last_build_log TEXT
            );
            ALTER TABLE "{schema}"."GameChallenges"
                ADD CONSTRAINT ck_gamechallenges_success_image_digest
                CHECK (
                    build_status <> 1 OR
                    COALESCE(BTRIM(build_image_digest), '') ~ '^sha256:[0-9A-Fa-f]{{64}}$' OR
                    COALESCE(BTRIM(build_image_digest), '') ~ '^[^[:space:]@]+@sha256:[0-9A-Fa-f]{{64}}$'
                );
            INSERT INTO "{schema}"."GameChallenges" VALUES
                (1, 1, 'sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', NULL),
                (2, 1, 'registry.example/team/app@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', NULL);
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
        sqlx::query(
            r#"INSERT INTO "GameChallenges"
               (id, build_status, build_image_digest) VALUES (3, 1, $1)"#,
        )
        .bind(WORKER_IMAGE)
        .execute(&pool)
        .await
        .unwrap();

        for invalid in [
            "worker://018f3c6a-d79b-7cc0-8f68-8fdbad0f57bb/sha256:short",
            "worker://not-a-uuid/sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "registry.example/team/app:latest",
        ] {
            let error = sqlx::query(
                r#"INSERT INTO "GameChallenges"
                   (id, build_status, build_image_digest) VALUES (10, 1, $1)"#,
            )
            .bind(invalid)
            .execute(&pool)
            .await
            .unwrap_err();
            assert!(matches!(
                error,
                sqlx::Error::Database(database) if database.code().as_deref() == Some("23514")
            ));
        }

        sqlx::raw_sql(DOWN_SQL).execute(&pool).await.unwrap();
        let rolled_back = sqlx::query_as::<_, (i16, Option<String>)>(
            r#"SELECT build_status, build_image_digest
                 FROM "GameChallenges" WHERE id = 3"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rolled_back, (5, None));

        pool.close().await;
        let cleanup = format!(r#"DROP SCHEMA "{schema}" CASCADE"#);
        sqlx::query(&cleanup).execute(&admin).await.unwrap();
    }
}
