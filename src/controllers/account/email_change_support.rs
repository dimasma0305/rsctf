//! Shared ticket publication and serialized identity helpers for email changes.

use super::*;

pub(super) const ACCOUNT_LINK_TTL: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// Cache publication paired with a durable mail intent. The database outbox
/// serializes replacements; this snapshot lets a failed commit restore the
/// prior usable link without overwriting a newer cache generation.
pub(super) struct TicketPublication {
    current_key: String,
    ticket_prefix: &'static str,
    token: Vec<u8>,
    ticket: Vec<u8>,
    previous: Option<(Vec<u8>, Vec<u8>)>,
}

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
    cache
        .set(&current_key, token, Some(ACCOUNT_LINK_TTL))
        .await;
    TicketPublication {
        current_key,
        ticket_prefix,
        token: token.to_vec(),
        ticket: ticket.to_vec(),
        previous,
    }
}

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
