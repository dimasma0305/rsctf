//! Account recovery / email-verification / password-reset / mail-change confirm
//! — split from account/mod.rs to stay under the 1000-line rule.
use super::*;
use sea_orm::sea_query::Expr;

const RECOVERY_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
const RECOVERY_RESPONSE_FLOOR: std::time::Duration = std::time::Duration::from_millis(25);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PasswordResetTicket {
    user_id: Uuid,
    security_stamp: String,
}

fn reset_current_key(user_id: Uuid) -> String {
    format!("pwreset-current:{user_id}")
}

pub(crate) fn verify_email_domain(email: &str, domain_list: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    if domain_list.trim().is_empty() {
        return true;
    }
    domain_list
        .split(',')
        .map(str::trim)
        .filter(|domain| !domain.is_empty())
        .any(|allowed| allowed.eq_ignore_ascii_case(domain))
}

pub(crate) async fn load_email_domain_list(st: &SharedState) -> AppResult<String> {
    Ok(
        config::Entity::find_by_id("AccountPolicy:EmailDomainList".to_string())
            .one(&st.db)
            .await?
            .and_then(|row| row.value)
            .unwrap_or_default(),
    )
}

async fn email_confirmation_required(st: &SharedState) -> AppResult<bool> {
    Ok(
        config::Entity::find_by_id("AccountPolicy:EmailConfirmationRequired".to_string())
            .one(&st.db)
            .await?
            .and_then(|row| row.value)
            .map(|value| value == "true")
            .unwrap_or(st.config.account.email_confirmation_required),
    )
}

async fn update_password_if_stamp_matches(
    txn: &DatabaseTransaction,
    user_id: Uuid,
    normalized_email: &str,
    expected_stamp: &str,
    password_hash: String,
    new_stamp: String,
) -> AppResult<bool> {
    let result = user::Entity::update_many()
        .col_expr(user::Column::PasswordHash, Expr::value(password_hash))
        .col_expr(user::Column::SecurityStamp, Expr::value(new_stamp))
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::NormalizedEmail.eq(normalized_email))
        .filter(user::Column::SecurityStamp.eq(expected_stamp))
        .exec(txn)
        .await?;
    Ok(result.rows_affected == 1)
}

#[derive(Debug, PartialEq, Eq)]
enum EmailUpdateOutcome {
    Updated,
    Conflict,
    StampMismatch,
}

/// Commit an email identity change under the same cross-replica lock used by
/// password registration, OAuth provisioning, and admin identity writers.
/// `normalized_email` is not protected by a database unique constraint on
/// existing installations, so the in-lock recheck is the authoritative guard;
/// any earlier handler-level lookup is only a fast failure path.
async fn update_email_serialized(
    pool: &sqlx::PgPool,
    user_id: Uuid,
    expected_stamp: &str,
    email: &str,
    normalized_email: &str,
    new_stamp: String,
) -> AppResult<EmailUpdateOutcome> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    let collision: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "AspNetUsers"
                WHERE normalized_email = $1 AND id <> $2
           )"#,
    )
    .bind(normalized_email)
    .bind(user_id)
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
            WHERE id = $4 AND security_stamp = $5"#,
    )
    .bind(email)
    .bind(normalized_email)
    .bind(new_stamp)
    .bind(user_id)
    .bind(expected_stamp)
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

