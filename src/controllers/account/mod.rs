//! Ported from RSCTF `Controllers/AccountController.cs`.
//!
//! Compatibility implementation of the `/api/account/*` surface: paths,
//! camelCase DTO fields, and success envelopes match `web/src/Api.ts`. The SPA authenticates with a
//! same-origin rsctf session cookie, so `login`/`register` set it via
//! `Set-Cookie` and `logout` clears it.

use crate::middlewares::rate_limiter::{limited, Policy};
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, EntityTrait,
    PaginatorTrait, QueryFilter, Set,
};
use serde::Serialize;
use std::net::SocketAddr;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{
    clear_session_cookie, set_session_cookie, CurrentUser, MaybeUser,
};
use crate::models::data::{config, game_manager, user};
use crate::models::request::account::*;
use crate::services::anti_cheat;
use crate::services::captcha::CaptchaSettings;
use crate::utils::crypto_utils::{hash_password_async, verify_password_async};
use crate::utils::enums::{RegisterStatus, Role};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{MessageResponse, RequestResponse, Wrapped};

const MAX_AVATAR_BYTES: usize = crate::utils::upload::IMAGE_FILE_BYTES;
pub(crate) const MAX_PASSWORD_BYTES: usize = 1_024;
pub(crate) const MAX_EMAIL_BYTES: usize = 320;
pub(crate) const MAX_USER_NAME_BYTES: usize = 128;
pub(super) const MAX_ACCOUNT_TOKEN_BYTES: usize = 256;
pub(super) const MAX_ENCODED_EMAIL_BYTES: usize = 1_024;
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$YBSHJA9ANNWFII7EsOe1rw$O5h6h9EwR/6Pyoe9wCcjK91HivbrgJZwb44fhsiqonw";
pub(crate) const REGISTRATION_LOCK_ID: i64 = 0x5253_4354_4652_4547; // "RSCTFREG"

fn registration_disposition(
    is_first: bool,
    active_on_register: bool,
    email_confirmation_required: bool,
) -> (bool, RegisterStatus) {
    let session_eligible = is_first || (active_on_register && !email_confirmation_required);
    let status = if session_eligible {
        RegisterStatus::LoggedIn
    } else if email_confirmation_required {
        RegisterStatus::EmailConfirmationRequired
    } else {
        RegisterStatus::AdminConfirmationRequired
    };
    (session_eligible, status)
}

mod avatar;
mod bootstrap;
mod email_confirmation;
mod link_attempts;
mod profile_bounds;
mod recovery;
mod request_models;
mod stats;
pub use avatar::avatar;
pub use email_confirmation::verify;
use profile_bounds::load_user;
pub(crate) use profile_bounds::validate_profile_fields;
pub use recovery::*;
pub use request_models::{
    AccountVerifyModel, LoginModel, MailChangeModel, PasswordResetModel, RecoveryModel,
};
pub use stats::*;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/account/avatar",
            put(avatar).layer(DefaultBodyLimit::max(
                crate::utils::upload::IMAGE_BODY_BYTES,
            )),
        )
        .route(
            "/api/account/changeemail",
            limited(
                Policy::Register,
                limited(Policy::CredentialMutation, put(change_email)),
            ),
        )
        .route(
            "/api/account/changepassword",
            limited(Policy::CredentialMutation, put(change_password)),
        )
        .route(
            "/api/account/fingerprintchallenge",
            limited(Policy::Register, get(fingerprint_challenge)),
        )
        .route("/api/account/login", limited(Policy::Login, post(login)))
        .route("/api/account/logout", post(logout))
        .route(
            "/api/account/mailchangeconfirm",
            limited(Policy::Register, post(mail_change_confirm)),
        )
        .route(
            "/api/account/passwordreset",
            limited(Policy::CredentialMutation, post(password_reset)),
        )
        .route("/api/account/profile", get(profile))
        .route("/api/account/stats", get(stats))
        .route(
            "/api/account/recovery",
            limited(Policy::Register, post(recovery)),
        )
        .route(
            "/api/account/register",
            limited(Policy::Register, post(register)),
        )
        .route("/api/account/update", put(update))
        .route("/api/account/verify", post(verify))
}

