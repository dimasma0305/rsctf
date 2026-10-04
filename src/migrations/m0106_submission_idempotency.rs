//! Durable idempotency reservations for normal Jeopardy flag submissions.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "SubmissionAttempts" (
    participation_id   INTEGER NOT NULL,
    challenge_id       INTEGER NOT NULL,
    attempt_id         UUID NOT NULL,
    request_fingerprint BYTEA NOT NULL,
    submission_id      INTEGER NULL,
    created_at_utc     TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (participation_id, challenge_id, attempt_id),
    CONSTRAINT ck_submission_attempt_fingerprint
        CHECK (OCTET_LENGTH(request_fingerprint) = 32),
    CONSTRAINT fk_submission_attempt_participation
        FOREIGN KEY (participation_id) REFERENCES "Participations"(id) ON DELETE CASCADE,
    CONSTRAINT fk_submission_attempt_challenge
        FOREIGN KEY (challenge_id) REFERENCES "GameChallenges"(id) ON DELETE CASCADE,
    CONSTRAINT fk_submission_attempt_submission
        FOREIGN KEY (submission_id, participation_id, challenge_id)
        REFERENCES "Submissions"(id, participation_id, challenge_id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_submission_attempt_submission
    ON "SubmissionAttempts"(submission_id)
    WHERE submission_id IS NOT NULL;
"#;

const DOWN_SQL: &str = r#"
DROP TABLE IF EXISTS "SubmissionAttempts";
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
    fn attempt_reservations_are_unique_fingerprinted_and_bound_to_one_submission() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"SubmissionAttempts\""));
        assert!(UP_SQL.contains("PRIMARY KEY (participation_id, challenge_id, attempt_id)"));
        assert!(UP_SQL.contains("OCTET_LENGTH(request_fingerprint) = 32"));
        assert!(UP_SQL.contains("FOREIGN KEY (submission_id, participation_id, challenge_id)"));
        assert!(
            UP_SQL.contains("CREATE UNIQUE INDEX IF NOT EXISTS ux_submission_attempt_submission")
        );
        assert!(UP_SQL.contains("WHERE submission_id IS NOT NULL"));
        assert!(!UP_SQL.contains("answer TEXT"));
        assert!(!UP_SQL.contains("proof TEXT"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn migration_is_reentrant_and_enforces_the_submission_tuple() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_m0106_{}", uuid::Uuid::new_v4().simple());
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
            r#"
            CREATE TABLE "Participations" (id INTEGER PRIMARY KEY);
            CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY);
            CREATE TABLE "Submissions" (
              id INTEGER PRIMARY KEY,
              participation_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL,
              UNIQUE (id, participation_id, challenge_id)
            );
            INSERT INTO "Participations" VALUES (1), (2);
            INSERT INTO "GameChallenges" VALUES (10), (20);
            INSERT INTO "Submissions" VALUES (100, 1, 10), (200, 2, 20);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(UP_SQL).execute(&pool).await.unwrap();
        sqlx::raw_sql(UP_SQL)
            .execute(&pool)
            .await
            .expect("the forward migration must be idempotent");
        let attempt = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "SubmissionAttempts"
                 (participation_id, challenge_id, attempt_id, request_fingerprint, submission_id)
               VALUES (1, 10, $1, decode(repeat('ab', 32), 'hex'), 100)"#,
        )
        .bind(attempt)
        .execute(&pool)
        .await
        .unwrap();

        let duplicate = sqlx::query(
            r#"INSERT INTO "SubmissionAttempts"
                 (participation_id, challenge_id, attempt_id, request_fingerprint)
               VALUES (1, 10, $1, decode(repeat('cd', 32), 'hex'))"#,
        )
        .bind(attempt)
        .execute(&pool)
        .await
        .expect_err("the participation/challenge/attempt tuple must be unique");
        assert_eq!(
            duplicate.as_database_error().and_then(|error| error.code()),
            Some(std::borrow::Cow::Borrowed("23505"))
        );

        let wrong_tuple = sqlx::query(
            r#"UPDATE "SubmissionAttempts" SET submission_id = 200
                WHERE participation_id = 1 AND challenge_id = 10 AND attempt_id = $1"#,
        )
        .bind(attempt)
        .execute(&pool)
        .await
        .expect_err("a reservation cannot point at another submission tuple");
        assert_eq!(
            wrong_tuple
                .as_database_error()
                .and_then(|error| error.code()),
            Some(std::borrow::Cow::Borrowed("23503"))
        );

        sqlx::query(r#"DELETE FROM "Submissions" WHERE id = 100"#)
            .execute(&pool)
            .await
            .expect("deleting an owned submission must cascade its retry metadata");
        let remaining: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "SubmissionAttempts""#)
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
