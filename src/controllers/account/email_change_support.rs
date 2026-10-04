//! Shared ticket publication and serialized identity helpers for email changes.

use super::*;
use sha2::{Digest, Sha256};

pub(super) const ACCOUNT_LINK_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Cache publication paired with a durable mail intent. The database outbox
/// serializes replacements; this snapshot lets a failed commit restore the
/// prior usable link without overwriting a newer cache generation.
#[cfg(test)]
pub(super) struct TicketPublication {
    current_key: String,
    ticket_prefix: &'static str,
    token: Vec<u8>,
    ticket: Vec<u8>,
    previous: Option<(Vec<u8>, Vec<u8>)>,
}

#[cfg(test)]
pub(super) async fn publish_ticket(
    cache: &dyn crate::services::cache::Cache,
    current_key: String,
    ticket_prefix: &'static str,
    token: &[u8],
    ticket: &[u8],
) -> TicketPublication {
    let previous = if let Some(previous_token) = cache.get(&current_key).await {
        if cache
            .compare_and_remove(&current_key, previous_token.as_ref())
            .await
        {
            let previous_key = std::str::from_utf8(&previous_token)
                .ok()
                .map(|token| format!("{ticket_prefix}{token}"));
            if let Some(previous_key) = previous_key {
                if let Some(previous_ticket) = cache.get(&previous_key).await {
                    if cache
                        .compare_and_remove(&previous_key, previous_ticket.as_ref())
                        .await
                    {
                        Some((previous_token.to_vec(), previous_ticket.to_vec()))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let token_text = std::str::from_utf8(token).expect("account tokens are ASCII");
    cache
        .set(
            &format!("{ticket_prefix}{token_text}"),
            ticket,
            Some(ACCOUNT_LINK_TTL),
        )
        .await;
    cache.set(&current_key, token, Some(ACCOUNT_LINK_TTL)).await;
    TicketPublication {
        current_key,
        ticket_prefix,
        token: token.to_vec(),
        ticket: ticket.to_vec(),
        previous,
    }
}

#[cfg(test)]
pub(super) async fn rollback_ticket_publication(
    cache: &dyn crate::services::cache::Cache,
    publication: TicketPublication,
) {
    if !cache
        .compare_and_remove(&publication.current_key, &publication.token)
        .await
    {
        return;
    }
    let token = std::str::from_utf8(&publication.token).expect("account tokens are ASCII");
    cache
        .compare_and_remove(
            &format!("{}{token}", publication.ticket_prefix),
            &publication.ticket,
        )
        .await;
    let Some((previous_token, previous_ticket)) = publication.previous else {
        return;
    };
    let Ok(previous_token_text) = std::str::from_utf8(&previous_token) else {
        return;
    };
    let restored = cache
        .set_if_absent(
            &format!("{}{previous_token_text}", publication.ticket_prefix),
            &previous_ticket,
            Some(ACCOUNT_LINK_TTL),
        )
        .await;
    if restored {
        cache
            .set_if_absent(
                &publication.current_key,
                &previous_token,
                Some(ACCOUNT_LINK_TTL),
            )
            .await;
    }
}

pub(super) async fn email_confirmation_required(st: &SharedState) -> AppResult<bool> {
    Ok(
        config::Entity::find_by_id("AccountPolicy:EmailConfirmationRequired".to_string())
            .one(&st.db)
            .await?
            .and_then(|row| row.value)
            .map(|value| value == "true")
            .unwrap_or(st.config.account.email_confirmation_required),
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EmailUpdateOutcome {
    Updated,
    Conflict,
    StampMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmailUpdateMode {
    Immediate,
    ConfirmedTicket,
}

pub(super) struct EmailUpdateRequest<'a> {
    pub(super) user_id: Uuid,
    pub(super) expected_stamp: &'a str,
    pub(super) email: &'a str,
    pub(super) normalized_email: &'a str,
    pub(super) new_stamp: String,
    pub(super) mode: EmailUpdateMode,
}

pub(super) struct ConfirmedEmailChange {
    pub(super) user_id: Uuid,
    pub(super) user_name: String,
}

/// Consume a durable email-change ticket and rotate the account identity in one
/// short registration-serialized transaction. `None` is reserved for a legacy
/// cache-only ticket created before this migration; callers may temporarily
/// fall back to the compatibility path for those links.
pub(super) async fn confirm_email_change_ticket(
    pool: &sqlx::PgPool,
    config: &crate::models::internal::configs::AppConfig,
    token: &str,
    supplied_email: &str,
) -> AppResult<Option<ConfirmedEmailChange>> {
    let token_hash = Sha256::digest(token.as_bytes()).to_vec();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let policy =
        crate::services::anti_cheat::lock_and_load_account_policy(&mut transaction, config).await?;
    if !verify_email_domain(supplied_email, &policy.email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let ticket = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            String,
            Uuid,
            Option<chrono::DateTime<Utc>>,
        ),
    >(
        r#"SELECT ticket.account_id, ticket.security_stamp, ticket.new_email,
                  ticket.normalized_email, ticket.operation_id, ticket.consumed_at_utc
             FROM "EmailChangeTickets" ticket
             JOIN "MailOutbox" outbox ON outbox.operation_id = ticket.operation_id
            WHERE ticket.token_hash = $1
              AND (ticket.consumed_at_utc IS NOT NULL
                   OR ticket.expires_at_utc > clock_timestamp())
              AND ticket.superseded_at_utc IS NULL
              AND outbox.purpose = $2
              AND (
                    (ticket.consumed_at_utc IS NULL
                     AND outbox.superseded_at_utc IS NULL)
                    OR (ticket.consumed_at_utc IS NOT NULL
                        AND outbox.consumed_at_utc IS NOT NULL)
                  )
            FOR UPDATE OF ticket"#,
    )
    .bind(&token_hash)
    .bind(crate::services::mail_outbox::MailPurpose::EmailChange as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((account_id, security_stamp, new_email, normalized_email, operation_id, consumed_at)) =
        ticket
    else {
        let durable_exists: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "EmailChangeTickets" WHERE token_hash = $1
               )"#,
        )
        .bind(&token_hash)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if durable_exists {
            return Err(AppError::bad_request(
                "Invalid or expired email-change token",
            ));
        }
        return Ok(None);
    };
    if supplied_email != new_email {
        return Err(AppError::bad_request("Invalid email"));
    }
    if consumed_at.is_some() {
        let user_name = sqlx::query_scalar::<_, String>(
            r#"SELECT COALESCE(user_name, '') FROM "AspNetUsers"
                WHERE id = $1"#,
        )
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(Some(ConfirmedEmailChange {
            user_id: account_id,
            user_name,
        }));
    }
    let collision: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "AspNetUsers"
                WHERE normalized_email = $1 AND id <> $2
           )"#,
    )
    .bind(&normalized_email)
    .bind(account_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if collision {
        return Err(AppError::conflict("Email already registered"));
    }
    let new_stamp = Uuid::new_v4().to_string();
    let user_name: Option<String> = sqlx::query_scalar(
        r#"UPDATE "AspNetUsers"
              SET email = $1, normalized_email = $2, email_confirmed = TRUE,
                  security_stamp = $3
            WHERE id = $4 AND security_stamp = $5
              AND email_confirmed = TRUE AND role <> $6
        RETURNING COALESCE(user_name, '')"#,
    )
    .bind(&new_email)
    .bind(&normalized_email)
    .bind(new_stamp)
    .bind(account_id)
    .bind(&security_stamp)
    .bind(Role::Banned as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(user_name) = user_name else {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    };
    let consumed = sqlx::query(
        r#"UPDATE "EmailChangeTickets"
              SET consumed_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND token_hash = $2
              AND superseded_at_utc IS NULL AND consumed_at_utc IS NULL"#,
    )
    .bind(operation_id)
    .bind(&token_hash)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if consumed.rows_affected() != 1 {
        return Err(AppError::conflict(
            "Email-change ticket was consumed concurrently",
        ));
    }
    // Stop a still-pending message after its link was already consumed. A
    // delivery already inside SMTP remains harmless because the ticket is now
    // terminal and cannot change the account a second time.
    sqlx::query(
        r#"UPDATE "MailOutbox"
              SET superseded_at_utc = COALESCE(superseded_at_utc, clock_timestamp()),
                  consumed_at_utc = COALESCE(consumed_at_utc, clock_timestamp()),
                  dead_at_utc = CASE
                      WHEN delivered_at_utc IS NULL AND lease_token IS NULL
                      THEN clock_timestamp() ELSE dead_at_utc END,
                  last_error = CASE
                      WHEN delivered_at_utc IS NULL AND lease_token IS NULL
                      THEN 'consumed' ELSE last_error END,
                  html_body = CASE
                      WHEN delivered_at_utc IS NULL AND lease_token IS NULL
                      THEN '' ELSE html_body END
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(Some(ConfirmedEmailChange {
        user_id: account_id,
        user_name,
    }))
}

/// Commit an email identity change under the same cross-replica lock used by
/// password registration, OAuth provisioning, and admin identity writers.
pub(super) async fn update_email_serialized(
    pool: &sqlx::PgPool,
    config: &crate::models::internal::configs::AppConfig,
    request: EmailUpdateRequest<'_>,
) -> AppResult<EmailUpdateOutcome> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let policy =
        crate::services::anti_cheat::lock_and_load_account_policy(&mut transaction, config).await?;
    if !verify_email_domain(request.email, &policy.email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    if request.mode == EmailUpdateMode::Immediate && policy.email_confirmation_required {
        return Err(AppError::bad_request(
            "Email confirmation policy changed; retry the email change",
        ));
    }

    let collision: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "AspNetUsers"
                WHERE normalized_email = $1 AND id <> $2
           )"#,
    )
    .bind(request.normalized_email)
    .bind(request.user_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if collision {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(EmailUpdateOutcome::Conflict);
    }

    let result = sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email = $1,
                  normalized_email = $2,
                  email_confirmed = TRUE,
                  security_stamp = $3
            WHERE id = $4
              AND security_stamp = $5
              AND email_confirmed = TRUE
              AND role <> $6"#,
    )
    .bind(request.email)
    .bind(request.normalized_email)
    .bind(request.new_stamp)
    .bind(request.user_id)
    .bind(request.expected_stamp)
    .bind(Role::Banned as i16)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if result.rows_affected() != 1 {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(EmailUpdateOutcome::StampMismatch);
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(EmailUpdateOutcome::Updated)
}

#[cfg(test)]
mod terminal_replay_tests {
    use std::str::FromStr;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn two_tabs_receive_the_same_terminal_email_change_result() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("email_change_replay_{}", Uuid::new_v4().simple());
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
            r#"CREATE TABLE "Configs" (
                 config_key TEXT PRIMARY KEY, value TEXT
               );
               CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY, user_name TEXT, email TEXT,
                 normalized_email TEXT UNIQUE, email_confirmed BOOLEAN NOT NULL,
                 role SMALLINT NOT NULL, security_stamp TEXT NOT NULL
               );
               CREATE TABLE "MailOutbox" (
                 operation_id UUID PRIMARY KEY, account_id UUID NOT NULL,
                 purpose SMALLINT NOT NULL, superseded_at_utc TIMESTAMPTZ,
                 consumed_at_utc TIMESTAMPTZ, delivered_at_utc TIMESTAMPTZ,
                 dead_at_utc TIMESTAMPTZ, lease_token UUID, last_error TEXT,
                 html_body TEXT NOT NULL
               );
               CREATE TABLE "EmailChangeTickets" (
                 operation_id UUID PRIMARY KEY, token_hash BYTEA UNIQUE NOT NULL,
                 account_id UUID NOT NULL, security_stamp TEXT NOT NULL,
                 new_email TEXT NOT NULL, normalized_email TEXT NOT NULL,
                 expires_at_utc TIMESTAMPTZ NOT NULL,
                 superseded_at_utc TIMESTAMPTZ, consumed_at_utc TIMESTAMPTZ
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let account_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let token = "terminal-email-change-token";
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id,user_name,email,normalized_email,email_confirmed,role,security_stamp)
               VALUES ($1,'player','old@example.test','OLD@EXAMPLE.TEST',TRUE,$2,'stamp-a')"#,
        )
        .bind(account_id)
        .bind(Role::User as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "MailOutbox"
                 (operation_id,account_id,purpose,html_body)
               VALUES ($1,$2,$3,'mail')"#,
        )
        .bind(operation_id)
        .bind(account_id)
        .bind(crate::services::mail_outbox::MailPurpose::EmailChange as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "EmailChangeTickets"
                 (operation_id,token_hash,account_id,security_stamp,new_email,
                  normalized_email,expires_at_utc)
               VALUES ($1,$2,$3,'stamp-a','new@example.test','NEW@EXAMPLE.TEST',
                       clock_timestamp() + INTERVAL '15 minutes')"#,
        )
        .bind(operation_id)
        .bind(Sha256::digest(token.as_bytes()).to_vec())
        .bind(account_id)
        .execute(&pool)
        .await
        .unwrap();

        let config = crate::models::internal::configs::AppConfig::from_env();
        let (first, second) = tokio::join!(
            confirm_email_change_ticket(&pool, &config, token, "new@example.test"),
            confirm_email_change_ticket(&pool, &config, token, "new@example.test")
        );
        for outcome in [first.unwrap(), second.unwrap()] {
            let outcome = outcome.expect("durable link must not fall back to cache");
            assert_eq!(outcome.user_id, account_id);
            assert_eq!(outcome.user_name, "player");
        }
        let state: (String, bool, bool) = sqlx::query_as(
            r#"SELECT account.normalized_email, ticket.consumed_at_utc IS NOT NULL,
                      outbox.consumed_at_utc IS NOT NULL
                 FROM "AspNetUsers" account
                 JOIN "EmailChangeTickets" ticket ON ticket.account_id = account.id
                 JOIN "MailOutbox" outbox ON outbox.operation_id = ticket.operation_id
                WHERE account.id = $1"#,
        )
        .bind(account_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, ("NEW@EXAMPLE.TEST".to_string(), true, true));

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
