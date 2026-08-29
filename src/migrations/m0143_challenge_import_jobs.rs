//! Durable, replica-safe admission and idempotency for challenge imports.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "GameChallenges"
    ADD COLUMN IF NOT EXISTS import_source_identity VARCHAR(96);

CREATE UNIQUE INDEX IF NOT EXISTS ux_gamechallenges_import_source_identity
    ON "GameChallenges" (game_id, import_source_identity)
    WHERE import_source_identity IS NOT NULL;

CREATE TABLE IF NOT EXISTS "ChallengeImportJobs" (
    id UUID PRIMARY KEY,
    game_id INTEGER NOT NULL REFERENCES "Games" (id) ON DELETE CASCADE,
    actor_user_id UUID NOT NULL REFERENCES "AspNetUsers" (id) ON DELETE CASCADE,
    operation_id UUID NOT NULL,
    source_kind SMALLINT NOT NULL CHECK (source_kind IN (0, 1)),
    import_policy SMALLINT NOT NULL CHECK (import_policy IN (0, 1)),
    source_key TEXT NOT NULL CHECK (octet_length(source_key) BETWEEN 1 AND 2048),
    source_file_id INTEGER REFERENCES "Files" (id) ON DELETE RESTRICT,
    repo_url TEXT CHECK (repo_url IS NULL OR octet_length(repo_url) BETWEEN 1 AND 2048),
    git_ref TEXT CHECK (git_ref IS NULL OR octet_length(git_ref) BETWEEN 1 AND 255),
    subpath TEXT CHECK (subpath IS NULL OR octet_length(subpath) BETWEEN 1 AND 1024),
    token_ciphertext BYTEA CHECK (
        token_ciphertext IS NULL OR octet_length(token_ciphertext) <= 8192
    ),
    token_nonce BYTEA,
    resolved_revision TEXT CHECK (
        resolved_revision IS NULL OR octet_length(resolved_revision) BETWEEN 1 AND 4096
    ),
    coalesced_job_id UUID REFERENCES "ChallengeImportJobs" (id) ON DELETE SET NULL,
    status SMALLINT NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 3),
    result JSONB,
    error TEXT CHECK (error IS NULL OR octet_length(error) <= 16384),
    lease_owner UUID,
    lease_expires_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 8),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (clock_timestamp() + INTERVAL '24 hours'),
    UNIQUE (game_id, actor_user_id, operation_id),
    CHECK (
        (source_kind = 0 AND (source_file_id IS NOT NULL OR status IN (2, 3)) AND repo_url IS NULL
            AND token_ciphertext IS NULL AND token_nonce IS NULL)
        OR
        (source_kind = 1 AND source_file_id IS NULL AND repo_url IS NOT NULL
            AND ((token_ciphertext IS NULL AND token_nonce IS NULL)
                OR (token_ciphertext IS NOT NULL AND octet_length(token_nonce) = 12)))
    )
);

CREATE INDEX IF NOT EXISTS ix_challengeimportjobs_claim
    ON "ChallengeImportJobs" (status, lease_expires_at, created_at)
    WHERE coalesced_job_id IS NULL AND status IN (0, 1);
CREATE INDEX IF NOT EXISTS ix_challengeimportjobs_game_active
    ON "ChallengeImportJobs" (game_id, status)
    WHERE status IN (0, 1);
CREATE INDEX IF NOT EXISTS ix_challengeimportjobs_expiry
    ON "ChallengeImportJobs" (expires_at, id);

CREATE OR REPLACE FUNCTION rsctf_release_challenge_import_source()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
    IF OLD.source_file_id IS NOT NULL THEN
        UPDATE "Files"
           SET reference_count = GREATEST(reference_count - 1, 0)
         WHERE id = OLD.source_file_id;
    END IF;
    RETURN OLD;
END
$function$;

