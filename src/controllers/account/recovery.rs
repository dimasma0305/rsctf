//! Account recovery / email-verification / password-reset / mail-change confirm
//! — split from account/mod.rs to stay under the 1000-line rule.
use super::*;
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RECOVERY_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
const RECOVERY_RESPONSE_FLOOR: std::time::Duration = std::time::Duration::from_millis(25);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PasswordResetTicket {
    user_id: Uuid,
    security_stamp: String,
}

async fn stage_password_reset_ticket(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    outcome: crate::services::mail_outbox::EnqueueOutcome,
    token_hash: &[u8],
    ticket: &PasswordResetTicket,
) -> AppResult<bool> {
    if outcome == crate::services::mail_outbox::EnqueueOutcome::Replayed {
        return Ok(false);
    }

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
    Ok(true)
}

fn reset_current_key(user_id: Uuid) -> String {
    format!("pwreset-current:{user_id}")
}

fn email_change_token(
    secret: &[u8],
    operation_id: Uuid,
    account_id: Uuid,
    security_stamp: &str,
    normalized_email: &str,
) -> String {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(b"rsctf-email-change-v1\0");
    mac.update(operation_id.as_bytes());
    mac.update(account_id.as_bytes());
    mac.update(security_stamp.as_bytes());
    mac.update(normalized_email.as_bytes());
    hex::encode(mac.finalize().into_bytes())
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

#[derive(Debug, PartialEq, Eq)]
enum EmailUpdateOutcome {
    Updated,
    Conflict,
    StampMismatch,
}

struct EmailUpdateRequest<'a> {
    user_id: Uuid,
    expected_stamp: &'a str,
    email: &'a str,
    normalized_email: &'a str,
    new_stamp: String,
}

