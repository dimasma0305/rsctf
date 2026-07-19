//! Ported from RSCTF `Controllers/AccountController.cs`.
//!
//! Compatibility implementation of the `/api/account/*` surface: paths,
//! camelCase DTO fields, and success envelopes match `web/src/Api.ts`. The SPA authenticates with a
//! same-origin rsctf session cookie, so `login`/`register` set it via
//! `Set-Cookie` and `logout` clears it.

use crate::middlewares::rate_limiter::{limited, Policy};
use axum::extract::{ConnectInfo, Multipart, State};
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
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{
    clear_session_cookie, set_session_cookie, CurrentUser, MaybeUser,
};
use crate::models::data::{
    config, first_solve, game, game_challenge, game_manager, log_entry, submission, user,
};
use crate::models::request::account::*;
use crate::services::anti_cheat;
use crate::services::captcha::CaptchaSettings;
use crate::utils::crypto_utils::{hash_password_async, verify_password_async};
use crate::utils::enums::{RegisterStatus, Role};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{MessageResponse, RequestResponse, Wrapped};

const MAX_AVATAR_BYTES: usize = 3 * 1024 * 1024;
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$YBSHJA9ANNWFII7EsOe1rw$O5h6h9EwR/6Pyoe9wCcjK91HivbrgJZwb44fhsiqonw";
pub(crate) const REGISTRATION_LOCK_ID: i64 = 0x5253_4354_4652_4547; // "RSCTFREG"

mod bootstrap;
mod recovery;
pub use recovery::*;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/account/avatar", put(avatar))
        .route(
            "/api/account/changeemail",
            limited(Policy::Register, put(change_email)),
        )
        .route("/api/account/changepassword", put(change_password))
        .route(
            "/api/account/fingerprintchallenge",
            get(fingerprint_challenge),
        )
        .route("/api/account/login", limited(Policy::Login, post(login)))
        .route("/api/account/logout", post(logout))
        .route(
            "/api/account/mailchangeconfirm",
            limited(Policy::Register, post(mail_change_confirm)),
        )
        .route("/api/account/passwordreset", post(password_reset))
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
// Local request DTOs not shared elsewhere (camelCase, tolerant).
// ---------------------------------------------------------------------------

/// `LoginModel` — credentials plus the optional browser fingerprint the SPA
/// collects (see `Api.ts`). The shared request DTO omits `fingerprint`, so we
/// deserialize into this tolerant local copy to capture it at login. Shadows
/// the glob-imported `models::request::account::LoginModel` within this module.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginModel {
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Captcha token (`ModelWithCaptcha.challenge`); verified only when the live
    /// `AccountPolicy:UseCaptcha` is on. Absent/`null` on captcha-off deployments.
    #[serde(default)]
    pub challenge: Option<String>,
}

/// `MailChangeModel` — new email address.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailChangeModel {
    #[serde(default)]
    pub new_mail: String,
    /// Current password re-authentication. A session bearer alone is not enough
    /// to redirect future recovery mail to a new address.
    #[serde(default)]
    pub password: String,
}

/// `AccountVerifyModel` — email-confirmation / mail-change token.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountVerifyModel {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub email: String,
}

/// `PasswordResetModel` — reset password using an emailed token.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordResetModel {
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub r_token: String,
}

/// `RecoveryModel` — request a password-recovery email.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryModel {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub challenge: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct EmailChangeTicket {
    pub user_id: Uuid,
    pub new_email: String,
    pub security_stamp: String,
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

/// Serialize account provisioning across every replica. The lock is scoped to
/// the transaction, so it is released on commit, rollback, or connection loss.
pub(crate) async fn locked_registration_transaction(
    st: &SharedState,
) -> AppResult<DatabaseTransaction> {
    let txn = crate::utils::database::begin_seaorm_transaction(&st.db).await?;
    txn.execute_unprepared(&format!(
        "SELECT pg_advisory_xact_lock({REGISTRATION_LOCK_ID})"
    ))
    .await?;
    Ok(txn)
}

