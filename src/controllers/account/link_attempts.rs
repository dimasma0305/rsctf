use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::utils::codec::sha256_str;
use crate::utils::error::{AppError, AppResult};

const SUCCESS: i16 = 1;
const SUPERSEDED: i16 = 2;
const MAX_RETAINED_PER_PURPOSE: i64 = 16;

pub(super) struct LockedLink {
    pub account_id: Uuid,
    pub security_generation_digest: String,
    pub safe_result: Option<String>,
}

pub(super) fn token_digest(token: &str) -> String {
    sha256_str(token)
}

pub(super) fn value_digest(value: &str) -> String {
    sha256_str(value)
}

pub(super) async fn stage_registration(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    account_id: Uuid,
    security_generation_digest: &str,
    destination_digest: &str,
    expires_at: DateTime<Utc>,
) -> AppResult<()> {
    let digest = token_digest(token);
    sqlx::query(
        r#"INSERT INTO "AccountLinkAttempts"
                  (token_digest, purpose, account_id,
                   security_generation_digest, destination_digest, expires_at_utc, active)
           VALUES ($1, 'registration', $2, $3, $4, $5, FALSE)
           ON CONFLICT (token_digest) DO NOTHING"#,
    )
    .bind(&digest)
    .bind(account_id)
    .bind(security_generation_digest)
    .bind(destination_digest)
    .bind(expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(super) async fn lock_registration(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    account_id: Uuid,
    security_generation_digest: &str,
    destination_digest: &str,
) -> AppResult<LockedLink> {
    let digest = token_digest(token);
    lock_exact(
        transaction,
        &digest,
        "registration",
        account_id,
        security_generation_digest,
        destination_digest,
    )
    .await
}

pub(super) async fn activate_registration_locked(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    account_id: Uuid,
) -> AppResult<()> {
    activate_staged_locked(transaction, token, account_id, "registration").await
}

pub(super) async fn stage_email_change(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    account_id: Uuid,
    security_stamp: &str,
    normalized_email: &str,
    expires_at: DateTime<Utc>,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "AccountLinkAttempts"
                  (token_digest, purpose, account_id,
                   security_generation_digest, destination_digest,
                   expires_at_utc, active)
           VALUES ($1, 'email_change', $2, $3, $4, $5, FALSE)"#,
    )
    .bind(token_digest(token))
    .bind(account_id)
    .bind(value_digest(security_stamp))
    .bind(value_digest(normalized_email))
    .bind(expires_at)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

pub(super) async fn activate_email_change_locked(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    account_id: Uuid,
) -> AppResult<()> {
    activate_staged_locked(transaction, token, account_id, "email_change").await
}

async fn activate_staged_locked(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    account_id: Uuid,
    purpose: &'static str,
) -> AppResult<()> {
    let digest = token_digest(token);
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        transaction,
        &format!("account-link:{account_id}:{purpose}"),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let target = sqlx::query_as::<_, (i64, bool, Option<i16>)>(
        r#"SELECT issued_sequence, expires_at_utc > clock_timestamp(), terminal_result
             FROM "AccountLinkAttempts"
            WHERE token_digest = $1 AND account_id = $2 AND purpose = $3
            FOR UPDATE"#,
    )
    .bind(&digest)
    .bind(account_id)
    .bind(purpose)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::bad_request("Invalid or expired account link"))?;
    if target.2 == Some(SUCCESS) || target.2 == Some(SUPERSEDED) {
        return Ok(());
    }
    if !target.1 {
        return Err(AppError::bad_request("Invalid or expired account link"));
    }
    let newer_active: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "AccountLinkAttempts"
                WHERE account_id = $1 AND purpose = $2
                  AND token_digest <> $3 AND active = TRUE
                  AND terminal_result IS NULL AND issued_sequence > $4
           )"#,
    )
    .bind(account_id)
    .bind(purpose)
    .bind(&digest)
    .bind(target.0)
    .fetch_one(&mut **transaction)
    .await
    .map_err(database_error)?;
    if newer_active {
        sqlx::query(
            r#"UPDATE "AccountLinkAttempts"
                  SET terminal_result = $2, completed_at_utc = clock_timestamp()
                WHERE token_digest = $1 AND terminal_result IS NULL"#,
        )
        .bind(&digest)
        .bind(SUPERSEDED)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
        retain_bounded(transaction, account_id, purpose).await?;
        return Ok(());
    }
    sqlx::query(
        r#"UPDATE "AccountLinkAttempts"
              SET active = TRUE
            WHERE token_digest = $1 AND account_id = $2
              AND purpose = $3 AND terminal_result IS NULL"#,
    )
    .bind(&digest)
    .bind(account_id)
    .bind(purpose)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "AccountLinkAttempts"
              SET active = FALSE, terminal_result = $3,
                  completed_at_utc = clock_timestamp()
            WHERE account_id = $1 AND purpose = $4
              AND token_digest <> $2 AND active = TRUE
              AND terminal_result IS NULL"#,
    )
    .bind(account_id)
    .bind(&digest)
    .bind(SUPERSEDED)
    .bind(purpose)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    retain_bounded(transaction, account_id, purpose).await?;
    Ok(())
}

