//! Durable, digest-only ownership and replay state for emailed account links.

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::utils::error::{AppError, AppResult};

const CLEANUP_BATCH: i64 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub(super) enum Purpose {
    Registration = 0,
    EmailChange = 1,
}

#[derive(Debug)]
pub(super) struct PendingAttempt {
    pub token_digest: [u8; 32],
    pub account_id: Uuid,
    pub security_generation_digest: [u8; 32],
    pub destination_digest: [u8; 32],
}

#[derive(Debug)]
pub(super) enum Claim {
    Pending(PendingAttempt),
    Completed(JsonValue),
}

pub(super) fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

async fn lock_generation(
    transaction: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    purpose: Purpose,
) -> AppResult<()> {
    sqlx::query(
        r#"SELECT pg_advisory_xact_lock(
               hashtextextended($1::text || ':' || $2::text, 194207)
           )"#,
    )
    .bind(account_id)
    .bind(purpose as i16)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"DELETE FROM "AccountLinkAttempts" WHERE token_digest IN (
               SELECT token_digest FROM "AccountLinkAttempts"
                WHERE account_id = $1 AND purpose = $2 AND NOT is_current
                ORDER BY created_at_utc DESC
                OFFSET 31 LIMIT $3
           )"#,
    )
    .bind(account_id)
    .bind(purpose as i16)
    .bind(CLEANUP_BATCH)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

/// Persist a candidate before delivery. A resend is deliberately not current
/// yet, so failed SMTP delivery cannot invalidate the last delivered link.
pub(super) async fn stage(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    purpose: Purpose,
    account_id: Uuid,
    security_generation: &str,
    destination: &str,
    expires_at_unix: i64,
    activate_immediately: bool,
) -> AppResult<()> {
    lock_generation(transaction, account_id, purpose).await?;
    sqlx::query(
        r#"DELETE FROM "AccountLinkAttempts" WHERE token_digest IN (
               SELECT token_digest FROM "AccountLinkAttempts"
                WHERE account_id = $1 AND purpose = $2
                  AND expires_at_utc < clock_timestamp() - interval '7 days'
                ORDER BY expires_at_utc
                LIMIT $3
           )"#,
    )
    .bind(account_id)
    .bind(purpose as i16)
    .bind(CLEANUP_BATCH)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if activate_immediately {
        sqlx::query(
            r#"UPDATE "AccountLinkAttempts" SET is_current = FALSE
                WHERE account_id = $1 AND purpose = $2 AND is_current"#,
        )
        .bind(account_id)
        .bind(purpose as i16)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    }
    let generation: i64 = sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(generation), 0) + 1
             FROM "AccountLinkAttempts"
            WHERE account_id = $1 AND purpose = $2"#,
    )
    .bind(account_id)
    .bind(purpose as i16)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"INSERT INTO "AccountLinkAttempts"
             (token_digest, purpose, account_id, generation,
              security_generation_digest, destination_digest, expires_at_utc,
              is_current, delivered_at_utc)
           VALUES ($1,$2,$3,$4,$5,$6,to_timestamp($7),$8,
                   CASE WHEN $8 THEN clock_timestamp() ELSE NULL END)"#,
    )
    .bind(digest(token.as_bytes()).to_vec())
    .bind(purpose as i16)
    .bind(account_id)
    .bind(generation)
    .bind(digest(security_generation.as_bytes()).to_vec())
    .bind(digest(destination.as_bytes()).to_vec())
    .bind(expires_at_unix)
    .bind(activate_immediately)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

/// Publish a successfully delivered staged link and supersede the previous
/// generation atomically. An exact retry is a no-op.
pub(super) async fn activate(
    pool: &sqlx::PgPool,
    token: &str,
    purpose: Purpose,
    account_id: Uuid,
) -> AppResult<()> {
    let token_digest = digest(token.as_bytes());
    let mut transaction = pool.begin().await.map_err(database_error)?;
    lock_generation(&mut transaction, account_id, purpose).await?;
    let exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "AccountLinkAttempts"
                WHERE token_digest = $1 AND account_id = $2 AND purpose = $3
                  AND consumed_at_utc IS NULL
           )"#,
    )
    .bind(token_digest.to_vec())
    .bind(account_id)
    .bind(purpose as i16)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if !exists {
        return Err(AppError::bad_request("Invalid or expired account link"));
    }
    sqlx::query(
        r#"UPDATE "AccountLinkAttempts" SET is_current = FALSE
            WHERE account_id = $1 AND purpose = $2 AND is_current
              AND token_digest <> $3"#,
    )
    .bind(account_id)
    .bind(purpose as i16)
    .bind(token_digest.to_vec())
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "AccountLinkAttempts"
              SET is_current = TRUE,
                  delivered_at_utc = COALESCE(delivered_at_utc, clock_timestamp())
            WHERE token_digest = $1 AND account_id = $2 AND purpose = $3
              AND consumed_at_utc IS NULL"#,
    )
    .bind(token_digest.to_vec())
    .bind(account_id)
    .bind(purpose as i16)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    transaction.commit().await.map_err(database_error)?;
    Ok(())
}