/// `POST /api/account/register` -> `RequestResponseOfRegisterStatus`.
///
/// Creates the account and, when no confirmation gate is configured, logs the
/// user straight in by issuing a session cookie.
pub async fn register(
    State(st): State<SharedState>,
    Json(model): Json<RegisterModel>,
) -> AppResult<Response> {
    // Fail fast before policy loading, captcha verification, and Argon2.
    let may_be_first = bootstrap::preflight(&st, model.bootstrap_token.as_deref()).await?;
    // Load the live AccountPolicy from the `Configs` key/value table so the
    // /admin/config toggles take effect per-request (RSCTF reads AccountPolicy
    // from an IOptionsSnapshot backed by the DB). Each key falls back to the
    // startup env-loaded `st.config.account` when it was never persisted —
    // mirrors InfoController reading GlobalConfig from `config::Entity`.
    let mut allow_register = st.config.account.allow_register;
    let mut email_confirmation_required = st.config.account.email_confirmation_required;
    let mut active_on_register = st.config.account.active_on_register;
    // Comma-separated domain allowlist; empty = allow all (RSCTF EmailDomainList).
    let mut email_domain_list = String::new();
    for row in config::Entity::find().all(&st.db).await? {
        let Some(value) = row.value else { continue };
        match row.config_key.as_str() {
            // Persisted as lowercase `bool::to_string()` (matching admin config).
            "AccountPolicy:AllowRegister" => allow_register = value == "true",
            "AccountPolicy:EmailConfirmationRequired" => {
                email_confirmation_required = value == "true";
            }
            "AccountPolicy:ActiveOnRegister" => active_on_register = value == "true",
            "AccountPolicy:EmailDomainList" => email_domain_list = value,
            _ => {}
        }
    }

    // The very first account always bootstraps the platform admin — even when
    // public registration is disabled — so a fresh or locked-down instance can
    // always be set up. Everyone after the first obeys the allow_register gate.
    if !may_be_first && !allow_register {
        return Err(AppError::bad_request("Registration is disabled"));
    }

    // Captcha gate (RSCTF `AccountController.Register`: `if (UseCaptcha &&
    // !VerifyAsync) return BadRequest`), placed right after the AllowRegister gate
    // and BEFORE creating the account. Only enforced when the live
    // `AccountPolicy:UseCaptcha` is on, so captcha-off registration is unaffected.
    let captcha = CaptchaSettings::load(&st.db).await;
    if captcha.use_captcha
        && !captcha
            .service()
            .verify(model.challenge.as_deref().unwrap_or(""), st.cache.as_ref())
            .await?
    {
        return Err(AppError::bad_request("Captcha failed"));
    }

    let user_name = model.user_name.trim().to_string();
    if user_name.len() < 3 {
        return Err(AppError::bad_request(
            "Username must be at least 3 characters",
        ));
    }
    if model.password.len() < 6 {
        return Err(AppError::bad_request(
            "Password must be at least 6 characters",
        ));
    }
    validate_password(&model.password)?;
    let email = model.email.trim().to_lowercase();
    if !email.contains('@') {
        return Err(AppError::bad_request("Invalid email address"));
    }
    // Enforce the EmailDomainList allowlist (RSCTF VerifyEmailDomain): a non-empty
    // list rejects addresses whose domain is not in it. Same 400 RSCTF returns.
    if !verify_email_domain(&email, &email_domain_list) {
        return Err(AppError::bad_request("Email domain is not allowed"));
    }

    let norm_name = user_name.to_uppercase();
    let norm_email = email.to_uppercase();

    if user::Entity::find()
        .filter(user::Column::NormalizedUserName.eq(norm_name.clone()))
        .one(&st.db)
        .await?
        .is_some()
    {
        return Err(AppError::conflict("Username already taken"));
    }
    if user::Entity::find()
        .filter(user::Column::NormalizedEmail.eq(norm_email.clone()))
        .one(&st.db)
        .await?
        .is_some()
    {
        return Err(AppError::conflict("Email already registered"));
    }

    // RSCTF `EnableBrowserFingerprint`: when on, registration must carry a
    // well-formed browser fingerprint (`^[a-f0-9]{64}$`), else 400.
    enforce_browser_fingerprint(
        &st,
        model
            .fingerprint
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty()),
    )
    .await?;

    let now = Utc::now();
    let id = Uuid::now_v7();
    let password_hash = hash_password_async(model.password.clone()).await?;
    let security_stamp = Uuid::new_v4().to_string();

    // The initial count above is only a cheap disabled-registration fast path.
    // Re-evaluate while holding a transaction-scoped advisory lock so exactly one
    // concurrent request can observe an empty user table and become bootstrap admin.
    let txn = locked_registration_transaction(&st).await?;
    let is_first = user::Entity::find().count(&txn).await? == 0;
    let txn = bootstrap::recheck(txn, is_first, model.bootstrap_token.as_deref()).await?;
    if !is_first && !allow_register {
        txn.rollback().await?;
        return Err(AppError::bad_request("Registration is disabled"));
    }
    if user::Entity::find()
        .filter(user::Column::NormalizedUserName.eq(norm_name.clone()))
        .one(&txn)
        .await?
        .is_some()
    {
        txn.rollback().await?;
        return Err(AppError::conflict("Username already taken"));
    }
    if user::Entity::find()
        .filter(user::Column::NormalizedEmail.eq(norm_email.clone()))
        .one(&txn)
        .await?
        .is_some()
    {
        txn.rollback().await?;
        return Err(AppError::conflict("Email already registered"));
    }
    let role = if is_first { Role::Admin } else { Role::User };

    let am = user::ActiveModel {
        id: Set(id),
        user_name: Set(Some(user_name.clone())),
        normalized_user_name: Set(Some(norm_name)),
        email: Set(Some(email)),
        normalized_email: Set(Some(norm_email)),
        // The first user (the bootstrap admin) is always active; everyone else
        // is auto-confirmed only under active-on-register (RSCTF sets
        // EmailConfirmed=true solely inside the ActiveOnRegister branch — the
        // admin-approval / email-verification paths leave it false until granted).
        email_confirmed: Set(is_first || active_on_register),
        password_hash: Set(Some(password_hash)),
        security_stamp: Set(Some(security_stamp.clone())),
        concurrency_stamp: Set(Some(Uuid::new_v4().to_string())),
        phone_number: Set(None),
        phone_number_confirmed: Set(false),
        two_factor_enabled: Set(false),
        lockout_end: Set(None),
        lockout_enabled: Set(false),
        access_failed_count: Set(0),
        role: Set(role),
        ip: Set("0.0.0.0".to_string()),
        browser_fingerprint: Set(None),
        last_signed_in_utc: Set(now),
        last_visited_utc: Set(now),
        register_time_utc: Set(now),
        bio: Set(String::new()),
        real_name: Set(String::new()),
        std_number: Set(String::new()),
        exercise_visible: Set(true),
        avatar_hash: Set(None),
    };
    am.insert(&txn).await?;
    txn.commit().await?;

    // RSCTF register precedence (AccountController lines 194-246): active-on-
    // register wins and logs straight in; otherwise the admin-approval path
    // unless email confirmation is required. The first account (bootstrap admin)
    // always logs straight in regardless of policy.
    let status = if is_first || active_on_register {
        RegisterStatus::LoggedIn
    } else if !email_confirmation_required {
        RegisterStatus::AdminConfirmationRequired
    } else {
        RegisterStatus::EmailConfirmationRequired
    };

    // RSCTF `AccountController` audit events: `Account_UserRegisteredLog` on the
    // straight-to-login path, otherwise `Account_UserRegisteredWaitingApprovalLog`
    // when the account still needs email/admin approval. Best-effort.
    let register_msg = if status == RegisterStatus::LoggedIn {
        format!("User {user_name} registered")
    } else {
        format!("User {user_name} registered, waiting for approval")
    };
    crate::services::audit::info(
        &st.db,
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
/// RSCTF `BrowserFingerprintRegex` — the decrypted fingerprint must be a 64-char
/// lowercase-hex SHA-256 digest.
fn valid_browser_fingerprint(fp: &str) -> bool {
    fp.len() == 64
        && fp
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Live `AccountPolicy:EnableBrowserFingerprint` toggle (falls back to the
/// startup config when the key is absent), mirroring how register reads the other
/// AccountPolicy keys.
pub(super) async fn enable_browser_fingerprint(st: &SharedState) -> bool {
    config::Entity::find_by_id("AccountPolicy:EnableBrowserFingerprint".to_string())
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .and_then(|c| c.value)
        .map(|v| v == "true")
        // RSCTF `AccountPolicy.EnableBrowserFingerprint` default is false.
        .unwrap_or(false)
}

/// Enforce the browser-fingerprint gate RSCTF applies on login/register when
/// `EnableBrowserFingerprint` is on: reject a request whose fingerprint is missing
/// or malformed (`Parameter_FingerprintRequired`/`FingerprintInvalid`). NOTE: the
/// full proof validation (`ValidateBrowserFingerprint` — RSA `DecryptApiData` +
/// `BrowserFingerprintProofValidator.IsTrusted`) needs the apiPublicKey API-data
/// encryption subsystem, which rsctf does not port (`api_public_key: None`), so
/// the client sends the fingerprint in plaintext and only the presence/format is
/// enforced here.
pub(super) async fn enforce_browser_fingerprint(
    st: &SharedState,
    fingerprint: Option<&str>,
) -> AppResult<()> {
    if enable_browser_fingerprint(st).await && !fingerprint.is_some_and(valid_browser_fingerprint) {
        return Err(AppError::bad_request(
            "A valid browser fingerprint is required.",
        ));
    }
    Ok(())
}

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
    // Captcha gate (RSCTF `AccountController.LogIn`: `if (UseCaptcha &&
    // !VerifyAsync) return BadRequest`), verified FIRST — before the user lookup,
    // so response ordering/timing can't leak account existence. Only enforced when
    // the live `AccountPolicy:UseCaptcha` is on, so captcha-off login is unaffected.
    let captcha = CaptchaSettings::load(&st.db).await;
    if captcha.use_captcha
        && !captcha
            .service()
            .verify(model.challenge.as_deref().unwrap_or(""), st.cache.as_ref())
            .await?
    {
        return Err(AppError::bad_request("Captcha failed"));
    }

    let key = model.user_name.trim().to_uppercase();
    let found = user::Entity::find()
        .filter(
            user::Column::NormalizedUserName
                .eq(key.clone())
                .or(user::Column::NormalizedEmail.eq(key)),
        )
        .one(&st.db)
        .await?;
    // Unknown accounts verify the same valid Argon2id shape as real accounts.
    // This equalizes the dominant CPU cost as well as the status and response body.
    let password_hash = found
        .as_ref()
        .and_then(|user| user.password_hash.as_deref())
        .unwrap_or(DUMMY_PASSWORD_HASH)
        .to_string();
    let password_valid = verify_password_async(model.password.clone(), password_hash).await;
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

    // RSCTF `EnableBrowserFingerprint`: when on, a login must carry a well-formed
    // browser fingerprint (`^[a-f0-9]{64}$`), else 400.
    enforce_browser_fingerprint(
        &st,
        model
            .fingerprint
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty()),
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
    let needs_security_stamp = found.security_stamp.as_deref().is_none_or(str::is_empty);

    // Capture the client IP and the submitted browser fingerprint. `current_ip`
    // is normalized so it compares equal to a stored `user.ip`; both are used to
    // stamp the user row *and* feed the anti-cheat gate below.
    let current_ip = anti_cheat::client_ip(&headers, Some(peer.ip()));
    let fingerprint = model
        .fingerprint
        .as_ref()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty());

    // The pre-update Model — the anti-cheat gate needs the user's id + name (for
    // self-exclusion, the teammate lookup, and the recorded block row).
    let user_model = found.clone();

    let mut am: user::ActiveModel = found.into();
    am.last_signed_in_utc = Set(Utc::now());
    if needs_security_stamp {
        am.security_stamp = Set(Some(security_stamp.clone()));
    }
    // Persist the login IP / fingerprint (RSCTF `UpdateByHttpContext` +
    // fingerprint claim). Only overwrite when captured, so a login without a
    // fingerprint never wipes a previously stored one.
    if let Some(ip) = current_ip.as_deref() {
        am.ip = Set(ip.to_string());
    }
    if let Some(fp) = fingerprint {
        am.browser_fingerprint = Set(Some(fp.to_string()));
    }
    am.update(&st.db).await?;

    // Anti-cheat login gate (RSCTF `CheckAntiCheatConflictAsync`): deny — and do
    // NOT issue the session — when the IP/fingerprint collides with a teammate or,
    // under a global policy, any other recently-active account.
    let policy = anti_cheat::load_policy_flags(&st.db).await?;
    if let Some(block) = anti_cheat::check_login_conflict(
        &st.db,
        &policy,
        &user_model,
        current_ip.as_deref(),
        fingerprint,
    )
    .await?
    {
        return Ok(MessageResponse::new(anti_cheat::block_message(&block), 403).into_response());
    }

    // RSCTF's browser-fingerprint capture point: when the client submits a
    // non-empty fingerprint, record it in the audit log for the anti-cheat /
    // suspicion correlation. Best-effort — never blocks the login.
    if let Some(fp) = model
        .fingerprint
        .as_ref()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty())
    {
        let entry = log_entry::ActiveModel {
            time_utc: Set(Utc::now()),
            level: Set("Information".to_string()),
            logger: Set("fingerprint".to_string()),
            remote_ip: Set(current_ip.clone()),
            user_name: Set(Some(user_name.clone())),
            message: Set(fp.to_string()),
            status: Set(Some("login".to_string())),
            ..Default::default()
        };
        entry.insert(&st.db).await?;
    }

    // RSCTF `AccountController` audit event (`Account_UserLogined`): the
    // human-readable login row shown on `/admin/logs`, emitted on the guaranteed
    // success path (after the anti-cheat gate, regardless of fingerprint). RSCTF
    // attaches the submitted fingerprint to this row (`logger.Log(..., fingerprint:
    // fingerprint)`); it surfaces in the admin Logs table's `fingerprint` column.
    crate::services::audit::info_with_fingerprint(
        &st.db,
        "AccountController",
        Some(user_name.clone()),
        current_ip.clone(),
        fingerprint.map(str::to_string),
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

/// RSCTF `GameStatItem` — one game the user has solves in.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatItem {
    pub game_id: i32,
    pub game_title: String,
    #[serde(with = "crate::utils::datetime::millis")]
    pub end_time_utc: chrono::DateTime<Utc>,
    pub solves: i32,
}

/// RSCTF `UserStatsModel` — the "My Stats" tab payload.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserStatsModel {
    pub total_solves: i32,
    pub total_first_bloods: i32,
    pub games_participated: i32,
    pub solves_by_category: std::collections::BTreeMap<String, i32>,
    pub games: Vec<GameStatItem>,
}