pub(super) async fn lock_email_change(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    normalized_email: &str,
) -> AppResult<LockedLink> {
    let digest = token_digest(token);
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            bool,
            Option<i16>,
            Option<String>,
            bool,
        ),
    >(
        r#"SELECT account_id, security_generation_digest, destination_digest,
                  active, terminal_result, safe_result,
                  expires_at_utc > clock_timestamp()
             FROM "AccountLinkAttempts"
            WHERE token_digest = $1 AND purpose = 'email_change'
            FOR UPDATE"#,
    )
    .bind(&digest)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(invalid_email_change)?;
    if row.2 != value_digest(normalized_email) {
        return Err(invalid_email_change());
    }
    if row.4 == Some(SUCCESS) {
        return Ok(LockedLink {
            account_id: row.0,
            security_generation_digest: row.1,
            safe_result: row.5,
        });
    }
    if !row.3 || row.4.is_some() || !row.6 {
        return Err(invalid_email_change());
    }
    Ok(LockedLink {
        account_id: row.0,
        security_generation_digest: row.1,
        safe_result: None,
    })
}

pub(super) async fn complete(
    transaction: &mut Transaction<'_, Postgres>,
    token: &str,
    account_id: Uuid,
    purpose: &'static str,
    safe_result: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "AccountLinkAttempts"
              SET active = FALSE, terminal_result = $3, safe_result = $4,
                  completed_at_utc = clock_timestamp()
            WHERE token_digest = $1 AND account_id = $2
              AND terminal_result IS NULL"#,
    )
    .bind(token_digest(token))
    .bind(account_id)
    .bind(SUCCESS)
    .bind(safe_result)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    retain_bounded(transaction, account_id, purpose).await?;
    Ok(())
}

async fn lock_exact(
    transaction: &mut Transaction<'_, Postgres>,
    token_digest: &str,
    purpose: &'static str,
    account_id: Uuid,
    generation_digest: &str,
    destination_digest: &str,
) -> AppResult<LockedLink> {
    let row = sqlx::query_as::<_, (Uuid, String, String, Option<i16>, Option<String>, bool)>(
        r#"SELECT account_id, security_generation_digest, destination_digest,
                  terminal_result, safe_result,
                  active AND expires_at_utc > clock_timestamp()
             FROM "AccountLinkAttempts"
            WHERE token_digest = $1 AND purpose = $2
            FOR UPDATE"#,
    )
    .bind(token_digest)
    .bind(purpose)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::bad_request("Invalid or expired account link"))?;
    if row.0 != account_id || row.1 != generation_digest || row.2 != destination_digest {
        return Err(AppError::bad_request("Invalid or expired account link"));
    }
    if row.3 == Some(SUCCESS) {
        return Ok(LockedLink {
            account_id,
            security_generation_digest: row.1,
            safe_result: row.4,
        });
    }
    if row.3.is_some() || !row.5 {
        return Err(AppError::bad_request("Invalid or expired account link"));
    }
    Ok(LockedLink {
        account_id,
        security_generation_digest: row.1,
        safe_result: None,
    })
}

async fn retain_bounded(
    transaction: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    purpose: &'static str,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "AccountLinkAttempts" old
            WHERE old.account_id = $1 AND old.purpose = $2
              AND (old.expires_at_utc < clock_timestamp() - INTERVAL '7 days'
               OR old.ctid IN (
                    SELECT ctid FROM "AccountLinkAttempts"
                     WHERE account_id = $1 AND purpose = $2
                     ORDER BY issued_sequence DESC
                    OFFSET $3
              ))"#,
    )
    .bind(account_id)
    .bind(purpose)
    .bind(MAX_RETAINED_PER_PURPOSE)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn invalid_email_change() -> AppError {
    AppError::bad_request("Invalid or expired email-change token")
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn retention_never_deletes_another_accounts_expired_links() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("account_link_retention_{}", Uuid::new_v4().simple());
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
        sqlx::query(
            r#"CREATE TABLE "AccountLinkAttempts" (
                 token_digest TEXT PRIMARY KEY,
                 purpose TEXT NOT NULL,
                 account_id UUID NOT NULL,
                 expires_at_utc TIMESTAMPTZ NOT NULL,
                 issued_sequence BIGSERIAL NOT NULL
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let owner = Uuid::new_v4();
        let unrelated = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "AccountLinkAttempts"
                 (token_digest, purpose, account_id, expires_at_utc)
               VALUES ('owner-expired', 'registration', $1,
                         clock_timestamp() - INTERVAL '8 days'),
                      ('owner-other-purpose', 'email_change', $1,
                         clock_timestamp() - INTERVAL '8 days'),
                      ('other-expired', 'registration', $2,
                         clock_timestamp() - INTERVAL '8 days')"#,
        )
        .bind(owner)
        .bind(unrelated)
        .execute(&pool)
        .await
        .unwrap();

        let mut transaction = pool.begin().await.unwrap();
        retain_bounded(&mut transaction, owner, "registration")
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let remaining: Vec<String> = sqlx::query_scalar(
            r#"SELECT token_digest FROM "AccountLinkAttempts" ORDER BY token_digest"#,
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(remaining, vec!["other-expired", "owner-other-purpose"]);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
