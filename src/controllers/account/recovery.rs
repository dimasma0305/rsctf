//! Account recovery / email-verification / password-reset / mail-change confirm
//! — split from account/mod.rs to stay under the 1000-line rule.
use super::*;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

mod recovery_mail;
mod reset_ticket_store;
pub(crate) use recovery_mail::process_recovery_mail;
pub(super) use reset_ticket_store::invalidate_password_reset_tokens;
use reset_ticket_store::{
    insert_reset_ticket, lock_reset_identity, reset_current_key, PasswordResetTicket,
};

use super::email_change_support::{
    confirm_email_change_ticket, email_confirmation_required, update_email_serialized,
    EmailUpdateMode, EmailUpdateOutcome, EmailUpdateRequest, ACCOUNT_LINK_TTL,
};
#[cfg(test)]
use super::email_change_support::{publish_ticket, rollback_ticket_publication};

const RECOVERY_TTL: std::time::Duration = ACCOUNT_LINK_TTL;
const RECOVERY_RESPONSE_FLOOR: std::time::Duration = std::time::Duration::from_secs(1);

async fn recovery_success(started: tokio::time::Instant) -> MessageResponse {
    if let Some(remaining) = RECOVERY_RESPONSE_FLOOR.checked_sub(started.elapsed()) {
        tokio::time::sleep(remaining).await;
    }
    MessageResponse::ok("If that email is registered, a password reset link has been sent.")
}

async fn update_authenticated_password(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_security_stamp: &str,
    expected_password_hash: &str,
    new_password_hash: String,
    new_security_stamp: &str,
) -> AppResult<bool> {
    let updated = sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET password_hash = $1, security_stamp = $2
            WHERE id = $3
              AND security_stamp = $4
              AND password_hash = $5
              AND email_confirmed = TRUE
              AND role <> $6"#,
    )
    .bind(new_password_hash)
    .bind(new_security_stamp)
    .bind(user_id)
    .bind(expected_security_stamp)
    .bind(expected_password_hash)
    .bind(Role::Banned as i16)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(updated.rows_affected() == 1)
}

