use super::*;

/// `POST /api/account/mailchangeconfirm` -> `void`.
///
/// Apply a pending email change via a cached single-use token that maps to the
/// account; the new address arrives base64-encoded in `email`. A missing token
/// degrades to a plain success (never 500), matching `verify`.
pub async fn mail_change_confirm(
    State(st): State<SharedState>,
    Json(model): Json<AccountVerifyModel>,
) -> AppResult<MessageResponse> {
    if model.token.is_empty()
        || model.token.len() > MAX_ACCOUNT_TOKEN_BYTES
        || model.email.len() > MAX_ENCODED_EMAIL_BYTES
    {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    }
    let supplied_email = crate::utils::codec::base64_decode(&model.email)
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|email| email.len() <= MAX_EMAIL_BYTES)
        .map(|email| email.trim().to_lowercase())
        .ok_or_else(|| AppError::bad_request("Invalid email"))?;
    let normalized = supplied_email.to_uppercase();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let link =
        link_attempts::lock_email_change(&mut transaction, &model.token, &normalized).await?;
    if link.safe_result.is_some() {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(MessageResponse::ok(""));
    }

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let policy = crate::services::anti_cheat::lock_and_load_account_policy(
        &mut transaction,
        st.config.as_ref(),
    )
    .await?;
    if !verify_email_domain(&supplied_email, &policy.email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let collision: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "AspNetUsers"
                WHERE normalized_email = $1 AND id <> $2
           )"#,
    )
    .bind(&normalized)
    .bind(link.account_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if collision {
        return Err(AppError::conflict("Email already registered"));
    }
    let current = sqlx::query_as::<_, (Option<String>, bool, i16, Option<String>)>(
        r#"SELECT user_name, email_confirmed, role, security_stamp
             FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(link.account_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
    let stamp = current
        .3
        .as_deref()
        .filter(|stamp| {
            link_attempts::value_digest(stamp) == link.security_generation_digest
                && current.1
                && current.2 != Role::Banned as i16
        })
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
    let updated = sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email = $1, normalized_email = $2, email_confirmed = TRUE,
                  security_stamp = $3
            WHERE id = $4 AND security_stamp = $5
              AND email_confirmed = TRUE AND role <> $6"#,
    )
    .bind(&supplied_email)
    .bind(&normalized)
    .bind(Uuid::new_v4().to_string())
    .bind(link.account_id)
    .bind(stamp)
    .bind(Role::Banned as i16)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    }
    let name = current.0.unwrap_or_default();
    link_attempts::complete(
        &mut transaction,
        &model.token,
        link.account_id,
        "email_change",
        &name,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    crate::services::audit::info(
        &st,
        "AccountController",
        Some(name.clone()),
        None,
        format!("User {name} changed email"),
    )
    .await;

    Ok(MessageResponse::ok(""))
}
