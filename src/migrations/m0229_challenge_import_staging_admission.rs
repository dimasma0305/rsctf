//! Reserve durable ZIP-import admission before storing the uploaded archive.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "ChallengeImportJobs"
    ADD COLUMN IF NOT EXISTS source_staged BOOLEAN NOT NULL DEFAULT TRUE;

DO $migration$
DECLARE
    constraint_name TEXT;
BEGIN
    FOR constraint_name IN
        SELECT conname
          FROM pg_constraint
         WHERE conrelid = '"ChallengeImportJobs"'::regclass
           AND contype = 'c'
           AND position('source_file_id' IN pg_get_constraintdef(oid)) > 0
           AND position('repo_url' IN pg_get_constraintdef(oid)) > 0
    LOOP
        EXECUTE format(
            'ALTER TABLE "ChallengeImportJobs" DROP CONSTRAINT %I',
            constraint_name
        );
    END LOOP;
END
$migration$;

ALTER TABLE "ChallengeImportJobs"
    DROP CONSTRAINT IF EXISTS ck_challengeimportjobs_source_fields_v2;
ALTER TABLE "ChallengeImportJobs"
    ADD CONSTRAINT ck_challengeimportjobs_source_fields_v2 CHECK (
        (source_kind = 0
         AND repo_url IS NULL
         AND token_ciphertext IS NULL
         AND token_nonce IS NULL
         AND (
             source_file_id IS NOT NULL
             OR status IN (2, 3)
             OR (source_staged = FALSE AND status = 0)
         ))
        OR
        (source_kind = 1
         AND source_file_id IS NULL
         AND source_staged = TRUE
         AND repo_url IS NOT NULL
         AND (
             (token_ciphertext IS NULL AND token_nonce IS NULL)
             OR (token_ciphertext IS NOT NULL AND octet_length(token_nonce) = 12)
         ))
    ) NOT VALID;
ALTER TABLE "ChallengeImportJobs"
    VALIDATE CONSTRAINT ck_challengeimportjobs_source_fields_v2;
"#;

const DOWN_SQL: &str = r#"
ALTER TABLE "ChallengeImportJobs"
    DROP CONSTRAINT IF EXISTS ck_challengeimportjobs_source_fields_v2;
ALTER TABLE "ChallengeImportJobs"
    DROP COLUMN IF EXISTS source_staged;
ALTER TABLE "ChallengeImportJobs"
    DROP CONSTRAINT IF EXISTS ck_challengeimportjobs_source_fields;
ALTER TABLE "ChallengeImportJobs"
    ADD CONSTRAINT ck_challengeimportjobs_source_fields CHECK (
        (source_kind = 0 AND (source_file_id IS NOT NULL OR status IN (2, 3))
            AND repo_url IS NULL AND token_ciphertext IS NULL AND token_nonce IS NULL)
        OR
        (source_kind = 1 AND source_file_id IS NULL AND repo_url IS NOT NULL
            AND ((token_ciphertext IS NULL AND token_nonce IS NULL)
                OR (token_ciphertext IS NOT NULL AND octet_length(token_nonce) = 12)))
    );
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
    use super::UP_SQL;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn staging_reservation_is_bounded_to_unclaimed_zip_jobs() {
        assert!(UP_SQL.contains("source_staged BOOLEAN NOT NULL DEFAULT TRUE"));
        assert!(UP_SQL.contains("source_staged = FALSE AND status = 0"));
        assert!(UP_SQL.contains("source_kind = 1"));
        assert!(UP_SQL.contains("source_staged = TRUE"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn forward_migration_is_idempotent_and_preserves_source_invariants() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("rsctf_m0229_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
               CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "Files" (id INTEGER PRIMARY KEY, reference_count BIGINT NOT NULL DEFAULT 1);
               CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::m0143_challenge_import_jobs::UP_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let actor = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES (7)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(actor)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "ChallengeImportJobs"
                  (id, game_id, actor_user_id, operation_id, source_kind,
                   import_policy, source_key, source_staged)
                VALUES ($1, 7, $2, $3, 0, 0, 'zip-reservation', FALSE)"#,
        )
        .bind(uuid::Uuid::new_v4())
        .bind(actor)
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await
        .expect("unstaged ZIP reservation is valid");
        let invalid_git = sqlx::query(
            r#"INSERT INTO "ChallengeImportJobs"
                  (id, game_id, actor_user_id, operation_id, source_kind,
                   import_policy, source_key, source_staged, repo_url)
                VALUES ($1, 7, $2, $3, 1, 0, 'git-reservation', FALSE,
                        'https://github.com/TCP1P/repo.git')"#,
        )
        .bind(uuid::Uuid::new_v4())
        .bind(actor)
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await;
        assert!(
            invalid_git.is_err(),
            "Git jobs cannot bypass source readiness"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