DROP TRIGGER IF EXISTS tr_challengeimportjobs_release_source ON "ChallengeImportJobs";
CREATE TRIGGER tr_challengeimportjobs_release_source
    BEFORE DELETE ON "ChallengeImportJobs"
    FOR EACH ROW
    EXECUTE FUNCTION rsctf_release_challenge_import_source();

CREATE TABLE IF NOT EXISTS "ChallengeImportRevisions" (
    game_id INTEGER NOT NULL REFERENCES "Games" (id) ON DELETE CASCADE,
    source_kind SMALLINT NOT NULL CHECK (source_kind IN (0, 1)),
    revision_key TEXT NOT NULL CHECK (octet_length(revision_key) BETWEEN 1 AND 4096),
    owner_job_id UUID NOT NULL REFERENCES "ChallengeImportJobs" (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (game_id, source_kind, revision_key)
);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn jobs_have_atomic_retry_revision_and_bounded_claim_indexes() {
        assert!(UP_SQL.contains("UNIQUE (game_id, actor_user_id, operation_id)"));
        assert!(UP_SQL.contains("PRIMARY KEY (game_id, source_kind, revision_key)"));
        assert!(UP_SQL.contains("WHERE coalesced_job_id IS NULL AND status IN (0, 1)"));
        assert!(UP_SQL.contains("attempts BETWEEN 0 AND 8"));
        assert!(UP_SQL.contains("octet_length(error) <= 16384"));
        assert!(UP_SQL.contains("octet_length(token_nonce) = 12"));
        assert!(UP_SQL.contains("octet_length(token_ciphertext) <= 8192"));
        assert!(UP_SQL.contains("source_file_id IS NOT NULL OR status IN (2, 3)"));
        assert!(UP_SQL.contains("tr_challengeimportjobs_release_source"));
        assert!(UP_SQL.contains("GREATEST(reference_count - 1, 0)"));
        assert!(UP_SQL.contains("ux_gamechallenges_import_source_identity"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn operation_and_revision_identities_are_atomic_across_connections() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("rsctf_m0143_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(4)
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
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();

        let actor = uuid::Uuid::new_v4();
        let operation = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES (7)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(actor)
            .execute(&pool)
            .await
            .unwrap();

        let insert = |id| {
            let pool = pool.clone();
            async move {
                let result = sqlx::query(
                    r#"INSERT INTO "ChallengeImportJobs"
                          (id, game_id, actor_user_id, operation_id, source_kind,
                           import_policy, source_key, repo_url)
                        VALUES ($1, 7, $2, $3, 1, 0, 'same-source',
                                'https://github.com/TCP1P/repo.git')"#,
                )
                .bind(id)
                .bind(actor)
                .bind(operation)
                .execute(&pool)
                .await;
                (id, result)
            }
        };
        let first_id = uuid::Uuid::new_v4();
        let (first, second) = tokio::join!(insert(first_id), insert(uuid::Uuid::new_v4()));
        assert_ne!(
            first.1.is_ok(),
            second.1.is_ok(),
            "exactly one operation insert wins"
        );
        let owner_id = if first.1.is_ok() { first.0 } else { second.0 };

        sqlx::query(
            r#"INSERT INTO "GameChallenges" (id, game_id, import_source_identity)
                VALUES (1, 7, 'import/same')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert!(sqlx::query(
            r#"INSERT INTO "GameChallenges" (id, game_id, import_source_identity)
                VALUES (2, 7, 'import/same')"#,
        )
        .execute(&pool)
        .await
        .is_err());

        let revision_insert = |owner| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    r#"INSERT INTO "ChallengeImportRevisions"
                          (game_id, source_kind, revision_key, owner_job_id)
                        VALUES (7, 1, 'same-commit', $1)
                        ON CONFLICT (game_id, source_kind, revision_key) DO NOTHING"#,
                )
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap()
                .rows_affected()
            }
        };
        let (first_revision, second_revision) =
            tokio::join!(revision_insert(owner_id), revision_insert(owner_id));
        assert_eq!(first_revision + second_revision, 1);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
