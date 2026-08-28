//! Stateless registration email-confirmation tokens and idempotent resend.

use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use super::*;

const EMAIL_CONFIRMATION_TTL_SECS: i64 = 15 * 60;
const LEGACY_TOKEN_VERSION: u8 = 1;
const TOKEN_VERSION: u8 = 2;
const LEGACY_TOKEN_PAYLOAD_BYTES: usize = 1 + 16 + 8 + 32 + 32;
const TOKEN_PAYLOAD_BYTES: usize = LEGACY_TOKEN_PAYLOAD_BYTES + 16;
const LEGACY_TOKEN_DOMAIN: &[u8] = b"rsctf-email-confirmation-v1\0";
const TOKEN_DOMAIN: &[u8] = b"rsctf-email-confirmation-v2\0";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfirmationClaims {
    user_id: Uuid,
    expires_at_unix: i64,
    email_hash: [u8; 32],
    security_stamp_hash: [u8; 32],
    operation_id: Option<Uuid>,
}

pub(super) struct PendingConfirmation<'a> {
    pub user_id: Uuid,
    pub user_name: &'a str,
    pub normalized_user_name: &'a str,
    pub email: &'a str,
    pub normalized_email: &'a str,
    pub password_hash: &'a str,
    pub security_stamp: &'a str,
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

fn issue_token(
    secret: &[u8],
    user_id: Uuid,
    normalized_email: &str,
    security_stamp: &str,
    expires_at_unix: i64,
    operation_id: Uuid,
) -> String {
    let mut payload = Vec::with_capacity(TOKEN_PAYLOAD_BYTES);
    payload.push(TOKEN_VERSION);
    payload.extend_from_slice(user_id.as_bytes());
    payload.extend_from_slice(&expires_at_unix.to_be_bytes());
    payload.extend_from_slice(&digest(normalized_email.as_bytes()));
    payload.extend_from_slice(&digest(security_stamp.as_bytes()));
    payload.extend_from_slice(operation_id.as_bytes());
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(TOKEN_DOMAIN);
    mac.update(&payload);
    let signature = mac.finalize().into_bytes();
    let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!("{}.{}", encoder.encode(payload), encoder.encode(signature))
}