/// `POST /api/account/logout` -> `void`. Valid sessions are revoked; invalid or
/// deleted sessions still receive a clearing cookie so the browser can recover.
pub async fn logout(
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
) -> AppResult<Response> {
    if let Some(user) = user {
        // A stale cached request may clear its own browser cookie, but it must
        // not rotate a newer live session that was issued after this JWT.
        sqlx::query(
            r#"UPDATE "AspNetUsers" SET security_stamp = $1
                WHERE id = $2 AND security_stamp = $3"#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(user.id)
        .bind(&user.security_stamp)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    let mut resp = StatusCode::OK.into_response();
    set_cookie(&mut resp, &clear_session_cookie(st.config.cookie_secure))?;
    Ok(resp)
}

/// `PUT /api/account/changepassword` -> `void`.
pub async fn change_password(
    State(st): State<SharedState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(model): Json<PasswordChangeModel>,
) -> AppResult<Response> {
    validate_password(&model.new)?;
    let account_scope = format!("credential-account:{}:{}", user.id, user.security_stamp);
    let source =
        anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_else(|| peer.ip().to_string());
    let source_scope = format!("credential-source:{source}");
    let mut credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::Interactive,
        &[&account_scope, &source_scope],
    )
    .await?;
    let current = load_user(&st, user.id).await?;
    if !current.email_confirmed
        || current.role == Role::Banned
        || current.security_stamp.as_deref() != Some(user.security_stamp.as_str())
    {
        return Err(AppError::Unauthorized);
    }
    if let Some(normalized_email) = current.normalized_email.as_deref() {
        let email_scope = format!("credential-email:{normalized_email}");
        credential_work.try_add_scopes(&[&email_scope]).await?;
    }
    let old_hash = current.password_hash.clone().unwrap_or_default();
    if model.old.len() > MAX_PASSWORD_BYTES {
        return Err(AppError::bad_request("Old password is incorrect"));
    }
    if !verify_password_async(model.old, old_hash.clone()).await {
        return Err(AppError::bad_request("Old password is incorrect"));
    }
    credential_work.ensure_owned().await?;
    let new_hash = hash_password_async(model.new).await?;
    credential_work.ensure_owned().await?;
    let new_stamp = Uuid::new_v4().to_string();
    if !update_authenticated_password(
        st.pg(),
        user.id,
        &user.security_stamp,
        &old_hash,
        new_hash,
        &new_stamp,
    )
    .await?
    {
        return Err(AppError::Unauthorized);
    }
    invalidate_password_reset_tokens(&st, user.id).await;

    crate::services::audit::info(
        &st,
        "AccountController",
        Some(user.name.clone()),
        None,
        format!("User {} changed password", user.name),
    )
    .await;

    let token = st.token.issue(user.id, user.role, &user.name, &new_stamp)?;
    let mut resp = MessageResponse::ok("").into_response();
    set_cookie(
        &mut resp,
        &set_session_cookie(&token, st.config.jwt_ttl_secs, st.config.cookie_secure),
    )?;
    Ok(resp)
}

/// `POST /api/account/recovery` -> `RequestResponse` (`{title,status}`).
///
/// Look up the account by email, mint a single-use reset token in the cache, and
/// email a reset link (best-effort). Mirrors RSCTF's posture of never revealing
/// whether the address exists: the same success message is returned regardless of
/// whether a matching user was found or the mail relay was even configured.
pub async fn recovery(
    State(st): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(model): Json<RecoveryModel>,
) -> AppResult<MessageResponse> {
    // Captcha gate (RSCTF `AccountController.Recovery`: `if (UseCaptcha &&
    // !VerifyAsync) return BadRequest`), verified BEFORE the email lookup. Only
    // enforced when the live `AccountPolicy:UseCaptcha` is on, so captcha-off
    // recovery is unaffected. `PasswordReset` carries no captcha token and is
    // intentionally NOT gated (RSCTF verifies captcha only on recovery, not reset).
    let captcha =
        crate::services::captcha::CaptchaSettings::load(st.pg(), st.config.account.use_captcha)
            .await?;
    let captcha_admission = captcha
        .verify_local(
            model.challenge.as_deref().unwrap_or(""),
            st.cache.as_ref(),
            st.config.jwt_secret.as_bytes(),
        )
        .await?;
    crate::services::anti_cheat::authorize_captcha_admission(
        st.pg(),
        st.config.as_ref(),
        captcha_admission,
    )
    .await?;

    let response_started = tokio::time::Instant::now();
    let operation_id = model.operation_id;
    if operation_id.is_nil() {
        return Err(AppError::bad_request("operationId is required"));
    }
    let request_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()));
    let norm_email = if model.email.len() <= MAX_EMAIL_BYTES {
        model.email.trim().to_uppercase()
    } else {
        String::new()
    };

    // Do not cancel this future on a process-local wall-clock deadline: doing so
    // can drop the transaction after the SMTP intent has begun committing and
    // leave the caller with an ambiguous success. Every PostgreSQL statement in
    // the preparation path has a shorter server-side timeout instead.
    process_recovery_mail(&st, operation_id, &norm_email, request_ip.as_deref()).await?;

    Ok(recovery_success(response_started).await)
}