/// `GET /api/account/stats` -> `UserStatsModel` (RSCTF `Account.Stats`): the
/// signed-in user's solve totals, first bloods, per-category and per-game solves.
pub async fn stats(
    State(st): State<SharedState>,
    user: CurrentUser,
) -> AppResult<RequestResponse<UserStatsModel>> {
    use crate::utils::enums::AnswerResult;
    use sea_orm::ColumnTrait;
    use std::collections::{BTreeMap, HashMap, HashSet};

    let subs = submission::Entity::find()
        .filter(submission::Column::UserId.eq(user.id))
        .filter(submission::Column::Status.eq(AnswerResult::Accepted))
        .all(&st.db)
        .await?;

    let challenge_ids: Vec<i32> = subs
        .iter()
        .map(|s| s.challenge_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let game_ids: Vec<i32> = subs
        .iter()
        .map(|s| s.game_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // Challenge categories + game titles/ends for the solved set.
    let cat_by_challenge: HashMap<i32, String> = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.is_in(challenge_ids.clone()))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|c| {
            let label = serde_json::to_value(c.category)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            (c.id, label)
        })
        .collect();
    let games_map: HashMap<i32, game::Model> = game::Entity::find()
        .filter(game::Column::Id.is_in(game_ids.clone()))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|g| (g.id, g))
        .collect();

    // First bloods: first-solve rows whose submission is one of this user's.
    let sub_ids: HashSet<i32> = subs.iter().map(|s| s.id).collect();
    // Count in SQL (WHERE submission_id IN …) instead of loading the entire
    // first_solve table and counting in Rust. `is_in` on an empty set yields a
    // 0 count, matching the old behaviour.
    let total_first_bloods = first_solve::Entity::find()
        .filter(first_solve::Column::SubmissionId.is_in(sub_ids.iter().copied()))
        .count(&st.db)
        .await? as i32;

    // Distinct-challenge tallies per category and per game.
    let mut by_category: BTreeMap<String, HashSet<i32>> = BTreeMap::new();
    let mut by_game: HashMap<i32, HashSet<i32>> = HashMap::new();
    for s in &subs {
        if let Some(cat) = cat_by_challenge.get(&s.challenge_id) {
            by_category
                .entry(cat.clone())
                .or_default()
                .insert(s.challenge_id);
        }
        by_game.entry(s.game_id).or_default().insert(s.challenge_id);
    }

    let mut games: Vec<GameStatItem> = by_game
        .iter()
        .filter_map(|(gid, chals)| {
            games_map.get(gid).map(|g| GameStatItem {
                game_id: *gid,
                game_title: g.title.clone(),
                end_time_utc: g.end_time_utc,
                solves: chals.len() as i32,
            })
        })
        .collect();
    games.sort_by_key(|game| std::cmp::Reverse(game.end_time_utc));

    Ok(RequestResponse::ok(UserStatsModel {
        total_solves: challenge_ids.len() as i32,
        total_first_bloods,
        games_participated: games.len() as i32,
        solves_by_category: by_category
            .into_iter()
            .map(|(k, v)| (k, v.len() as i32))
            .collect(),
        games,
    }))
}

