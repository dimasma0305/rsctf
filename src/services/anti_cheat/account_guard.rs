//! Live-account revalidation inside an identity admission transaction.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::database_error;
use crate::utils::error::{AppError, AppResult};

pub(super) async fn lock_live_existing_account(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    expected_security_stamp: &str,
    expected_normalized_email: Option<&str>,
) -> AppResult<()> {
    let row = sqlx::query_as::<_, (bool, i16, Option<String>, Option<String>)>(
        r#"SELECT email_confirmed, role, security_stamp, normalized_email
             FROM "AspNetUsers"
            WHERE id = $1
            FOR UPDATE"#,
    )
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AppError::not_found("User not found"))?;
    let stamp_matches = row
        .2
        .as_deref()
        .filter(|stamp| !stamp.is_empty())
        .is_none_or(|stamp| stamp == expected_security_stamp);
    let email_matches =
        expected_normalized_email.is_none_or(|expected| row.3.as_deref() == Some(expected));
    if !row.0
        || row.1 == crate::utils::enums::Role::Banned as i16
        || !stamp_matches
        || !email_matches
    {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

/// Revalidate an authenticated request against the exact account state carried
/// by its JWT. Unlike login admission, this path never repairs a missing legacy
/// stamp: an already-issued request must match exactly or fail closed.
pub(super) async fn lock_live_request_account(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    expected_security_stamp: &str,
) -> AppResult<()> {
    let row = sqlx::query_as::<_, (bool, i16, Option<String>)>(
        r#"SELECT email_confirmed, role, security_stamp
             FROM "AspNetUsers"
            WHERE id = $1
            FOR SHARE"#,
    )
    .bind(user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or(AppError::Unauthorized)?;
    if !row.0
        || row.1 == crate::utils::enums::Role::Banned as i16
        || row.2.as_deref() != Some(expected_security_stamp)
    {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}
