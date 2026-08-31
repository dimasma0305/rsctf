//! Bounded, enumeration-resistant password-recovery mail preparation.

use super::*;

pub(super) async fn process_recovery_mail(
    st: &SharedState,
    operation_id: Uuid,
    normalized_email: &str,
    request_ip: Option<&str>,
) -> AppResult<()> {
    // Source/deployment overload is account-independent, so the route can
    // preserve its Retry-After response. Other preparation errors are folded
    // into the generic result because operation replay state can imply that an
    // earlier lookup resolved an account. Once an account lookup succeeds,
    // every failure is likewise folded into the enumeration-safe response.
    let preparation = crate::services::mail_outbox::try_prepare(
        st.pg(),
        operation_id,
        crate::services::mail_outbox::MailPurpose::PasswordRecovery,
        normalized_email,
        request_ip,
    )
    .await;
    let preparation = match preparation {
        Ok(preparation) => preparation,
        Err(error) => {
            if matches!(
                &error,
                AppError::TooManyRequests { .. } | AppError::RetryableUnavailable { .. }
            ) {
                // These limits are source/deployment scoped and therefore do
                // not vary with whether this address is registered.
                return Err(error);
            }
            // In particular, do not expose operation replay conflicts: an
            // outbox row exists only for a registered address, so a 409 here
            // would become an account-existence oracle.
            tracing::debug!(
                %error,
                operation_id = %operation_id,
                "password-recovery mail preparation was not admitted"
            );
            return Ok(());
        }
    };
    let Some(mut preparation) = preparation else {
        return Ok(());
    };

    let result = async {
        let mut lookup = st
            .pg()
            .begin()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        set_recovery_sql_bounds(&mut lookup).await?;
        let user_id = sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM "AspNetUsers" WHERE normalized_email = $1 LIMIT 1"#,
        )
        .bind(normalized_email)
        .fetch_optional(&mut *lookup)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        lookup
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let Some(user_id) = user_id else {
            return Ok(());
        };
        preparation.bind_account(user_id).await?;
        let mut transaction = st
            .pg()
            .begin()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        // Bound waits at PostgreSQL itself so cancellation or client disconnect
        // cannot leave a long-running command occupying a pooled connection.
        set_recovery_sql_bounds(&mut transaction).await?;
        let Some((ticket, user_email)) =
            lock_reset_identity(&mut transaction, user_id, normalized_email).await?
        else {
            transaction
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(());
        };

        let token = crate::utils::codec::random_token(32);
        let token_hash = Sha256::digest(token.as_bytes()).to_vec();
        let current_key = reset_current_key(user_id);
        let base = std::env::var("RSCTF_PUBLIC_URL")
            .ok()
            .map(|url| url.trim_end_matches('/').to_string())
            .unwrap_or_default();
        let link = format!(
            "{base}/account/reset?token={token}&email={}",
            crate::utils::codec::base64_encode(user_email.as_bytes())
        );
        let (subject, body) =
            crate::services::mail::reset_password(&link, Some(st.config.global.title.as_str()));
        let outcome = crate::services::mail_outbox::enqueue_in_transaction(
            &mut transaction,
            crate::services::mail_outbox::MailIntent {
                operation_id,
                purpose: crate::services::mail_outbox::MailPurpose::PasswordRecovery,
                account_id: user_id,
                security_generation: &ticket.security_stamp,
                destination: &user_email,
                source: request_ip,
                subject: &subject,
                html_body: &body,
            },
        )
        .await?;
        preparation
            .ensure_owned_in_transaction(&mut transaction)
            .await?;

        match outcome {
            crate::services::mail_outbox::EnqueueOutcome::Inserted => {
                insert_reset_ticket(&mut transaction, &ticket, &token_hash).await?;
                transaction
                    .commit()
                    .await
                    .map_err(|error| AppError::internal(error.to_string()))?;
                invalidate_password_reset_tokens(st, user_id).await;
                let ticket_bytes = serde_json::to_vec(&ticket).map_err(|error| {
                    AppError::internal(format!("password-reset ticket: {error}"))
                })?;
                st.cache
                    .set(
                        &format!("pwreset:{token}"),
                        &ticket_bytes,
                        Some(RECOVERY_TTL),
                    )
                    .await;
                st.cache
                    .set(&current_key, token.as_bytes(), Some(RECOVERY_TTL))
                    .await;
            }
            crate::services::mail_outbox::EnqueueOutcome::Replayed => {
                transaction
                    .commit()
                    .await
                    .map_err(|error| AppError::internal(error.to_string()))?;
            }
        }
        Ok(())
    }
    .await;
    preparation.release().await;
    if let Err(error) = result {
        tracing::debug!(%error, operation_id = %operation_id, "password-recovery mail intent was not committed");
    }
    Ok(())
}

async fn set_recovery_sql_bounds(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> AppResult<()> {
    sqlx::query("SET LOCAL lock_timeout = '300ms'")
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SET LOCAL statement_timeout = '700ms'")
        .execute(&mut **transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