/// `PUT /api/account/update` -> `void`.
pub async fn update(
    State(st): State<SharedState>,
    user: CurrentUser,
    Json(model): Json<ProfileUpdateModel>,
) -> AppResult<MessageResponse> {
    let current = load_user(&st, user.id).await?;
    let mut am: user::ActiveModel = current.into();

    if let Some(name) = model.user_name {
        let name = name.trim().to_string();
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
        &st.db,
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
/// Issues a benign challenge; the register flow accepts any proof.
pub async fn fingerprint_challenge() -> Wrapped<BrowserFingerprintChallengeModel> {
    Wrapped::ok(BrowserFingerprintChallengeModel {
        nonce: Uuid::new_v4().to_string(),
        required_signals: Vec::new(),
        expires_in_seconds: 120,
    })
}

/// `PUT /api/account/avatar` (multipart, field `file`) -> raw avatar URL string.
pub async fn avatar(
    State(st): State<SharedState>,
    user: CurrentUser,
    mut multipart: Multipart,
) -> AppResult<RequestResponse<String>> {
    let mut data: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("multipart error: {e}")))?
    {
        if field.name() == Some("file") {
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("could not read file: {e}")))?;
            data = Some(bytes.to_vec());
            break;
        }
    }
    let bytes = data.ok_or_else(|| AppError::bad_request("No file provided"))?;
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return Err(AppError::bad_request("Invalid avatar file size"));
    }

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let old_hash = sqlx::query_as::<_, (Option<String>,)>(
        r#"SELECT avatar_hash FROM "AspNetUsers" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(user.id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("User not found"))?
    .0;
    let (blob, _) = crate::services::blob_refs::store_and_acquire_in_transaction(
        st.storage.as_ref(),
        &mut transaction,
        "avatar",
        &bytes,
    )
    .await?;
    sqlx::query(r#"UPDATE "AspNetUsers" SET avatar_hash = $2 WHERE id = $1"#)
        .bind(user.id)
        .bind(&blob.hash)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(old_hash) = old_hash {
        if let Err(error) =
            crate::services::blob_refs::release_and_purge(st.pg(), st.storage.as_ref(), &old_hash)
                .await
        {
            tracing::warn!(%error, hash = %old_hash, "old user avatar purge failed");
        }
    }

    // RSCTF `AccountController` audit event (`Account_AvatarUpdated`). Best-effort.
    crate::services::audit::info(
        &st.db,
        "AccountController",
        Some(user.name.clone()),
        None,
        format!("User {} updated avatar", user.name),
    )
    .await;

    Ok(RequestResponse::ok(format!("/assets/{}/avatar", blob.hash)))
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

pub(super) async fn load_user(st: &SharedState, id: Uuid) -> AppResult<user::Model> {
    user::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("User not found"))
}