/// Commit an email identity change under the same cross-replica lock used by
/// password registration, OAuth provisioning, and admin identity writers.
/// `normalized_email` is not protected by a database unique constraint on
/// existing installations, so the in-lock recheck is the authoritative guard;
/// any earlier handler-level lookup is only a fast failure path.
async fn update_email_serialized(
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

    // REGISTRATION_LOCK_ID precedes the shared account-policy lock everywhere
    // a user identity can be created or changed. Revalidate the domain and
    // confirmation mode here, after any admin policy update we waited behind.
    let policy =
        crate::services::anti_cheat::lock_and_load_account_policy(&mut transaction, config).await?;
    if !verify_email_domain(request.email, &policy.email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    if policy.email_confirmation_required {
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
    let account_scope = format!("password-change:{}:{}", user.id, user.security_stamp);
    let source =
        anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_else(|| peer.ip().to_string());
    let source_scope = format!("credential-source:{source}");
    let _credential_work = crate::services::credential_admission::try_acquire_scopes(
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
    let old_hash = current.password_hash.clone().unwrap_or_default();
    if model.old.len() > MAX_PASSWORD_BYTES {
        return Err(AppError::bad_request("Old password is incorrect"));
    }
    if !verify_password_async(model.old, old_hash.clone()).await {
        return Err(AppError::bad_request("Old password is incorrect"));
    }
    let new_hash = hash_password_async(model.new).await?;
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
            &st.config.jwt_secret,
        )
        .await?;
    crate::services::anti_cheat::authorize_captcha_admission(
        st.pg(),
        st.config.as_ref(),
        captcha_admission,
    )
    .await?;

    let response_started = tokio::time::Instant::now();
    let operation_id = model.operation_id.unwrap_or_else(Uuid::now_v7);
    let request_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()));
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
        let token = crate::utils::codec::random_token(32);
        let key = format!("pwreset:{token}");
        let current_key = reset_current_key(user.id);
        let ticket = PasswordResetTicket {
            user_id: user.id,
            security_stamp: user.security_stamp.clone().unwrap_or_default(),
        };
        let token_hash = Sha256::digest(token.as_bytes()).to_vec();
        let cached_ticket = serde_json::to_vec(&ticket)
            .map_err(|e| AppError::internal(format!("password-reset ticket: {e}")))?;
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
        let (subject, body) =
            crate::services::mail::reset_password(&link, Some(st.config.global.title.as_str()));

        // The database operation lock coalesces the same operation across
        // replicas before its cache generation is changed. No SMTP work occurs
        // here, and every failure retains the anonymous success response.
        let mut transaction = match st.pg().begin().await {
            Ok(transaction) => transaction,
            Err(error) => {
                tracing::error!(%error, operation_id = %operation_id, "password-recovery mail transaction failed");
                if let Some(remaining) =
                    RECOVERY_RESPONSE_FLOOR.checked_sub(response_started.elapsed())
                {
                    tokio::time::sleep(remaining).await;
                }
                return Ok(MessageResponse::ok(
                    "If that email is registered, a password reset link has been sent.",
                ));
            }
        };
        let outcome = crate::services::mail_outbox::enqueue_in_transaction(
            &mut transaction,
            crate::services::mail_outbox::MailIntent {
                operation_id,
                purpose: crate::services::mail_outbox::MailPurpose::PasswordRecovery,
                account_id: user.id,
                security_generation: user.security_stamp.as_deref().unwrap_or_default(),
                destination: &user_email,
                source: request_ip.as_deref(),
                subject: &subject,
                html_body: &body,
            },
        )
        .await;
        match outcome {
            Ok(outcome) => {
                match stage_password_reset_ticket(&mut transaction, outcome, &token_hash, &ticket)
                    .await
                {
                    Ok(true) => match transaction.commit().await {
                        Ok(()) => {
                            invalidate_password_reset_tokens(&st, user.id).await;
                            st.cache.set(&key, &cached_ticket, Some(RECOVERY_TTL)).await;
                            st.cache
                                .set(&current_key, token.as_bytes(), Some(RECOVERY_TTL))
                                .await;
                        }
                        Err(error) => {
                            tracing::error!(%error, operation_id = %operation_id, "password-recovery mail commit failed");
                        }
                    },
                    Ok(false) => {
                        if let Err(error) = transaction.commit().await {
                            tracing::error!(%error, operation_id = %operation_id, "password-recovery replay commit failed");
                        }
                    }
                    Err(error) => {
                        let _ = transaction.rollback().await;
                        tracing::error!(%error, operation_id = %operation_id, "password-recovery ticket staging failed");
                    }
                }
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                tracing::warn!(%error, operation_id = %operation_id, "password-recovery mail intent was not admitted");
            }
        }
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(model): Json<PasswordResetModel>,
) -> AppResult<Response> {
    if model.operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A valid password reset operation ID is required",
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
    let mut digest_builder =
        <Hmac<Sha256> as KeyInit>::new_from_slice(st.config.jwt_secret.as_bytes())
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
        // One-release compatibility for reset links minted before the durable
        // ticket migration. Promote a still-live cache ticket before use.
        let bytes = st
            .cache
            .get(&key)
            .await
            .ok_or_else(|| AppError::bad_request("Invalid or expired reset token"))?;
        let ticket: PasswordResetTicket = serde_json::from_slice(&bytes)
            .map_err(|_| AppError::bad_request("Invalid or expired reset token"))?;
        sqlx::query(
            r#"INSERT INTO "PasswordResetTickets"
                   (token_hash, user_id, security_stamp, expires_at_utc)
               VALUES ($1, $2, $3, clock_timestamp() + INTERVAL '15 minutes')
               ON CONFLICT (token_hash) DO NOTHING"#,
        )
        .bind(&token_hash)
        .bind(ticket.user_id)
        .bind(&ticket.security_stamp)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        ticket
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
    let account_scope = format!("password-reset:{}", hex::encode(&token_hash));
    let source =
        anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_else(|| peer.ip().to_string());
    let source_scope = format!("credential-source:{source}");
    let _credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::Interactive,
        &[&account_scope, &source_scope],
    )
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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    user: CurrentUser,
    Json(model): Json<MailChangeModel>,
) -> AppResult<Response> {
    let operation_id = model.operation_id.unwrap_or_else(Uuid::now_v7);
    let request_ip = crate::services::anti_cheat::client_ip(&headers, Some(peer.ip()));
    let new_mail = model.new_mail.trim().to_lowercase();
    if new_mail.len() > MAX_EMAIL_BYTES || !new_mail.contains('@') {
        return Err(AppError::bad_request("Invalid email address"));
    }
    let account_scope = format!(
        "email-change:{}:{}:{}",
        user.id,
        user.security_stamp,
        new_mail.to_uppercase()
    );
    let source =
        anti_cheat::client_ip(&headers, Some(peer.ip())).unwrap_or_else(|| peer.ip().to_string());
    let source_scope = format!("credential-source:{source}");
    let _credential_work = crate::services::credential_admission::try_acquire_scopes(
        st.pg(),
        crate::services::credential_admission::CredentialWorkClass::Interactive,
        &[&account_scope, &source_scope],
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
        let token = email_change_token(
            st.config.jwt_secret.as_bytes(),
            operation_id,
            user.id,
            &expected_stamp,
            &norm,
        );

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
        link_attempts::stage_email_change(
            &mut transaction,
            &token,
            user.id,
            &expected_stamp,
            &norm,
            Utc::now() + chrono::Duration::from_std(RECOVERY_TTL).unwrap_or_default(),
        )
        .await?;
        link_attempts::activate_email_change_locked(&mut transaction, &token, user.id).await?;
        crate::services::mail_outbox::enqueue_in_transaction(
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

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