/// `POST /api/account/passwordreset` -> `void`.
///
/// Consume the cached single-use reset token, confirm it belongs to the account
/// named by the (base64) email, then re-hash and store the new password.
pub async fn password_reset(
    State(st): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(model): Json<PasswordResetModel>,
) -> AppResult<Response> {
    if model.operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A valid password-reset operation ID is required",
        ));
    }
    if model.password.len() < 6 {
        return Err(AppError::bad_request(
            "Password must be at least 6 characters",
        ));
    }
    validate_password(&model.password)?;
    if model.r_token.is_empty()
        || model.r_token.len() > MAX_ACCOUNT_TOKEN_BYTES
        || model.email.len() > MAX_ENCODED_EMAIL_BYTES
    {
        return Err(AppError::bad_request("Invalid or expired reset token"));
    }

    let token_hash = Sha256::digest(model.r_token.as_bytes()).to_vec();
    let mut digest_builder = Hmac::<Sha256>::new_from_slice(st.config.jwt_secret.as_bytes())
        .map_err(|_| AppError::internal("initialize password reset request digest"))?;
    Mac::update(&mut digest_builder, b"rsctf:password-reset-attempt:v1\0");
    Mac::update(&mut digest_builder, model.email.as_bytes());
    Mac::update(&mut digest_builder, model.password.as_bytes());
    let request_digest = digest_builder.finalize().into_bytes().to_vec();
    let operation_replay: Option<(Vec<u8>, Vec<u8>, i16)> = sqlx::query_as(
        r#"SELECT token_hash, request_digest, status
             FROM "PasswordResetAttempts" WHERE operation_id = $1"#,
    )
    .bind(model.operation_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some((bound_token, bound_digest, status)) = operation_replay {
        if bound_token != token_hash || bound_digest != request_digest {
            return Err(AppError::conflict(
                "Password reset operation ID is bound to different input",
            ));
        }
        if status == 1 {
            let mut resp = MessageResponse::ok("").into_response();
            set_cookie(&mut resp, &clear_session_cookie(st.config.cookie_secure))?;
            return Ok(resp);
        }
    }

    // A terminal operation replay above performs no credential work and remains
    // stable even while the work budget is full. New/recoverable work is admitted
    // on the only trustworthy identity available before ticket/account reads,
    // then extended with resolved semantic identities below.
    let source =
        anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_else(|| peer.ip().to_string());
    let source_scope = format!("credential-source:{source}");
    let mut credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::Interactive,
        &[&source_scope],
    )
    .await?;

    let key = format!("pwreset:{}", model.r_token);
    let durable_ticket: Option<(Uuid, String)> = sqlx::query_as(
        r#"SELECT user_id, security_stamp
             FROM "PasswordResetTickets"
            WHERE token_hash = $1
              AND expires_at_utc > clock_timestamp()
              AND superseded_at_utc IS NULL
              AND consumed_at_utc IS NULL"#,
    )
    .bind(&token_hash)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let ticket = if let Some((user_id, security_stamp)) = durable_ticket {
        PasswordResetTicket {
            user_id,
            security_stamp,
        }
    } else {
        // A cache-only legacy ticket has no recoverable absolute expiry. Never
        // extend it by promoting it at presentation time; new recovery links
        // are durable from issuance and preserve their original deadline.
        return Err(AppError::bad_request("Invalid or expired reset token"));
    };

    // The (base64) email must resolve to the same account the token was minted for.
    let email = crate::utils::codec::base64_decode(&model.email)
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|email| email.len() <= MAX_EMAIL_BYTES)
        .ok_or_else(|| AppError::bad_request("Invalid email"))?;
    let norm_email = email.trim().to_uppercase();

    let current = user::Entity::find_by_id(ticket.user_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("Invalid or expired reset token"))?;
    if current.normalized_email.as_deref() != Some(norm_email.as_str()) {
        return Err(AppError::bad_request("Invalid email"));
    }
    if current.security_stamp.as_deref() != Some(ticket.security_stamp.as_str()) {
        return Err(AppError::bad_request("Invalid or expired reset token"));
    }
    let account_scope = format!(
        "credential-account:{}:{}",
        ticket.user_id, ticket.security_stamp
    );
    let token_scope = format!("credential-reset-token:{}", hex::encode(&token_hash));
    let email_scope = format!("credential-email:{norm_email}");
    credential_work
        .try_add_scopes(&[&account_scope, &token_scope, &email_scope])
        .await?;

    let lease_token = Uuid::new_v4();
    let mut claim = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let ticket_claimable: bool = sqlx::query_scalar(
        r#"SELECT expires_at_utc > clock_timestamp()
                  AND superseded_at_utc IS NULL AND consumed_at_utc IS NULL
             FROM "PasswordResetTickets"
            WHERE token_hash = $1 FOR UPDATE"#,
    )
    .bind(&token_hash)
    .fetch_optional(&mut *claim)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(false);
    let existing: Option<(Uuid, Vec<u8>, i16, bool)> = sqlx::query_as(
        r#"SELECT operation_id, request_digest, status,
                  lease_expires_at_utc > clock_timestamp()
             FROM "PasswordResetAttempts" WHERE token_hash = $1 FOR UPDATE"#,
    )
    .bind(&token_hash)
    .fetch_optional(&mut *claim)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some((operation_id, digest, status, lease_live)) = existing {
        if operation_id != model.operation_id || digest != request_digest {
            return Err(AppError::conflict(
                "Reset token is already bound to another attempt",
            ));
        }
        if status == 1 {
            claim
                .rollback()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            let mut resp = MessageResponse::ok("").into_response();
            set_cookie(&mut resp, &clear_session_cookie(st.config.cookie_secure))?;
            return Ok(resp);
        }
        if lease_live {
            return Err(AppError::too_many_requests(1));
        }
        if !ticket_claimable {
            return Err(AppError::bad_request("Invalid or expired reset token"));
        }
        sqlx::query(
            r#"UPDATE "PasswordResetAttempts"
                  SET lease_token = $2,
                      lease_expires_at_utc = clock_timestamp() + INTERVAL '45 seconds'
                WHERE operation_id = $1 AND status = 0"#,
        )
        .bind(model.operation_id)
        .bind(lease_token)
        .execute(&mut *claim)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    } else {
        if !ticket_claimable {
            return Err(AppError::bad_request("Invalid or expired reset token"));
        }
        sqlx::query(
            r#"INSERT INTO "PasswordResetAttempts"
                   (operation_id, token_hash, request_digest, lease_token,
                    lease_expires_at_utc)
               VALUES ($1, $2, $3, $4,
                       clock_timestamp() + INTERVAL '45 seconds')"#,
        )
        .bind(model.operation_id)
        .bind(&token_hash)
        .bind(&request_digest)
        .bind(lease_token)
        .execute(&mut *claim)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    claim
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let new_hash = hash_password_async(model.password.clone()).await?;
    credential_work.ensure_owned().await?;
    let renewed_attempt = sqlx::query(
        r#"UPDATE "PasswordResetAttempts"
              SET lease_expires_at_utc = clock_timestamp() + INTERVAL '45 seconds'
            WHERE operation_id = $1 AND lease_token = $2 AND status = 0"#,
    )
    .bind(model.operation_id)
    .bind(lease_token)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if renewed_attempt != 1 {
        return Err(AppError::conflict("Password reset attempt lost its lease"));
    }

    // Authorize the write against the same security stamp. A concurrent logout or
    // password change either wins first and makes this affect zero rows, or wins
    // afterward and replaces this reset.
    let name = current.user_name.clone().unwrap_or_default();
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let updated = sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET password_hash = $4, security_stamp = $5
            WHERE id = $1 AND normalized_email = $2 AND security_stamp = $3"#,
    )
    .bind(ticket.user_id)
    .bind(&norm_email)
    .bind(&ticket.security_stamp)
    .bind(new_hash)
    .bind(Uuid::new_v4().to_string())
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if updated != 1 {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::bad_request("Invalid or expired reset token"));
    }
    let consumed = sqlx::query(
        r#"UPDATE "PasswordResetTickets"
              SET consumed_at_utc = clock_timestamp()
            WHERE token_hash = $1 AND consumed_at_utc IS NULL
              AND superseded_at_utc IS NULL AND expires_at_utc > clock_timestamp()"#,
    )
    .bind(&token_hash)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    let completed = sqlx::query(
        r#"UPDATE "PasswordResetAttempts"
              SET status = 1, completed_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND lease_token = $2 AND status = 0"#,
    )
    .bind(model.operation_id)
    .bind(lease_token)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if consumed != 1 || completed != 1 {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict("Password reset attempt lost its lease"));
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    st.cache.remove(&reset_current_key(ticket.user_id)).await;
    st.cache.remove(&key).await;

    // RSCTF `AccountController` audit event (`Account_PasswordReset`). Best-effort.
    crate::services::audit::info(
        &st,
        "AccountController",
        Some(name.clone()),
        None,
        format!("User {name} reset password"),
    )
    .await;

    let mut resp = MessageResponse::ok("").into_response();
    set_cookie(&mut resp, &clear_session_cookie(st.config.cookie_secure))?;
    Ok(resp)
}

