//! Indexes for bounded monitor event/submission paging and normalized search.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
-- pg_trgm is a trusted PostgreSQL extension. It keeps literal contains-searches
-- indexed without changing the established monitor search semantics.
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;

CREATE INDEX IF NOT EXISTS ix_gameevents_monitor_page
    ON "GameEvents" (game_id, publish_time_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_gameevents_game_user
    ON "GameEvents" (game_id, user_id)
    WHERE user_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_submissions_monitor_status_page
    ON "Submissions" (game_id, status, submit_time_utc DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_submissions_game_team
    ON "Submissions" (game_id, team_id);
CREATE INDEX IF NOT EXISTS ix_submissions_game_user
    ON "Submissions" (game_id, user_id)
    WHERE user_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_submissions_game_challenge
    ON "Submissions" (game_id, challenge_id);

DO $migration$
DECLARE
    extension_schema TEXT;
BEGIN
    SELECT namespace.nspname
      INTO extension_schema
      FROM pg_catalog.pg_extension extension
      JOIN pg_catalog.pg_namespace namespace
        ON namespace.oid = extension.extnamespace
     WHERE extension.extname = 'pg_trgm';

    IF extension_schema IS NULL THEN
        RAISE EXCEPTION 'pg_trgm extension schema could not be resolved';
    END IF;

    EXECUTE pg_catalog.format(
        $index$CREATE INDEX IF NOT EXISTS ix_teams_monitor_name_trgm
            ON "Teams" USING GIN (LOWER(name) %I.gin_trgm_ops)$index$,
        extension_schema
    );
    EXECUTE pg_catalog.format(
        $index$CREATE INDEX IF NOT EXISTS ix_users_monitor_name_trgm
            ON "AspNetUsers" USING GIN (LOWER(user_name) %I.gin_trgm_ops)
            WHERE user_name IS NOT NULL$index$,
        extension_schema
    );
    EXECUTE pg_catalog.format(
        $index$CREATE INDEX IF NOT EXISTS ix_challenges_monitor_title_trgm
            ON "GameChallenges" USING GIN (LOWER(title) %I.gin_trgm_ops)$index$,
        extension_schema
    );
    EXECUTE pg_catalog.format(
        $index$CREATE INDEX IF NOT EXISTS ix_submissions_monitor_answer_trgm
            ON "Submissions" USING GIST (LOWER(answer) %I.gist_trgm_ops(siglen=64))$index$,
        extension_schema
    );
    EXECUTE pg_catalog.format(
        $index$CREATE INDEX IF NOT EXISTS ix_gameevents_monitor_values_trgm
            ON "GameEvents" USING GIST (LOWER(values::text) %I.gist_trgm_ops(siglen=64))$index$,
        extension_schema
    );
END
$migration$;
"#;

const DOWN_SQL: &str = r#"
DROP INDEX IF EXISTS ix_gameevents_monitor_values_trgm;
DROP INDEX IF EXISTS ix_submissions_monitor_answer_trgm;
DROP INDEX IF EXISTS ix_challenges_monitor_title_trgm;
DROP INDEX IF EXISTS ix_users_monitor_name_trgm;
DROP INDEX IF EXISTS ix_teams_monitor_name_trgm;
DROP INDEX IF EXISTS ix_submissions_game_challenge;
DROP INDEX IF EXISTS ix_submissions_game_user;
DROP INDEX IF EXISTS ix_submissions_game_team;
DROP INDEX IF EXISTS ix_submissions_monitor_status_page;
DROP INDEX IF EXISTS ix_gameevents_game_user;
DROP INDEX IF EXISTS ix_gameevents_monitor_page;
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
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[test]
    fn migration_is_forward_idempotent_and_keeps_extension_on_down() {
        assert!(UP_SQL.contains("CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public"));
        assert_eq!(UP_SQL.matches("CREATE INDEX IF NOT EXISTS").count(), 11);
        assert!(UP_SQL.contains("(game_id, publish_time_utc DESC, id DESC)"));
        assert!(UP_SQL.contains("(game_id, status, submit_time_utc DESC, id DESC)"));
        assert!(UP_SQL.contains("JOIN pg_catalog.pg_namespace namespace"));
        assert!(UP_SQL.contains("%I.gin_trgm_ops"));
        assert!(UP_SQL.contains("%I.gist_trgm_ops(siglen=64)"));
        assert!(!UP_SQL.contains("public.gin_trgm_ops"));
        assert!(!UP_SQL.contains("public.gist_trgm_ops"));
        assert!(!DOWN_SQL.contains("DROP EXTENSION"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL administrator via RSCTF_TEST_DATABASE_URL"]
    async fn resolves_trigram_operator_classes_from_a_non_public_extension_schema() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let database_name = format!("rsctf_m0107_{}", uuid::Uuid::new_v4().simple());
        assert!(database_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));

        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::query(&format!(r#"CREATE DATABASE "{database_name}""#))
            .execute(&admin)
            .await
            .unwrap();

        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .database(&database_name);
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE SCHEMA managed_extensions;
            CREATE EXTENSION pg_trgm WITH SCHEMA managed_extensions;
            CREATE SCHEMA rsctf_app;
            SET search_path TO rsctf_app;

            CREATE TABLE "GameEvents" (
                id INTEGER NOT NULL,
                game_id INTEGER NOT NULL,
                publish_time_utc TIMESTAMPTZ NOT NULL,
                user_id UUID,
                "values" JSONB NOT NULL
            );
            CREATE TABLE "Submissions" (
                id INTEGER NOT NULL,
                game_id INTEGER NOT NULL,
                status SMALLINT NOT NULL,
                submit_time_utc TIMESTAMPTZ NOT NULL,
                team_id INTEGER NOT NULL,
                user_id UUID,
                challenge_id INTEGER NOT NULL,
                answer TEXT NOT NULL
            );
            CREATE TABLE "Teams" (name TEXT NOT NULL);
            CREATE TABLE "AspNetUsers" (user_name TEXT);
            CREATE TABLE "GameChallenges" (title TEXT NOT NULL);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect("the forward migration must remain idempotent");

        let installed_schema: String = sqlx::query_scalar(
            r#"SELECT namespace.nspname
                 FROM pg_catalog.pg_extension extension
                 JOIN pg_catalog.pg_namespace namespace
                   ON namespace.oid = extension.extnamespace
                WHERE extension.extname = 'pg_trgm'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(installed_schema, "managed_extensions");

        let index_count: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint
                 FROM pg_catalog.pg_indexes
                WHERE schemaname = 'rsctf_app'
                  AND indexname = ANY(ARRAY[
                      'ix_gameevents_monitor_page',
                      'ix_gameevents_game_user',
                      'ix_submissions_monitor_status_page',
                      'ix_submissions_game_team',
                      'ix_submissions_game_user',
                      'ix_submissions_game_challenge',
                      'ix_teams_monitor_name_trgm',
                      'ix_users_monitor_name_trgm',
                      'ix_challenges_monitor_title_trgm',
                      'ix_submissions_monitor_answer_trgm',
                      'ix_gameevents_monitor_values_trgm'
                  ])"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(index_count, 11);

        let operator_classes: Vec<(String, String, String)> = sqlx::query_as(
            r#"SELECT index_relation.relname,
                      operator_namespace.nspname,
                      operator_class.opcname
                 FROM pg_catalog.pg_index index_metadata
                 JOIN pg_catalog.pg_class index_relation
                   ON index_relation.oid = index_metadata.indexrelid
                 JOIN pg_catalog.pg_namespace index_namespace
                   ON index_namespace.oid = index_relation.relnamespace
                CROSS JOIN LATERAL
                     unnest(index_metadata.indclass::oid[]) selected(operator_class_oid)
                 JOIN pg_catalog.pg_opclass operator_class
                   ON operator_class.oid = selected.operator_class_oid
                 JOIN pg_catalog.pg_namespace operator_namespace
                   ON operator_namespace.oid = operator_class.opcnamespace
                WHERE index_namespace.nspname = 'rsctf_app'
                  AND index_relation.relname IN (
                      'ix_teams_monitor_name_trgm',
                      'ix_users_monitor_name_trgm',
                      'ix_challenges_monitor_title_trgm',
                      'ix_submissions_monitor_answer_trgm',
                      'ix_gameevents_monitor_values_trgm'
                  )
                ORDER BY index_relation.relname"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(operator_classes.len(), 5);
        assert!(operator_classes.iter().all(|(_, schema, operator_class)| {
            schema == "managed_extensions"
                && matches!(operator_class.as_str(), "gin_trgm_ops" | "gist_trgm_ops")
        }));

        pool.close().await;
        sqlx::query(&format!(r#"DROP DATABASE "{database_name}""#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