fn verify_token(
    secret: &[u8],
    token: &str,
    normalized_email: &str,
) -> AppResult<ConfirmationClaims> {
    let (encoded_payload, encoded_signature) = token
        .split_once('.')
        .ok_or_else(|| AppError::bad_request("Invalid or expired email-confirmation token"))?;
    let decoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = decoder
        .decode(encoded_payload)
        .map_err(|_| AppError::bad_request("Invalid or expired email-confirmation token"))?;
    let signature = decoder
        .decode(encoded_signature)
        .map_err(|_| AppError::bad_request("Invalid or expired email-confirmation token"))?;
    let (domain, operation_id) = match (payload.first().copied(), payload.len()) {
        (Some(TOKEN_VERSION), TOKEN_PAYLOAD_BYTES) => (
            TOKEN_DOMAIN,
            Some(Uuid::from_slice(&payload[89..105]).map_err(|_| {
                AppError::bad_request("Invalid or expired email-confirmation token")
            })?),
        ),
        (Some(LEGACY_TOKEN_VERSION), LEGACY_TOKEN_PAYLOAD_BYTES) => (LEGACY_TOKEN_DOMAIN, None),
        _ => {
            return Err(AppError::bad_request(
                "Invalid or expired email-confirmation token",
            ));
        }
    };
    if signature.len() != 32 {
        return Err(AppError::bad_request(
            "Invalid or expired email-confirmation token",
        ));
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(domain);
    mac.update(&payload);
    mac.verify_slice(&signature)
        .map_err(|_| AppError::bad_request("Invalid or expired email-confirmation token"))?;

    let user_id = Uuid::from_slice(&payload[1..17])
        .map_err(|_| AppError::bad_request("Invalid or expired email-confirmation token"))?;
    let expires_at_unix = i64::from_be_bytes(
        payload[17..25]
            .try_into()
            .map_err(|_| AppError::bad_request("Invalid or expired email-confirmation token"))?,
    );
    let email_hash: [u8; 32] = payload[25..57]
        .try_into()
        .map_err(|_| AppError::bad_request("Invalid or expired email-confirmation token"))?;
    let security_stamp_hash: [u8; 32] = payload[57..89]
        .try_into()
        .map_err(|_| AppError::bad_request("Invalid or expired email-confirmation token"))?;
    if email_hash != digest(normalized_email.as_bytes()) {
        return Err(AppError::bad_request("Invalid email"));
    }
    Ok(ConfirmationClaims {
        user_id,
        expires_at_unix,
        email_hash,
        security_stamp_hash,
        operation_id,
    })
}

fn encoded_email(email: &str) -> String {
    crate::utils::codec::base64_encode(email.as_bytes())
        .replace('+', "%2B")
        .replace('/', "%2F")
        .replace('=', "%3D")
}

pub(super) fn token_for_registration(
    config: &crate::models::internal::configs::AppConfig,
    user_id: Uuid,
    normalized_email: &str,
    security_stamp: &str,
    database_now: chrono::DateTime<Utc>,
    operation_id: Uuid,
) -> String {
    issue_token(
        config.jwt_secret.as_bytes(),
        user_id,
        normalized_email,
        security_stamp,
        database_now.timestamp() + EMAIL_CONFIRMATION_TTL_SECS,
        operation_id,
    )
}

pub(super) fn require_delivery_origin(
    config: &crate::models::internal::configs::AppConfig,
) -> AppResult<&str> {
    config
        .public_url
        .as_deref()
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .ok_or_else(|| {
            AppError::bad_request("Email confirmation requires a canonical RSCTF_PUBLIC_URL")
        })
}

pub(super) async fn enqueue_confirmation(
    st: &SharedState,
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation_id: Uuid,
    account_id: Uuid,
    security_generation: &str,
    email: &str,
    token: &str,
    source: Option<&str>,
) -> AppResult<()> {
    let base = require_delivery_origin(st.config.as_ref())?.trim_end_matches('/');
    let link = format!(
        "{base}/account/verify?token={token}&email={}",
        encoded_email(email)
    );
    let (subject, body) =
        crate::services::mail::confirm_email(email, &link, Some(st.config.global.title.as_str()));
    crate::services::mail_outbox::enqueue_in_transaction(
        transaction,
        crate::services::mail_outbox::MailIntent {
            operation_id,
            purpose: crate::services::mail_outbox::MailPurpose::RegistrationConfirmation,
            account_id,
            security_generation,
            destination: email,
            source,
            subject: &subject,
            html_body: &body,
        },
    )
    .await?;
    Ok(())
}

/// Authenticate a repeated registration for the same pending identity and
/// mint a fresh stateless link. No session or identity observation is created.
pub(super) async fn resend_pending_confirmation(
    st: &SharedState,
    pending: PendingConfirmation<'_>,
    captcha: crate::services::captcha::CaptchaAdmission,
    operation_id: Uuid,
    source: Option<&str>,
) -> AppResult<()> {
    require_delivery_origin(st.config.as_ref())?;
    crate::services::mail::validate_recipient(pending.email)?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let policy =
        anti_cheat::lock_and_load_account_policy(&mut transaction, st.config.as_ref()).await?;
    policy.authorize_captcha(captcha)?;
    if !policy.email_confirmation_required
        || !verify_email_domain(pending.email, &policy.email_domain_list)
    {
        return Err(AppError::conflict("Username or email already registered"));
    }
    let locked: Option<(String,)> = sqlx::query_as(
        r#"SELECT security_stamp
             FROM "AspNetUsers"
            WHERE id = $1
              AND normalized_user_name = $2
              AND normalized_email = $3
              AND password_hash = $4
              AND security_stamp = $5
              AND email_confirmed = FALSE
              AND role = $6
            FOR SHARE"#,
    )
    .bind(pending.user_id)
    .bind(pending.normalized_user_name)
    .bind(pending.normalized_email)
    .bind(pending.password_hash)
    .bind(pending.security_stamp)
    .bind(Role::User as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if locked.is_none() {
        return Err(AppError::conflict("Username or email already registered"));
    }
    let database_now: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let token = token_for_registration(
        st.config.as_ref(),
        pending.user_id,
        pending.normalized_email,
        pending.security_stamp,
        database_now,
        operation_id,
    );
    link_attempts::stage_registration(
        &mut transaction,
        &token,
        pending.user_id,
        &link_attempts::value_digest(pending.security_stamp),
        &link_attempts::value_digest(pending.normalized_email),
        database_now + chrono::Duration::seconds(EMAIL_CONFIRMATION_TTL_SECS),
    )
    .await?;
    link_attempts::activate_registration_locked(&mut transaction, &token, pending.user_id).await?;
    enqueue_confirmation(
        st,
        &mut transaction,
        operation_id,
        pending.user_id,
        pending.security_stamp,
        pending.email,
        &token,
        source,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::services::audit::info(
        st,
        "AccountController",
        Some(pending.user_name.to_string()),
        None,
        format!(
            "User {} requested a new email confirmation",
            pending.user_name
        ),
    )
    .await;
    Ok(())
}

fn decode_supplied_email(encoded: &str) -> AppResult<(String, String)> {
    let email = crate::utils::codec::base64_decode(encoded)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|email| email.len() <= MAX_EMAIL_BYTES)
        .map(|email| email.trim().to_lowercase())
        .filter(|email| email.contains('@'))
        .ok_or_else(|| AppError::bad_request("Invalid email"))?;
    let normalized = email.to_uppercase();
    Ok((email, normalized))
}

async fn confirm_token(
    pool: &sqlx::PgPool,
    config: &crate::models::internal::configs::AppConfig,
    model: &AccountVerifyModel,
) -> AppResult<String> {
    if model.token.is_empty()
        || model.token.len() > MAX_ACCOUNT_TOKEN_BYTES
        || model.email.is_empty()
        || model.email.len() > MAX_ENCODED_EMAIL_BYTES
    {
        return Err(AppError::bad_request(
            "Invalid or expired email-confirmation token",
        ));
    }
    let (email, normalized_email) = decode_supplied_email(&model.email)?;
    let claims = verify_token(
        config.jwt_secret.as_bytes(),
        &model.token,
        &normalized_email,
    )?;

    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let link = link_attempts::lock_registration(
        &mut transaction,
        &model.token,
        claims.user_id,
        &hex::encode(claims.security_stamp_hash),
        &hex::encode(claims.email_hash),
    )
    .await?;
    if let Some(name) = link.safe_result {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(name);
    }
    let policy = anti_cheat::lock_and_load_account_policy(&mut transaction, config).await?;
    if !verify_email_domain(&email, &policy.email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let database_now: i64 =
        sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM clock_timestamp())::BIGINT")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if database_now >= claims.expires_at_unix {
        return Err(AppError::bad_request(
            "Invalid or expired email-confirmation token",
        ));
    }
    if let Some(operation_id) = claims.operation_id {
        let is_current: bool = sqlx::query_scalar(
            r#"SELECT EXISTS (
                   SELECT 1
                     FROM "MailOutbox"
                    WHERE operation_id = $1
                      AND account_id = $2
                      AND purpose = $3
                      AND superseded_at_utc IS NULL
               )"#,
        )
        .bind(operation_id)
        .bind(claims.user_id)
        .bind(crate::services::mail_outbox::MailPurpose::RegistrationConfirmation as i16)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !is_current {
            return Err(AppError::bad_request(
                "Invalid or expired email-confirmation token",
            ));
        }
    }
    let locked: Option<(Option<String>, String)> = sqlx::query_as(
        r#"SELECT user_name, security_stamp
             FROM "AspNetUsers"
            WHERE id = $1
              AND normalized_email = $2
              AND email_confirmed = FALSE
              AND role <> $3
            FOR UPDATE"#,
    )
    .bind(claims.user_id)
    .bind(&normalized_email)
    .bind(Role::Banned as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((name, security_stamp)) = locked else {
        return Err(AppError::bad_request(
            "Invalid or expired email-confirmation token",
        ));
    };
    if digest(security_stamp.as_bytes()) != claims.security_stamp_hash {
        return Err(AppError::bad_request(
            "Invalid or expired email-confirmation token",
        ));
    }
    let new_stamp = Uuid::new_v4().to_string();
    let updated = sqlx::query(
        r#"UPDATE "AspNetUsers"
              SET email_confirmed = TRUE,
                  security_stamp = $1
            WHERE id = $2
              AND normalized_email = $3
              AND security_stamp = $4
              AND email_confirmed = FALSE
              AND role <> $5"#,
    )
    .bind(new_stamp)
    .bind(claims.user_id)
    .bind(&normalized_email)
    .bind(&security_stamp)
    .bind(Role::Banned as i16)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::bad_request(
            "Invalid or expired email-confirmation token",
        ));
    }
    let name = name.unwrap_or_default();
    link_attempts::complete(
        &mut transaction,
        &model.token,
        claims.user_id,
        "registration",
        &name,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(name)
}