/// Lock a link attempt. A completed exact replay is returned before the
/// current/expiry checks because consuming the link intentionally clears it.
pub(super) async fn claim(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    purpose: Purpose,
) -> AppResult<Claim> {
    let token_digest = digest(token.as_bytes());
    let row: Option<(Uuid, Vec<u8>, Vec<u8>, bool, bool, Option<JsonValue>)> = sqlx::query_as(
        r#"SELECT account_id, security_generation_digest, destination_digest,
                  is_current, expires_at_utc > clock_timestamp(), result
             FROM "AccountLinkAttempts"
            WHERE token_digest = $1 AND purpose = $2
            FOR UPDATE"#,
    )
    .bind(token_digest.to_vec())
    .bind(purpose as i16)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?;
    let Some((account_id, security_digest, destination_digest, current, live, result)) = row else {
        return Err(AppError::bad_request("Invalid or expired account link"));
    };
    if let Some(result) = result {
        return Ok(Claim::Completed(result));
    }
    if !current || !live {
        return Err(AppError::bad_request("Invalid or expired account link"));
    }
    let security_generation_digest = security_digest
        .try_into()
        .map_err(|_| AppError::internal("invalid account-link security digest"))?;
    let destination_digest = destination_digest
        .try_into()
        .map_err(|_| AppError::internal("invalid account-link destination digest"))?;
    Ok(Claim::Pending(PendingAttempt {
        token_digest,
        account_id,
        security_generation_digest,
        destination_digest,
    }))
}

pub(super) async fn complete(
    transaction: &mut Transaction<'_, Postgres>,
    attempt: &PendingAttempt,
    result: &JsonValue,
) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "AccountLinkAttempts"
              SET consumed_at_utc = clock_timestamp(), result = $2, is_current = FALSE
            WHERE token_digest = $1 AND consumed_at_utc IS NULL AND result IS NULL"#,
    )
    .bind(attempt.token_digest.to_vec())
    .bind(result)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("Account link was consumed concurrently"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn digests_are_stable_and_domain_values_do_not_overlap() {
        assert_eq!(digest(b"one"), digest(b"one"));
        assert_ne!(digest(b"one"), digest(b"two"));
        assert_ne!(Purpose::Registration as i16, Purpose::EmailChange as i16);
    }

    #[test]
    fn plaintext_tokens_are_never_part_of_the_sql_contract() {
        let source = include_str!("link_attempts.rs");
        assert!(source.contains("token_digest"));
        assert!(!source.contains("INSERT INTO \"AccountLinkAttempts\" (token,"));
    }

    #[test]
    fn request_path_cleanup_is_account_scoped_and_batch_bounded() {
        let source = include_str!("link_attempts.rs");
        assert!(source.contains("WHERE account_id = $1 AND purpose = $2"));
        assert!(source.contains("LIMIT $3"));
        assert!(!source.contains("DELETE FROM \"AccountLinkAttempts\" WHERE expires_at_utc"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn failed_delivery_preserves_current_and_exact_consumption_replays() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("account_links_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(3)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "AccountLinkAttempts" (
                 token_digest BYTEA PRIMARY KEY,
                 purpose SMALLINT NOT NULL,
                 account_id UUID NOT NULL REFERENCES "AspNetUsers"(id) ON DELETE CASCADE,
                 generation BIGINT NOT NULL,
                 security_generation_digest BYTEA NOT NULL,
                 destination_digest BYTEA NOT NULL,
                 expires_at_utc TIMESTAMPTZ NOT NULL,
                 is_current BOOLEAN NOT NULL DEFAULT FALSE,
                 delivered_at_utc TIMESTAMPTZ,
                 consumed_at_utc TIMESTAMPTZ,
                 result JSONB,
                 created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 UNIQUE (account_id, purpose, generation)
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let account_id = Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(account_id)
            .execute(&pool)
            .await
            .unwrap();
        let expiry = chrono::Utc::now().timestamp() + 3600;

        let mut first = pool.begin().await.unwrap();
        stage(
            &mut first,
            "first",
            Purpose::EmailChange,
            account_id,
            "stamp",
            "FIRST@EXAMPLE.TEST",
            expiry,
            true,
        )
        .await
        .unwrap();
        first.commit().await.unwrap();

        let mut undelivered = pool.begin().await.unwrap();
        stage(
            &mut undelivered,
            "undelivered",
            Purpose::EmailChange,
            account_id,
            "stamp",
            "SECOND@EXAMPLE.TEST",
            expiry,
            false,
        )
        .await
        .unwrap();
        undelivered.commit().await.unwrap();
        let current: Vec<Vec<u8>> = sqlx::query_scalar(
            r#"SELECT token_digest FROM "AccountLinkAttempts" WHERE is_current"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(current, vec![digest(b"first").to_vec()]);

        activate(&pool, "undelivered", Purpose::EmailChange, account_id)
            .await
            .unwrap();
        let mut consume = pool.begin().await.unwrap();
        let attempt = match claim(&mut consume, "undelivered", Purpose::EmailChange)
            .await
            .unwrap()
        {
            Claim::Pending(attempt) => attempt,
            Claim::Completed(_) => panic!("fresh link was already complete"),
        };
        complete(
            &mut consume,
            &attempt,
            &serde_json::json!({ "status": "emailChanged" }),
        )
        .await
        .unwrap();
        consume.commit().await.unwrap();

        let mut replay = pool.begin().await.unwrap();
        assert!(matches!(
            claim(&mut replay, "undelivered", Purpose::EmailChange)
                .await
                .unwrap(),
            Claim::Completed(_)
        ));
        replay.commit().await.unwrap();

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