// ---------------------------------------------------------------------------
// Local response DTOs (camelCase; must match Api.ts interfaces exactly).
// ---------------------------------------------------------------------------

/// `BrowserFingerprintChallengeModel`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserFingerprintChallengeModel {
    pub nonce: String,
    pub required_signals: Vec<String>,
    pub expires_in_seconds: i32,
}

/// `ProfileUserInfoModel` — the `Profile` view of a `UserInfo`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUserInfoModel {
    pub user_id: Uuid,
    pub role: Role,
    pub user_name: Option<String>,
    pub email: Option<String>,
    pub bio: Option<String>,
    pub phone: Option<String>,
    pub real_name: Option<String>,
    pub std_number: Option<String>,
    pub avatar: Option<String>,
    pub has_managed_games: bool,
}

impl ProfileUserInfoModel {
    fn from_user(u: &user::Model, has_managed_games: bool) -> Self {
        Self {
            user_id: u.id,
            role: u.role,
            user_name: u.user_name.clone(),
            email: u.email.clone(),
            bio: Some(u.bio.clone()),
            phone: u.phone_number.clone(),
            real_name: Some(u.real_name.clone()),
            std_number: Some(u.std_number.clone()),
            avatar: u.avatar_url(),
            has_managed_games,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Compatibility transaction helper for account/admin identity mutations that
/// still use SeaORM. New identity-admission writes use the underlying sqlx pool.
pub(crate) async fn locked_registration_transaction(
    st: &SharedState,
) -> AppResult<DatabaseTransaction> {
    let transaction = crate::utils::database::begin_seaorm_transaction(&st.db).await?;
    transaction
        .execute_unprepared(&format!(
            "SELECT pg_advisory_xact_lock({REGISTRATION_LOCK_ID})"
        ))
        .await?;
    Ok(transaction)
}

/// `POST /api/account/register` -> `RequestResponseOfRegisterStatus`.
///
/// Creates the account and, when no confirmation gate is configured, logs the
/// user straight in by issuing a session cookie.
pub async fn register(
    State(st): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(model): Json<RegisterModel>,
) -> AppResult<Response> {
    let mail_operation_id = model.operation_id.unwrap_or_else(Uuid::now_v7);
    let request_ip = anti_cheat::client_ip(&headers, Some(peer.ip()));
    // Fail fast before policy loading, captcha verification, and Argon2.
    let is_first_preflight = bootstrap::preflight(&st, model.bootstrap_token.as_deref()).await?;
    // Load the live AccountPolicy from the `Configs` key/value table so the
    // /admin/config toggles take effect per-request (RSCTF reads AccountPolicy
    // from an IOptionsSnapshot backed by the DB). Each key falls back to the
    // startup env-loaded `st.config.account` when it was never persisted —
    // mirrors InfoController reading GlobalConfig from `config::Entity`.
    // Comma-separated domain allowlist; empty = allow all (RSCTF EmailDomainList).
    let mut email_domain_list = String::new();
    for row in config::Entity::find().all(&st.db).await? {
        let Some(value) = row.value else { continue };
        if row.config_key == "AccountPolicy:EmailDomainList" {
            email_domain_list = value;
        }
    }

    // The canonical transaction below applies the new-account registration
    // gate. Deferring the fast rejection lets an already-created, unconfirmed
    // account authenticate a resend even if public registration was disabled
    // after its original request.

    // Apply the live captcha policy before creating the account; captcha-off
    // registration remains unaffected.
    let captcha = CaptchaSettings::load(st.pg(), st.config.account.use_captcha).await?;
    let captcha_admission = captcha
        .verify_local(
            model.challenge.as_deref().unwrap_or(""),
            st.cache.as_ref(),
            &st.config.jwt_secret,
        )
        .await?;

    let user_name = model.user_name.trim().to_string();
    if user_name.len() < 3 {
        return Err(AppError::bad_request(
            "Username must be at least 3 characters",
        ));
    }
    if user_name.len() > MAX_USER_NAME_BYTES {
        return Err(AppError::bad_request("Username is too long"));
    }
    if model.password.len() < 6 {
        return Err(AppError::bad_request(
            "Password must be at least 6 characters",
        ));
    }
    validate_password(&model.password)?;
    let email = model.email.trim().to_lowercase();
    if email.len() > MAX_EMAIL_BYTES {
        return Err(AppError::bad_request("Invalid email address"));
    }
    crate::services::mail::validate_recipient(&email)?;
    // Enforce the EmailDomainList allowlist (RSCTF VerifyEmailDomain): a non-empty
    // list rejects addresses whose domain is not in it. Same 400 RSCTF returns.
    if !verify_email_domain(&email, &email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }

    let norm_name = user_name.to_uppercase();
    let norm_email = email.to_uppercase();

    type PendingRow = (
        Uuid,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        i16,
        Option<String>,
        Option<String>,
    );
    let collisions: Vec<PendingRow> = sqlx::query_as(
        r#"SELECT id, user_name, normalized_user_name, email, normalized_email,
                  email_confirmed, role, password_hash, security_stamp
             FROM "AspNetUsers"
            WHERE normalized_user_name = $1 OR normalized_email = $2
            ORDER BY id"#,
    )
    .bind(&norm_name)
    .bind(&norm_email)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !collisions.is_empty() {
        if let [pending] = collisions.as_slice() {
            let exact_identity = pending.2.as_deref() == Some(norm_name.as_str())
                && pending.4.as_deref() == Some(norm_email.as_str());
            let pending_user = !pending.5 && pending.6 == Role::User as i16;
            if exact_identity && pending_user {
                if let (Some(password_hash), Some(security_stamp)) = (&pending.7, &pending.8) {
                    if verify_password_async(model.password.clone(), password_hash.clone()).await? {
                        email_confirmation::resend_pending_confirmation(
                            &st,
                            email_confirmation::PendingConfirmation {
                                user_id: pending.0,
                                user_name: pending.1.as_deref().unwrap_or(&user_name),
                                normalized_user_name: pending.2.as_deref().unwrap_or(&norm_name),
                                email: pending.3.as_deref().unwrap_or(&email),
                                normalized_email: pending.4.as_deref().unwrap_or(&norm_email),
                                password_hash,
                                security_stamp,
                            },
                            captcha_admission,
                            mail_operation_id,
                            request_ip.as_deref(),
                        )
                        .await?;
                        return Ok(
                            Wrapped::ok(RegisterStatus::EmailConfirmationRequired).into_response()
                        );
                    }
                }
            }
        }
        if collisions
            .iter()
            .any(|collision| collision.2.as_deref() == Some(norm_name.as_str()))
        {
            return Err(AppError::conflict("Username already taken"));
        }
        return Err(AppError::conflict("Email already registered"));
    }

    anti_cheat::preflight_password_registration(st.pg(), st.config.as_ref(), is_first_preflight)
        .await?;

    let identity_policy = anti_cheat::load_policy_flags(st.pg()).await?;
    let fingerprint = anti_cheat::validate_fingerprint_submission(
        &st,
        identity_policy,
        model.fingerprint.as_deref(),
        model.fingerprint_proof.as_deref(),
    )
    .await?;
    let current_ip = request_ip;

    let now = Utc::now();
    let id = Uuid::now_v7();
    let password_hash = hash_password_async(model.password.clone()).await?;
    let security_stamp = Uuid::new_v4().to_string();

    // Re-evaluate under the registration lock so only one caller can bootstrap.
    let mut txn = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_LOCK_ID)
        .execute(&mut *txn)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let is_first: bool = sqlx::query_scalar(r#"SELECT NOT EXISTS (SELECT 1 FROM "AspNetUsers")"#)
        .fetch_one(&mut *txn)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let account_policy =
        anti_cheat::lock_and_load_account_policy(&mut txn, st.config.as_ref()).await?;
    bootstrap::require(is_first, model.bootstrap_token.as_deref())?;
    if let Err(error) = account_policy.authorize_password_registration(is_first) {
        txn.rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(error);
    }
    account_policy.authorize_captcha(captcha_admission)?;
    if !verify_email_domain(&email, &account_policy.email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }
    let duplicate: Option<(bool, bool)> = sqlx::query_as(
        r#"SELECT normalized_user_name = $1, normalized_email = $2
             FROM "AspNetUsers"
            WHERE normalized_user_name = $1 OR normalized_email = $2
            LIMIT 1"#,
    )
    .bind(&norm_name)
    .bind(&norm_email)
    .fetch_optional(&mut *txn)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if duplicate.is_some_and(|row| row.0) {
        txn.rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict("Username already taken"));
    }
    if duplicate.is_some_and(|row| row.1) {
        txn.rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict("Email already registered"));
    }
    let role = if is_first { Role::Admin } else { Role::User };
    let (session_eligible, status) = registration_disposition(
        is_first,
        account_policy.active_on_register,
        account_policy.email_confirmation_required,
    );
    if status == RegisterStatus::EmailConfirmationRequired {
        email_confirmation::require_delivery_origin(st.config.as_ref())?;
    }
    let admission = if session_eligible {
        Some(
            anti_cheat::admit_new_user_in_transaction(
                &mut txn,
                st.config.as_ref(),
                id,
                Some(&user_name),
                current_ip.as_deref(),
                fingerprint.as_deref(),
                anti_cheat::IdentitySource::Registration,
            )
            .await?,
        )
    } else {
        // Pending accounts have authenticated nobody yet. Do not let an
        // unauthenticated registration reserve an IP/device identity.
        anti_cheat::mark_identity_neutral_insert(&mut txn).await?;
        None
    };
    let accepted_ip = if admission == Some(anti_cheat::AdmissionOutcome::Accepted) {
        current_ip.as_deref().unwrap_or("0.0.0.0")
    } else {
        "0.0.0.0"
    };

    let insert = sqlx::query(
        r#"INSERT INTO "AspNetUsers"
             (id, user_name, normalized_user_name, email, normalized_email,
              email_confirmed, password_hash, security_stamp, concurrency_stamp,
              phone_number, phone_number_confirmed, two_factor_enabled, lockout_end,
              lockout_enabled, access_failed_count, role, ip, browser_fingerprint,
              last_signed_in_utc, last_visited_utc, register_time_utc, bio,
              real_name, std_number, exercise_visible, avatar_hash)
           VALUES
             ($1, $2, $3, $4, $5, $6, $7, $8, $9,
              NULL, FALSE, FALSE, NULL, FALSE, 0, $10, $11, NULL,
              $12, $12, $12, '', '', '', TRUE, NULL)"#,
    )
    .bind(id)
    .bind(&user_name)
    .bind(&norm_name)
    .bind(&email)
    .bind(&norm_email)
    .bind(session_eligible)
    .bind(&password_hash)
    .bind(&security_stamp)
    .bind(Uuid::new_v4().to_string())
    .bind(role as i16)
    .bind(accepted_ip)
    .bind(now)
    .execute(&mut *txn)
    .await;
    if let Err(error) = insert {
        return Err(
            if matches!(&error, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
            {
                AppError::conflict("Username or email already registered")
            } else {
                AppError::internal(error.to_string())
            },
        );
    }
    let confirmation_token = if status == RegisterStatus::EmailConfirmationRequired {
        let database_now: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *txn)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        let token = email_confirmation::token_for_registration(
            st.config.as_ref(),
            id,
            &norm_email,
            &security_stamp,
            database_now,
            mail_operation_id,
        );
        Some((token, database_now + chrono::Duration::minutes(15)))
    } else {
        None
    };
    if let Some((token, expires_at)) = confirmation_token.as_ref() {
        let outcome = email_confirmation::enqueue_confirmation(
            &st,
            &mut txn,
            mail_operation_id,
            id,
            &security_stamp,
            &email,
            token,
            current_ip.as_deref(),
        )
        .await?;
        if outcome == crate::services::mail_outbox::EnqueueOutcome::Inserted {
            link_attempts::stage_registration(
                &mut txn,
                token,
                id,
                &link_attempts::value_digest(&security_stamp),
                &link_attempts::value_digest(&norm_email),
                *expires_at,
            )
            .await?;
            link_attempts::activate_registration_locked(&mut txn, token, id).await?;
        }
    }
    txn.commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    if admission == Some(anti_cheat::AdmissionOutcome::Blocked) {
        // The stable account id is intentionally retained so an administrator's
        // pair-scoped exemption applies to this same account on its next login.
        // No session and no successful identity observation are created.
        return Ok(MessageResponse::new(anti_cheat::block_message(), 403).into_response());
    }

    // The bootstrap admin is the sole exception. For every later account,
    // required email confirmation takes precedence over active-on-register so
    // enabling it can never silently issue a confirmed session.

    // RSCTF `AccountController` audit events: `Account_UserRegisteredLog` on the
    // straight-to-login path, otherwise `Account_UserRegisteredWaitingApprovalLog`
    // when the account still needs email/admin approval. Best-effort.
    let register_msg = if status == RegisterStatus::LoggedIn {
        format!("User {user_name} registered")
    } else {
        format!("User {user_name} registered, waiting for approval")
    };
    crate::services::audit::info(
        &st,
        "AccountController",
        Some(user_name.clone()),
        None,
        register_msg,
    )
    .await;

    let mut resp = Wrapped::ok(status).into_response();
    if status == RegisterStatus::LoggedIn {
        let token = st.token.issue(id, role, &user_name, &security_stamp)?;
        set_cookie(
            &mut resp,
            &set_session_cookie(&token, st.config.jwt_ttl_secs, st.config.cookie_secure),
        )?;
    }
    Ok(resp)
}

/// 401 Unauthorized with RSCTF's `Account_IncorrectUserNameOrPassword` message —
/// returned for both an unknown username and a wrong password so the two cases are
/// indistinguishable to the client (RSCTF `Unauthorized(…)`, status 401).
pub(super) fn unauthorized_credentials() -> AppError {
    AppError::Coded {
        http: axum::http::StatusCode::UNAUTHORIZED,
        code: 401,
        title: "Wrong username or password".to_string(),
    }
}

/// `POST /api/account/login` -> `void`. Sets the session cookie.
pub async fn login(
    State(st): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(model): Json<LoginModel>,
) -> AppResult<Response> {
    // Verify the live captcha policy before lookup so response ordering cannot
    // reveal whether an account exists; captcha-off login remains unaffected.
    let captcha = CaptchaSettings::load(st.pg(), st.config.account.use_captcha).await?;
    let captcha_admission = captcha
        .verify_local(
            model.challenge.as_deref().unwrap_or(""),
            st.cache.as_ref(),
            &st.config.jwt_secret,
        )
        .await?;

    let credentials_within_bounds =
        model.user_name.len() <= MAX_EMAIL_BYTES && model.password.len() <= MAX_PASSWORD_BYTES;
    let found = if credentials_within_bounds {
        let key = model.user_name.trim().to_uppercase();
        user::Entity::find()
            .filter(
                user::Column::NormalizedUserName
                    .eq(key.clone())
                    .or(user::Column::NormalizedEmail.eq(key)),
            )
            .one(&st.db)
            .await?
    } else {
        None
    };
    // Unknown accounts verify the same valid Argon2id shape as real accounts.
    // This equalizes the dominant CPU cost as well as the status and response body.
    let password_hash = found
        .as_ref()
        .and_then(|user| user.password_hash.as_deref())
        .unwrap_or(DUMMY_PASSWORD_HASH)
        .to_string();
    let supplied_password = if model.password.len() <= MAX_PASSWORD_BYTES {
        model.password.clone()
    } else {
        String::new()
    };
    let password_valid = verify_password_async(supplied_password, password_hash).await?;
    let found = found.ok_or_else(unauthorized_credentials)?;
    if !password_valid {
        return Err(unauthorized_credentials());
    }

    // Only a caller who proved the banned account's password learns its status.
    if found.role == Role::Banned {
        return Err(AppError::Coded {
            http: axum::http::StatusCode::UNAUTHORIZED,
            code: 401,
            title: "User is banned".to_string(),
        });
    }
    // Email-confirmation / admin-approval gate. RSCTF configures Identity with
    // `SignIn.RequireConfirmedEmail = true` (IdentityExtension), so
    // `CheckPasswordSignInAsync` fails its pre-sign-in check for an unconfirmed
    // account and returns the same generic 401 as a wrong password. An account
    // whose registration required email confirmation or admin approval keeps
    // `email_confirmed = false` until granted, and must not be able to log in.
    if !found.email_confirmed {
        return Err(unauthorized_credentials());
    }

    let policy = anti_cheat::load_policy_flags(st.pg()).await?;
    let fingerprint = anti_cheat::validate_fingerprint_submission(
        &st,
        policy,
        model.fingerprint.as_deref(),
        model.fingerprint_proof.as_deref(),
    )
    .await?;

    let id = found.id;
    let role = found.role;
    let user_name = found.user_name.clone().unwrap_or_default();
    let security_stamp = found
        .security_stamp
        .clone()
        .filter(|stamp| !stamp.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Capture the client IP and the submitted browser fingerprint. `current_ip`
    // is normalized so it compares equal to a stored `user.ip`; both are used to
    // stamp the user row *and* feed the anti-cheat gate below.
    let current_ip = anti_cheat::client_ip(&headers, Some(peer.ip()));
    let admission = anti_cheat::admit_existing_user(
        st.pg(),
        st.config.as_ref(),
        id,
        Some(&user_name),
        current_ip.as_deref(),
        fingerprint.as_deref(),
        anti_cheat::IdentitySource::Password,
        &security_stamp,
        None,
        captcha_admission,
    )
    .await?;
    if admission == anti_cheat::AdmissionOutcome::Blocked {
        return Ok(MessageResponse::new(anti_cheat::block_message(), 403).into_response());
    }

    // Fingerprints are durable only as keyed hashes in IdentityObservations;
    // never copy their raw value into application logs.
    crate::services::audit::info(
        &st,
        "AccountController",
        Some(user_name.clone()),
        current_ip.clone(),
        format!("User {user_name} logged in"),
    )
    .await;

    let token = st.token.issue(id, role, &user_name, &security_stamp)?;
    let mut resp = StatusCode::OK.into_response();
    set_cookie(
        &mut resp,
        &set_session_cookie(&token, st.config.jwt_ttl_secs, st.config.cookie_secure),
    )?;
    Ok(resp)
}

/// `GET /api/account/profile` -> raw `ProfileUserInfoModel`.
pub async fn profile(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<ProfileUserInfoModel>> {
    let model = load_user(&st, user.id).await?;
    // True when the user is a co-organizer of at least one game (RSCTF
    // `Game.Managers` / `EventManager`).
    let has_managed_games = game_manager::Entity::find()
        .filter(game_manager::Column::UserId.eq(user.id))
        .count(&st.db)
        .await?
        > 0;
    Ok(RequestResponse::ok(ProfileUserInfoModel::from_user(
        &model,
        has_managed_games,
    )))
}

/// `PUT /api/account/update` -> `void`.
pub async fn update(
    State(st): State<SharedState>,
    user: CurrentUser,
    Json(model): Json<ProfileUpdateModel>,
) -> AppResult<MessageResponse> {
    let current = load_user(&st, user.id).await?;
    validate_profile_fields(
        model.bio.as_deref(),
        model.phone.as_deref(),
        model.real_name.as_deref(),
        model.std_number.as_deref(),
    )?;
    let mut am: user::ActiveModel = current.into();

    if let Some(name) = model.user_name {
        let name = name.trim().to_string();
        if name.len() > MAX_USER_NAME_BYTES {
            return Err(AppError::bad_request("Username is too long"));
        }
        if name.len() >= 3 {
            let norm = name.to_uppercase();
            // `normalized_user_name` is unique; a duplicate rename would surface as a
            // Postgres unique-violation (HTTP 500). Reject cleanly first, mirroring
            // admin/users_mutate.rs update_user.
            if user::Entity::find()
                .filter(user::Column::NormalizedUserName.eq(norm.clone()))
                .filter(user::Column::Id.ne(user.id))
                .one(&st.db)
                .await?
                .is_some()
            {
                return Err(AppError::conflict("Username already taken"));
            }
            am.normalized_user_name = Set(Some(norm));
            am.user_name = Set(Some(name));
        }
    }
    if let Some(bio) = model.bio {
        am.bio = Set(bio);
    }
    if let Some(phone) = model.phone {
        am.phone_number = Set(Some(phone));
    }
    if let Some(real_name) = model.real_name {
        am.real_name = Set(real_name);
    }
    if let Some(std_number) = model.std_number {
        am.std_number = Set(std_number);
    }
    am.update(&st.db).await?;

    // RSCTF `AccountController` audit event (`Account_UserUpdated`). Best-effort.
    crate::services::audit::info(
        &st,
        "AccountController",
        Some(user.name.clone()),
        None,
        format!("User {} updated profile", user.name),
    )
    .await;

    Ok(MessageResponse::ok(""))
}

/// `GET /api/account/fingerprintchallenge` -> `RequestResponseOfBrowserFingerprintChallengeModel`.
///
/// Issues a short-lived, signed and server-stored one-time challenge.
pub async fn fingerprint_challenge(
    State(st): State<SharedState>,
) -> AppResult<Wrapped<BrowserFingerprintChallengeModel>> {
    let challenge = anti_cheat::issue_fingerprint_challenge(&st).await?;
    Ok(Wrapped::ok(BrowserFingerprintChallengeModel {
        nonce: challenge.nonce,
        required_signals: challenge.required_signals,
        expires_in_seconds: challenge.expires_in_seconds,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Attach a `Set-Cookie` header to an outgoing response.
pub(super) fn set_cookie(resp: &mut Response, cookie: &str) -> AppResult<()> {
    let value = HeaderValue::from_str(cookie)
        .map_err(|e| AppError::internal(format!("invalid Set-Cookie: {e}")))?;
    resp.headers_mut().insert(SET_COOKIE, value);
    Ok(())
}

/// Mirror of RSCTF's ASP.NET Identity password policy (IdentityExtension:
/// `RequireNonAlphanumeric = false`, `RequireDigit = true`, `RequireUppercase =
/// true`, `RequireLowercase = true`, `RequiredLength = 6`). RSCTF runs this inside
/// `UserManager.CreateAsync` / `ChangePasswordAsync` / `ResetPasswordAsync` and
/// surfaces the first failing validator's description through `HandleIdentityError`
/// as a 400. We reproduce Identity's `PasswordValidator` check order (length, then
/// digit, lowercase, uppercase) and its default `IdentityError` descriptions so the
/// 400 body matches RSCTF's.
pub(super) fn validate_password(pw: &str) -> AppResult<()> {
    if pw.len() > MAX_PASSWORD_BYTES {
        return Err(AppError::bad_request(format!(
            "Passwords cannot exceed {MAX_PASSWORD_BYTES} bytes."
        )));
    }
    if pw.chars().count() < 6 {
        return Err(AppError::bad_request(
            "Passwords must be at least 6 characters.",
        ));
    }
    if !pw.chars().any(|c| c.is_ascii_digit()) {
        return Err(AppError::bad_request(
            "Passwords must have at least one digit ('0'-'9').",
        ));
    }
    if !pw.chars().any(char::is_lowercase) {
        return Err(AppError::bad_request(
            "Passwords must have at least one lowercase ('a'-'z').",
        ));
    }
    if !pw.chars().any(char::is_uppercase) {
        return Err(AppError::bad_request(
            "Passwords must have at least one uppercase ('A'-'Z').",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "registration_policy_tests.rs"]
mod registration_policy_tests;
