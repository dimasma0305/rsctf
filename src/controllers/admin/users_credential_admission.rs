//! Bounded replica-safe scopes shared by administrator credential batches.

use super::*;

pub(super) async fn admin_credential_scopes(
    pool: &sqlx::PgPool,
    normalized_emails: &[String],
    normalized_names: &[String],
    source: &str,
) -> AppResult<Vec<String>> {
    let mut scopes = Vec::with_capacity(normalized_emails.len().saturating_add(3));
    scopes.push("admin-credential-bulk".to_string());
    scopes.push(format!("credential-source:{source}"));
    scopes.extend(
        normalized_emails
            .iter()
            .map(|email| format!("credential-email:{email}")),
    );
    let accounts: Vec<(Uuid, Option<String>)> = sqlx::query_as(
        r#"SELECT id, security_stamp
             FROM "AspNetUsers"
            WHERE normalized_email = ANY($1) OR normalized_user_name = ANY($2)"#,
    )
    .bind(normalized_emails)
    .bind(normalized_names)
    .fetch_all(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    scopes.extend(
        accounts.into_iter().filter_map(|(id, stamp)| {
            stamp.map(|stamp| format!("credential-account:{id}:{stamp}"))
        }),
    );
    Ok(scopes)
}
