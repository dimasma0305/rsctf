use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PasswordResetTicket {
    pub(super) user_id: Uuid,
    pub(super) security_stamp: String,
}

pub(super) fn reset_current_key(user_id: Uuid) -> String {
    format!("pwreset-current:{user_id}")
}

/// Lock and revalidate the account identity before binding a recovery mail
/// operation. The caller keeps this transaction through outbox admission and
/// ticket insertion, so another replica cannot change the destination between
/// validation and publication.
pub(super) async fn lock_reset_identity(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    requested_normalized_email: &str,
) -> AppResult<Option<(PasswordResetTicket, String)>> {
    let identity: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        r#"SELECT email, normalized_email, security_stamp
             FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((Some(email), Some(normalized_email), Some(security_stamp))) = identity else {
        return Ok(None);
    };
    if normalized_email != requested_normalized_email {
        return Ok(None);
    }
    Ok(Some((
        PasswordResetTicket {
            user_id,
            security_stamp,
        },
        email,
    )))
}

/// Replace the account's current durable reset generation inside the same
/// transaction that inserted its mail outbox row.
pub(super) async fn insert_reset_ticket(
    transaction: &mut Transaction<'_, Postgres>,
    ticket: &PasswordResetTicket,
    token_hash: &[u8],
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "PasswordResetTickets"
              SET superseded_at_utc = clock_timestamp()
            WHERE user_id = $1 AND superseded_at_utc IS NULL
              AND consumed_at_utc IS NULL"#,
    )
    .bind(ticket.user_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"INSERT INTO "PasswordResetTickets"
               (token_hash, user_id, security_stamp, expires_at_utc)
           VALUES ($1, $2, $3, clock_timestamp() + INTERVAL '15 minutes')"#,
    )
    .bind(token_hash)
    .bind(ticket.user_id)
    .bind(&ticket.security_stamp)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub(in crate::controllers::account) async fn invalidate_password_reset_tokens(
    st: &SharedState,
    user_id: Uuid,
) {
    let current_key = reset_current_key(user_id);
    if let Some(token) = st.cache.get(&current_key).await {
        if st
            .cache
            .compare_and_remove(&current_key, token.as_ref())
            .await
        {
            if let Ok(token) = std::str::from_utf8(&token) {
                st.cache.remove(&format!("pwreset:{token}")).await;
            }
        }
    }
}