pub(super) async fn invalidate_password_reset_tokens(st: &SharedState, user_id: Uuid) {
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

/// `POST /api/account/logout` -> `void`. Valid sessions are revoked; invalid or
/// deleted sessions still receive a clearing cookie so the browser can recover.
pub async fn logout(
    State(st): State<SharedState>,
    MaybeUser(user): MaybeUser,
) -> AppResult<Response> {
    if let Some(user) = user {
        let current = load_user(&st, user.id).await?;
        let mut am: user::ActiveModel = current.into();
        am.security_stamp = Set(Some(Uuid::new_v4().to_string()));
        am.update(&st.db).await?;
    }

    let mut resp = StatusCode::OK.into_response();
    set_cookie(&mut resp, &clear_session_cookie(st.config.cookie_secure))?;
    Ok(resp)
}

/// `PUT /api/account/changepassword` -> `void`.
pub async fn change_password(
    State(st): State<SharedState>,
    user: CurrentUser,
    Json(model): Json<PasswordChangeModel>,
) -> AppResult<Response> {
    validate_password(&model.new)?;
    let current = load_user(&st, user.id).await?;
    if model.old.len() > MAX_PASSWORD_BYTES {
        return Err(AppError::bad_request("Old password is incorrect"));
    }
    if !verify_password_async(model.old, current.password_hash.clone().unwrap_or_default()).await {
        return Err(AppError::bad_request("Old password is incorrect"));
    }
    let new_hash = hash_password_async(model.new).await?;
    let new_stamp = Uuid::new_v4().to_string();
    let mut am: user::ActiveModel = current.into();
    am.password_hash = Set(Some(new_hash));
    am.security_stamp = Set(Some(new_stamp.clone()));
    am.update(&st.db).await?;
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
    Json(model): Json<RecoveryModel>,
) -> AppResult<MessageResponse> {
    // Captcha gate (RSCTF `AccountController.Recovery`: `if (UseCaptcha &&
    // !VerifyAsync) return BadRequest`), verified BEFORE the email lookup. Only
    // enforced when the live `AccountPolicy:UseCaptcha` is on, so captcha-off
    // recovery is unaffected. `PasswordReset` carries no captcha token and is
    // intentionally NOT gated (RSCTF verifies captcha only on recovery, not reset).
    let captcha = crate::services::captcha::CaptchaSettings::load(&st.db).await;
    if captcha.use_captcha
        && !captcha
            .service()
            .verify(model.challenge.as_deref().unwrap_or(""), st.cache.as_ref())
            .await?
    {
        return Err(AppError::bad_request("Captcha failed"));
    }

    let response_started = tokio::time::Instant::now();
    let norm_email = if model.email.len() <= MAX_EMAIL_BYTES {
        model.email.trim().to_uppercase()
    } else {
        String::new()
    };

    if let Some(user) = user::Entity::find()
        .filter(user::Column::NormalizedEmail.eq(norm_email))
        .one(&st.db)
        .await?
    {
        // Opaque single-use token. A per-user current-generation pointer makes a
        // newer recovery request supersede every older outstanding link.
        let token = crate::utils::codec::random_token(32);
        let key = format!("pwreset:{token}");
        let current_key = reset_current_key(user.id);
        invalidate_password_reset_tokens(&st, user.id).await;
        let ticket = PasswordResetTicket {
            user_id: user.id,
            security_stamp: user.security_stamp.clone().unwrap_or_default(),
        };
        let ticket = serde_json::to_vec(&ticket)
            .map_err(|e| AppError::internal(format!("password-reset ticket: {e}")))?;
        st.cache.set(&key, &ticket, Some(RECOVERY_TTL)).await;
        st.cache
            .set(&current_key, token.as_bytes(), Some(RECOVERY_TTL))
            .await;

        // Build the link the SPA's /account/reset page consumes: the token verbatim
        // plus the base64-encoded email (as RSCTF's GetEmailLink does). An optional
        // RSCTF_PUBLIC_URL makes it absolute; otherwise it stays site-relative.
        let user_email = user
            .email
            .clone()
            .unwrap_or_else(|| model.email.trim().to_string());
        let base = std::env::var("RSCTF_PUBLIC_URL")
            .ok()
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_default();
        let link = format!(
            "{base}/account/reset?token={token}&email={}",
            crate::utils::codec::base64_encode(user_email.as_bytes())
        );

        // SMTP latency and audit insertion must not reveal whether the lookup hit.
        // Token generation stays ordered on the request path, while delivery runs
        // best-effort after the indistinguishable response has been produced.
        let background = st.clone();
        tokio::spawn(async move {
            let (subject, body) = crate::services::mail::reset_password(
                &link,
                Some(background.config.global.title.as_str()),
            );
            let sender = crate::services::mail::MailSender::from_env();
            let _ = sender.send(&user_email, &subject, &body).await;
            crate::services::audit::info(
                &background,
                "AccountController",
                None,
                None,
                format!("Password recovery email requested for {user_email}"),
            )
            .await;
        });
    }

    if let Some(remaining) = RECOVERY_RESPONSE_FLOOR.checked_sub(response_started.elapsed()) {
        tokio::time::sleep(remaining).await;
    }
    Ok(MessageResponse::ok(
        "If that email is registered, a password reset link has been sent.",
    ))
}

/// `POST /api/account/passwordreset` -> `void`.
///
/// Consume the cached single-use reset token, confirm it belongs to the account
/// named by the (base64) email, then re-hash and store the new password.
pub async fn password_reset(
    State(st): State<SharedState>,
    Json(model): Json<PasswordResetModel>,
) -> AppResult<Response> {
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

    let key = format!("pwreset:{}", model.r_token);
    let ticket_bytes = st
        .cache
        .get(&key)
        .await
        .ok_or_else(|| AppError::bad_request("Invalid or expired reset token"))?;
    let ticket: PasswordResetTicket = serde_json::from_slice(&ticket_bytes)
        .map_err(|_| AppError::bad_request("Invalid or expired reset token"))?;

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

    // Do the expensive work before claiming the token. The conditional database
    // update below still rejects a concurrent security-stamp rotation.
    let new_hash = hash_password_async(model.password.clone()).await?;

    // Atomically claim the current generation, then consume its ticket. Exactly
    // one concurrent request can pass this point, and an older generation cannot.
    let current_key = reset_current_key(ticket.user_id);
    if !st
        .cache
        .compare_and_remove(&current_key, model.r_token.as_bytes())
        .await
    {
        return Err(AppError::bad_request("Invalid or expired reset token"));
    }
    let consumed = st
        .cache
        .get_and_remove(&key)
        .await
        .ok_or_else(|| AppError::bad_request("Invalid or expired reset token"))?;
    if consumed != ticket_bytes {
        return Err(AppError::bad_request("Invalid or expired reset token"));
    }

    // Authorize the write against the same security stamp. A concurrent logout or
    // password change either wins first and makes this affect zero rows, or wins
    // afterward and replaces this reset.
    let name = current.user_name.clone().unwrap_or_default();
    let txn = crate::utils::database::begin_seaorm_transaction(&st.db).await?;
    let updated = update_password_if_stamp_matches(
        &txn,
        ticket.user_id,
        &norm_email,
        &ticket.security_stamp,
        new_hash,
        Uuid::new_v4().to_string(),
    )
    .await?;
    if !updated {
        txn.rollback().await?;
        return Err(AppError::bad_request("Invalid or expired reset token"));
    }
    txn.commit().await?;

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

/// `POST /api/account/verify` -> `void`.
///
/// Confirm an account's email using a cached single-use confirmation token. With
/// no token present (the store is empty in the default wiring) the request
/// degrades to a plain success rather than erroring — it never returns 500.
pub async fn verify(
    State(st): State<SharedState>,
    Json(model): Json<AccountVerifyModel>,
) -> AppResult<MessageResponse> {
    if model.token.len() > MAX_ACCOUNT_TOKEN_BYTES || model.email.len() > MAX_ENCODED_EMAIL_BYTES {
        return Ok(MessageResponse::ok(""));
    }
    let key = format!("emailconfirm:{}", model.token);
    if let Some(user_id) = st.cache.get(&key).await.and_then(|b| {
        std::str::from_utf8(&b)
            .ok()
            .and_then(|s| Uuid::parse_str(s).ok())
    }) {
        // Act only when the token maps to the account named by the (base64) email.
        let email = crate::utils::codec::base64_decode(&model.email)
            .and_then(|b| String::from_utf8(b).ok())
            .map(|e| e.trim().to_uppercase());

        if let Some(current) = user::Entity::find_by_id(user_id).one(&st.db).await? {
            if email.is_none() || current.normalized_email.as_deref() == email.as_deref() {
                let name = current.user_name.clone().unwrap_or_default();
                let mut am: user::ActiveModel = current.into();
                am.email_confirmed = Set(true);
                am.update(&st.db).await?;

                // RSCTF `AccountController` audit event (`Account_EmailVerified`).
                // Best-effort.
                crate::services::audit::info(
                    &st,
                    "AccountController",
                    Some(name.clone()),
                    None,
                    format!("User {name} verified email"),
                )
                .await;
            }
        }
        st.cache.remove(&key).await;
    }

    Ok(MessageResponse::ok(""))
}

/// `PUT /api/account/changeemail` -> `RequestResponseOfBoolean`.
///
/// Re-authentication is mandatory in both modes so possession of a session JWT
/// alone cannot redirect password-recovery mail and make a theft permanent.
pub async fn change_email(
    State(st): State<SharedState>,
    user: CurrentUser,
    Json(model): Json<MailChangeModel>,
) -> AppResult<Response> {
    let new_mail = model.new_mail.trim().to_lowercase();
    if new_mail.len() > MAX_EMAIL_BYTES || !new_mail.contains('@') {
        return Err(AppError::bad_request("Invalid email address"));
    }
    if !verify_email_domain(&new_mail, &load_email_domain_list(&st).await?) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let norm = new_mail.to_uppercase();

    let current = load_user(&st, user.id).await?;
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
    let expected_stamp = current
        .security_stamp
        .clone()
        .filter(|stamp| !stamp.is_empty())
        .ok_or(AppError::Unauthorized)?;
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
        let sender = crate::services::mail::MailSender::from_env();
        if !sender.is_configured() {
            return Err(AppError::bad_request(
                "Email confirmation is required but SMTP is not configured",
            ));
        }
        let token = crate::utils::codec::random_token(32);
        let key = format!("emailchange:{token}");
        let current_key = format!("emailchange-current:{}", user.id);
        if let Some(previous) = st.cache.get(&current_key).await {
            if let Ok(previous) = std::str::from_utf8(&previous) {
                st.cache.remove(&format!("emailchange:{previous}")).await;
            }
        }
        let ticket = EmailChangeTicket {
            user_id: user.id,
            new_email: new_mail.clone(),
            security_stamp: expected_stamp.clone(),
        };
        let bytes = serde_json::to_vec(&ticket)
            .map_err(|e| AppError::internal(format!("email-change ticket: {e}")))?;
        st.cache.set(&key, &bytes, Some(RECOVERY_TTL)).await;
        st.cache
            .set(&current_key, token.as_bytes(), Some(RECOVERY_TTL))
            .await;

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
        if let Err(error) = sender.send(&new_mail, &subject, &body).await {
            st.cache.remove(&key).await;
            st.cache
                .compare_and_remove(&current_key, token.as_bytes())
                .await;
            return Err(error);
        }
    } else {
        let new_stamp = Uuid::new_v4().to_string();
        match update_email_serialized(
            st.pg(),
            user.id,
            &expected_stamp,
            &new_mail,
            &norm,
            new_stamp.clone(),
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
    let key = format!("emailchange:{}", model.token);
    let ticket_bytes = st
        .cache
        .get(&key)
        .await
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
    let ticket: EmailChangeTicket = serde_json::from_slice(&ticket_bytes)
        .map_err(|_| AppError::bad_request("Invalid or expired email-change token"))?;
    let supplied_email = crate::utils::codec::base64_decode(&model.email)
        .and_then(|b| String::from_utf8(b).ok())
        .filter(|email| email.len() <= MAX_EMAIL_BYTES)
        .map(|email| email.trim().to_lowercase())
        .ok_or_else(|| AppError::bad_request("Invalid email"))?;
    if supplied_email != ticket.new_email {
        return Err(AppError::bad_request("Invalid email"));
    }
    if !verify_email_domain(&ticket.new_email, &load_email_domain_list(&st).await?) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let normalized = ticket.new_email.to_uppercase();
    if user::Entity::find()
        .filter(user::Column::NormalizedEmail.eq(normalized.clone()))
        .filter(user::Column::Id.ne(ticket.user_id))
        .one(&st.db)
        .await?
        .is_some()
    {
        return Err(AppError::conflict("Email already registered"));
    }
    let current = user::Entity::find_by_id(ticket.user_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
    if current.security_stamp.as_deref() != Some(ticket.security_stamp.as_str()) {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    }

    let current_key = format!("emailchange-current:{}", ticket.user_id);
    if !st
        .cache
        .compare_and_remove(&current_key, model.token.as_bytes())
        .await
    {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    }
    let consumed = st
        .cache
        .get_and_remove(&key)
        .await
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-change token"))?;
    if consumed != ticket_bytes {
        return Err(AppError::bad_request(
            "Invalid or expired email-change token",
        ));
    }

    let name = current.user_name.clone().unwrap_or_default();
    match update_email_serialized(
        st.pg(),
        ticket.user_id,
        &ticket.security_stamp,
        &ticket.new_email,
        &normalized,
        Uuid::new_v4().to_string(),
    )
    .await?
    {
        EmailUpdateOutcome::Updated => {}
        EmailUpdateOutcome::Conflict => {
            return Err(AppError::conflict("Email already registered"));
        }
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
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn unknown_login_uses_a_valid_dummy_argon2_hash() {
        assert!(argon2::PasswordHash::new(DUMMY_PASSWORD_HASH).is_ok());
        assert!(!crate::utils::crypto_utils::verify_password(
            "any submitted password",
            DUMMY_PASSWORD_HASH,
        ));
    }

    #[test]
    fn email_domain_validation_requires_one_complete_address() {
        assert!(verify_email_domain("user@allowed.test", "allowed.test"));
        assert!(verify_email_domain("user@allowed.test", "ALLOWED.TEST"));
        assert!(!verify_email_domain(
            "user@allowed.test@evil.test",
            "allowed.test"
        ));
        assert!(!verify_email_domain("@allowed.test", "allowed.test"));
        assert!(!verify_email_domain("user@", ""));
    }

    #[test]
    fn email_change_ticket_is_bound_to_the_security_stamp() {
        let ticket = EmailChangeTicket {
            user_id: Uuid::nil(),
            new_email: "new@example.test".to_string(),
            security_stamp: "stamp-1".to_string(),
        };
        let encoded = serde_json::to_vec(&ticket).unwrap();
        let decoded: EmailChangeTicket = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.security_stamp, "stamp-1");
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn email_change_rechecks_identity_after_a_registration_lock_wait() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("rsctf_email_identity_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY,
                 email TEXT,
                 normalized_email TEXT,
                 email_confirmed BOOLEAN NOT NULL DEFAULT FALSE,
                 security_stamp TEXT
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let changer = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id, email, normalized_email, email_confirmed, security_stamp)
               VALUES ($1, 'old@example.test', 'OLD@EXAMPLE.TEST', TRUE, 'stamp-old')"#,
        )
        .bind(changer)
        .execute(&pool)
        .await
        .unwrap();

        // Model a public/OAuth/admin registration that selected the requested
        // email while holding the shared identity lock but has not committed.
        let mut registration = crate::utils::database::begin_sqlx_transaction(&pool)
            .await
            .unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(REGISTRATION_LOCK_ID)
            .execute(&mut *registration)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id, email, normalized_email, email_confirmed, security_stamp)
               VALUES ($1, 'claimed@example.test', 'CLAIMED@EXAMPLE.TEST', TRUE, 'stamp-owner')"#,
        )
        .bind(Uuid::new_v4())
        .execute(&mut *registration)
        .await
        .unwrap();

        let contender = tokio::spawn({
            let pool = pool.clone();
            async move {
                update_email_serialized(
                    &pool,
                    changer,
                    "stamp-old",
                    "claimed@example.test",
                    "CLAIMED@EXAMPLE.TEST",
                    "stamp-new".to_string(),
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        registration.commit().await.unwrap();

        assert_eq!(
            contender.await.unwrap().unwrap(),
            EmailUpdateOutcome::Conflict
        );
        let changer_identity: (Option<String>, Option<String>) = sqlx::query_as(
            r#"SELECT normalized_email, security_stamp
                 FROM "AspNetUsers" WHERE id = $1"#,
        )
        .bind(changer)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            changer_identity,
            (Some("OLD@EXAMPLE.TEST".into()), Some("stamp-old".into()))
        );
        let owners: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::bigint FROM "AspNetUsers"
                WHERE normalized_email = 'CLAIMED@EXAMPLE.TEST'"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(owners, 1);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