/// `PUT /api/account/changeemail` -> `RequestResponseOfBoolean`.
///
/// Re-authentication is mandatory in both modes so possession of a session JWT
/// alone cannot redirect password-recovery mail and make a theft permanent.
pub async fn change_email(
    State(st): State<SharedState>,
    user: CurrentUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(model): Json<MailChangeModel>,
) -> AppResult<Response> {
    let operation_id = model.operation_id;
    if operation_id.is_nil() {
        return Err(AppError::bad_request("operationId is required"));
    }
    let request_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()));
    let new_mail = model.new_mail.trim().to_lowercase();
    if new_mail.len() > MAX_EMAIL_BYTES || !new_mail.contains('@') {
        return Err(AppError::bad_request("Invalid email address"));
    }
    let account_scope = format!("credential-account:{}:{}", user.id, user.security_stamp);
    let destination_scope = format!("credential-email:{}", new_mail.to_uppercase());
    let source =
        anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_else(|| peer.ip().to_string());
    let source_scope = format!("credential-source:{source}");
    let mut credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::Interactive,
        &[&account_scope, &destination_scope, &source_scope],
    )
    .await?;
    if !verify_email_domain(&new_mail, &load_email_domain_list(&st).await?) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let norm = new_mail.to_uppercase();

    let current = load_user(&st, user.id).await?;
    if !current.email_confirmed
        || current.role == Role::Banned
        || current.security_stamp.as_deref() != Some(user.security_stamp.as_str())
    {
        return Err(AppError::Unauthorized);
    }
    if let Some(normalized_email) = current.normalized_email.as_deref() {
        let current_email_scope = format!("credential-email:{normalized_email}");
        credential_work
            .try_add_scopes(&[&current_email_scope])
            .await?;
    }
    if model.password.len() > MAX_PASSWORD_BYTES {
        return Err(AppError::bad_request("Current password is incorrect"));
    }
    if !verify_password_async(
        model.password,
        current.password_hash.clone().unwrap_or_default(),
    )
    .await
    {
        return Err(AppError::bad_request("Current password is incorrect"));
    }
    credential_work.ensure_owned().await?;
    let expected_stamp = user.security_stamp.clone();
    if user::Entity::find()
        .filter(user::Column::NormalizedEmail.eq(norm.clone()))
        .filter(user::Column::Id.ne(user.id))
        .one(&st.db)
        .await?
        .is_some()
    {
        return Err(AppError::conflict("Email already registered"));
    }

    let confirmation_required = email_confirmation_required(&st).await?;
    let mut refreshed_stamp = None;
    if confirmation_required {
        let token = crate::utils::codec::random_token(32);
        let expires_at_unix = Utc::now().timestamp() + RECOVERY_TTL.as_secs() as i64;

        let encoded = crate::utils::codec::base64_encode(new_mail.as_bytes())
            .replace('+', "%2B")
            .replace('/', "%2F")
            .replace('=', "%3D");
        let base = std::env::var("RSCTF_PUBLIC_URL")
            .ok()
            .map(|url| url.trim_end_matches('/').to_string())
            .unwrap_or_default();
        let link = format!("{base}/account/confirm?token={token}&email={encoded}");
        let (subject, body) = crate::services::mail::change_email(
            &new_mail,
            &link,
            Some(st.config.global.title.as_str()),
        );
        let mut transaction = st
            .pg()
            .begin()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(REGISTRATION_LOCK_ID)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let identity_is_current = sqlx::query_scalar::<_, bool>(
            r#"SELECT TRUE FROM "AspNetUsers"
                WHERE id = $1 AND security_stamp = $2
                  AND email_confirmed = TRUE AND role <> $3
                FOR SHARE"#,
        )
        .bind(user.id)
        .bind(&expected_stamp)
        .bind(Role::Banned as i16)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .unwrap_or(false);
        if !identity_is_current {
            return Err(AppError::Unauthorized);
        }
        let outcome = crate::services::mail_outbox::enqueue_in_transaction(
            &mut transaction,
            crate::services::mail_outbox::MailIntent {
                operation_id,
                purpose: crate::services::mail_outbox::MailPurpose::EmailChange,
                account_id: user.id,
                security_generation: &expected_stamp,
                destination: &new_mail,
                source: request_ip.as_deref(),
                subject: &subject,
                html_body: &body,
            },
        )
        .await?;
        let inserted = outcome == crate::services::mail_outbox::EnqueueOutcome::Inserted;
        if inserted {
            super::link_attempts::stage(
                &mut transaction,
                operation_id,
                &token,
                super::link_attempts::Purpose::EmailChange,
                user.id,
                &expected_stamp,
                &norm,
                expires_at_unix,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    } else {
        let new_stamp = Uuid::new_v4().to_string();
        match update_email_serialized(
            st.pg(),
            st.config.as_ref(),
            EmailUpdateRequest {
                user_id: user.id,
                expected_stamp: &expected_stamp,
                email: &new_mail,
                normalized_email: &norm,
                new_stamp: new_stamp.clone(),
                mode: EmailUpdateMode::Immediate,
            },
        )
        .await?
        {
            EmailUpdateOutcome::Updated => refreshed_stamp = Some(new_stamp),
            EmailUpdateOutcome::Conflict => {
                return Err(AppError::conflict("Email already registered"));
            }
            EmailUpdateOutcome::StampMismatch => return Err(AppError::Unauthorized),
        }
    }

    crate::services::audit::info(
        &st,
        "AccountController",
        Some(user.name.clone()),
        None,
        format!("User {} requested email change", user.name),
    )
    .await;
    let mut response = Wrapped::ok(confirmation_required).into_response();
    if let Some(stamp) = refreshed_stamp {
        let token = st.token.issue(user.id, user.role, &user.name, &stamp)?;
        set_cookie(
            &mut response,
            &set_session_cookie(&token, st.config.jwt_ttl_secs, st.config.cookie_secure),
        )?;
    }
    Ok(response)
}

enum DigestLinkResult {
    Absent,
    Replayed,
    Updated(String),
}

async fn confirm_digest_email_change(
    st: &SharedState,
    token: &str,
    email: &str,
) -> AppResult<DigestLinkResult> {
    let token_digest = super::link_attempts::digest(token.as_bytes());
    let exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(SELECT 1 FROM "AccountLinkAttempts"
                           WHERE token_digest = $1 AND purpose = $2)"#,
    )
    .bind(token_digest.to_vec())
    .bind(super::link_attempts::Purpose::EmailChange as i16)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !exists {
        return Ok(DigestLinkResult::Absent);
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let attempt = match super::link_attempts::claim(
        &mut transaction,
        token,
        super::link_attempts::Purpose::EmailChange,
    )
    .await?
    {
        super::link_attempts::Claim::Completed(_) => {
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            return Ok(DigestLinkResult::Replayed);
        }
        super::link_attempts::Claim::Pending(attempt) => attempt,
    };
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let policy =
        anti_cheat::lock_and_load_account_policy(&mut transaction, st.config.as_ref()).await?;
    if !verify_email_domain(email, &policy.email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let normalized = email.to_uppercase();
    if attempt.destination_digest != super::link_attempts::digest(normalized.as_bytes()) {
        return Err(AppError::bad_request("Invalid email"));
    }
    let current: Option<(Option<String>, String)> = sqlx::query_as(
        r#"SELECT user_name, security_stamp FROM "AspNetUsers"
            WHERE id = $1 AND email_confirmed = TRUE AND role <> $2 FOR UPDATE"#,
    )
    .bind(attempt.account_id)
    .bind(Role::Banned as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((name, security_stamp)) = current else {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    };
    if attempt.security_generation_digest != super::link_attempts::digest(security_stamp.as_bytes())
    {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    }
    let changed = sqlx::query(
        r#"UPDATE "AspNetUsers" SET email = $1, normalized_email = $2,
                  security_stamp = $3
            WHERE id = $4 AND security_stamp = $5 AND NOT EXISTS (
                SELECT 1 FROM "AspNetUsers" WHERE normalized_email = $2 AND id <> $4
            )"#,
    )
    .bind(email)
    .bind(&normalized)
    .bind(Uuid::new_v4().to_string())
    .bind(attempt.account_id)
    .bind(&security_stamp)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if changed.rows_affected() != 1 {
        return Err(AppError::conflict("Email already registered"));
    }
    super::link_attempts::complete(
        &mut transaction,
        &attempt,
        &serde_json::json!({ "status": "emailChanged" }),
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(DigestLinkResult::Updated(name.unwrap_or_default()))
}

/// `POST /api/account/mailchangeconfirm` -> `void`.
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
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|email| email.len() <= MAX_EMAIL_BYTES)
        .map(|email| email.trim().to_lowercase())
        .ok_or_else(|| AppError::bad_request("Invalid email"))?;
    match confirm_digest_email_change(&st, &model.token, &supplied_email).await? {
        DigestLinkResult::Replayed => return Ok(MessageResponse::ok("")),
        DigestLinkResult::Updated(name) => {
            crate::services::audit::info(
                &st,
                "AccountController",
                Some(name.clone()),
                None,
                format!("User {name} changed email"),
            )
            .await;
            return Ok(MessageResponse::ok(""));
        }
        DigestLinkResult::Absent => {}
    }

    if let Some(confirmed) =
        confirm_email_change_ticket(st.pg(), st.config.as_ref(), &model.token, &supplied_email)
            .await?
    {
        st.cache
            .compare_and_remove(
                &format!("emailchange-current:{}", confirmed.user_id),
                model.token.as_bytes(),
            )
            .await;
        st.cache
            .remove(&format!("emailchange:{}", model.token))
            .await;
        crate::services::audit::info(
            &st,
            "AccountController",
            Some(confirmed.user_name.clone()),
            None,
            format!("User {} changed email", confirmed.user_name),
        )
        .await;
        return Ok(MessageResponse::ok(""));
    }

    // Compatibility for cache-only links issued before the durable migrations.
    let key = format!("emailchange:{}", model.token);
    let ticket_bytes = st
        .cache
        .get(&key)
        .await
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
    let ticket: EmailChangeTicket = serde_json::from_slice(&ticket_bytes)
        .map_err(|_| AppError::bad_request("Invalid or expired email-change token"))?;
    if supplied_email != ticket.new_email
        || !verify_email_domain(&ticket.new_email, &load_email_domain_list(&st).await?)
    {
        return Err(AppError::bad_request("Invalid email"));
    }
    let current = user::Entity::find_by_id(ticket.user_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
    if current.security_stamp.as_deref() != Some(ticket.security_stamp.as_str())
        || !st
            .cache
            .compare_and_remove(
                &format!("emailchange-current:{}", ticket.user_id),
                model.token.as_bytes(),
            )
            .await
        || st.cache.get_and_remove(&key).await.as_deref() != Some(ticket_bytes.as_ref())
    {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    }
    let name = current.user_name.unwrap_or_default();
    match update_email_serialized(
        st.pg(),
        st.config.as_ref(),
        EmailUpdateRequest {
            user_id: ticket.user_id,
            expected_stamp: &ticket.security_stamp,
            email: &ticket.new_email,
            normalized_email: &ticket.new_email.to_uppercase(),
            new_stamp: Uuid::new_v4().to_string(),
            mode: EmailUpdateMode::ConfirmedTicket,
        },
    )
    .await?
    {
        EmailUpdateOutcome::Updated => {}
        EmailUpdateOutcome::Conflict => return Err(AppError::conflict("Email already registered")),
        EmailUpdateOutcome::StampMismatch => {
            return Err(AppError::bad_request(
                "Invalid or expired email-change token",
            ));
        }
    }
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

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