/// `POST /api/account/verify` -> `void`.
pub async fn verify(
    State(st): State<SharedState>,
    Json(model): Json<AccountVerifyModel>,
) -> AppResult<MessageResponse> {
    let name = confirm_token(st.pg(), st.config.as_ref(), &model).await?;
    crate::services::audit::info(
        &st,
        "AccountController",
        Some(name.clone()),
        None,
        format!("User {name} verified email"),
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
    fn token_is_email_stamp_expiry_and_signature_bound() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let user_id = Uuid::new_v4();
        let operation_id = Uuid::new_v4();
        let token = issue_token(
            secret,
            user_id,
            "A@EXAMPLE.TEST",
            "stamp-a",
            12345,
            operation_id,
        );
        let claims = verify_token(secret, &token, "A@EXAMPLE.TEST").unwrap();
        assert_eq!(claims.user_id, user_id);
        assert_eq!(claims.expires_at_unix, 12345);
        assert_eq!(claims.security_stamp_hash, digest(b"stamp-a"));
        assert_eq!(claims.operation_id, Some(operation_id));
        assert!(verify_token(secret, &token, "B@EXAMPLE.TEST").is_err());
        let mut tampered = token.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        assert!(verify_token(
            secret,
            std::str::from_utf8(&tampered).unwrap(),
            "A@EXAMPLE.TEST"
        )
        .is_err());
    }

    #[test]
    fn deliberate_reissue_has_a_distinct_outbox_generation() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let user_id = Uuid::new_v4();
        let first = issue_token(
            secret,
            user_id,
            "A@EXAMPLE.TEST",
            "stamp-a",
            1000,
            Uuid::new_v4(),
        );
        let retry = issue_token(
            secret,
            user_id,
            "A@EXAMPLE.TEST",
            "stamp-a",
            1100,
            Uuid::new_v4(),
        );
        assert_ne!(first, retry);
        assert!(verify_token(secret, &first, "A@EXAMPLE.TEST").is_ok());
        assert!(verify_token(secret, &retry, "A@EXAMPLE.TEST").is_ok());
    }

    #[test]
    fn legacy_confirmation_links_remain_valid_during_rollout() {
        let secret = b"0123456789abcdef0123456789abcdef";
        let user_id = Uuid::new_v4();
        let mut payload = Vec::with_capacity(LEGACY_TOKEN_PAYLOAD_BYTES);
        payload.push(LEGACY_TOKEN_VERSION);
        payload.extend_from_slice(user_id.as_bytes());
        payload.extend_from_slice(&12345_i64.to_be_bytes());
        payload.extend_from_slice(&digest(b"A@EXAMPLE.TEST"));
        payload.extend_from_slice(&digest(b"stamp-a"));
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(LEGACY_TOKEN_DOMAIN);
        mac.update(&payload);
        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let token = format!(
            "{}.{}",
            encoder.encode(payload),
            encoder.encode(mac.finalize().into_bytes())
        );
        let claims = verify_token(secret, &token, "A@EXAMPLE.TEST").unwrap();
        assert_eq!(claims.user_id, user_id);
        assert_eq!(claims.operation_id, None);
    }

    #[test]
    fn confirmation_requires_a_canonical_public_origin() {
        let mut config = crate::models::internal::configs::AppConfig::from_env();
        config.public_url = None;
        assert!(require_delivery_origin(&config).is_err());
        config.public_url = Some("https://ctf.example".to_string());
        assert_eq!(
            require_delivery_origin(&config).unwrap(),
            "https://ctf.example"
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn failed_confirmation_commit_leaves_the_same_token_retryable() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("email_confirmation_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Configs" (
                 config_key TEXT PRIMARY KEY, value TEXT, cache_keys TEXT
               );
               CREATE TABLE "AspNetUsers" (
                 id UUID PRIMARY KEY,
                 user_name TEXT,
                 normalized_email TEXT,
                 email_confirmed BOOLEAN NOT NULL,
                 role SMALLINT NOT NULL,
                 security_stamp TEXT NOT NULL
               );
               CREATE TABLE "MailOutbox" (
                 operation_id UUID PRIMARY KEY,
                 account_id UUID NOT NULL,
                 purpose SMALLINT NOT NULL,
                 superseded_at_utc TIMESTAMPTZ
               );
               CREATE TABLE "AccountLinkAttempts" (
                 token_digest TEXT PRIMARY KEY,
                 purpose TEXT NOT NULL,
                 account_id UUID NOT NULL,
                 security_generation_digest TEXT NOT NULL,
                 destination_digest TEXT NOT NULL,
                 expires_at_utc TIMESTAMPTZ NOT NULL,
                 active BOOLEAN NOT NULL DEFAULT TRUE,
                 terminal_result SMALLINT,
                 safe_result TEXT,
                 issued_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                 issued_sequence BIGSERIAL NOT NULL,
                 completed_at_utc TIMESTAMPTZ
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let user_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "AspNetUsers"
                 (id,user_name,normalized_email,email_confirmed,role,security_stamp)
               VALUES ($1,'pending','PENDING@EXAMPLE.TEST',FALSE,$2,'stamp-a')"#,
        )
        .bind(user_id)
        .bind(Role::User as i16)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            r#"CREATE FUNCTION reject_confirmation_commit()
               RETURNS trigger LANGUAGE plpgsql AS $$
               BEGIN
                 IF NEW.email_confirmed AND NOT OLD.email_confirmed THEN
                   RAISE EXCEPTION 'synthetic deferred confirmation failure';
                 END IF;
                 RETURN NEW;
               END $$;
               CREATE CONSTRAINT TRIGGER reject_confirmation_commit
                 AFTER UPDATE ON "AspNetUsers"
                 DEFERRABLE INITIALLY DEFERRED
                 FOR EACH ROW EXECUTE FUNCTION reject_confirmation_commit();"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let mut config = crate::models::internal::configs::AppConfig::from_env();
        config.jwt_secret = "0123456789abcdef0123456789abcdef".to_string();
        config.account.use_captcha = false;
        let operation_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "MailOutbox"
                 (operation_id,account_id,purpose,superseded_at_utc)
               VALUES ($1,$2,$3,NULL)"#,
        )
        .bind(operation_id)
        .bind(user_id)
        .bind(crate::services::mail_outbox::MailPurpose::RegistrationConfirmation as i16)
        .execute(&pool)
        .await
        .unwrap();
        let token = issue_token(
            config.jwt_secret.as_bytes(),
            user_id,
            "PENDING@EXAMPLE.TEST",
            "stamp-a",
            Utc::now().timestamp() + 3600,
            operation_id,
        );
        let model = AccountVerifyModel {
            token,
            email: crate::utils::codec::base64_encode(b"pending@example.test"),
        };
        let mut link_tx = pool.begin().await.unwrap();
        link_attempts::stage_registration(
            &mut link_tx,
            &model.token,
            user_id,
            &link_attempts::value_digest("stamp-a"),
            &link_attempts::value_digest("PENDING@EXAMPLE.TEST"),
            Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        link_attempts::activate_registration_locked(&mut link_tx, &model.token, user_id)
            .await
            .unwrap();
        link_tx.commit().await.unwrap();
        assert!(confirm_token(&pool, &config, &model).await.is_err());
        let after_failure: (bool, String) = sqlx::query_as(
            r#"SELECT email_confirmed,security_stamp
                 FROM "AspNetUsers" WHERE id=$1"#,
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after_failure, (false, "stamp-a".to_string()));

        sqlx::raw_sql(
            r#"DROP TRIGGER reject_confirmation_commit ON "AspNetUsers";
               DROP FUNCTION reject_confirmation_commit();"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            confirm_token(&pool, &config, &model).await.unwrap(),
            "pending"
        );
        assert_eq!(
            confirm_token(&pool, &config, &model).await.unwrap(),
            "pending"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
