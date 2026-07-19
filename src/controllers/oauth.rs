//! controllers/oauth.rs — external OAuth login (Google / Discord), ported from
//! RSCTF `Controllers/AccountController.External.cs`.
//!
//! Two full-page-redirect endpoints implement the hand-rolled OAuth dance the
//! SPA drives (see `web/src/components/OAuthButtons.tsx`):
//!
//!   * `GET /api/oauth/{provider}`          — bounce the browser to the provider
//!     consent screen, after minting a one-time CSRF `state` in the cache.
//!   * `GET /api/oauth/{provider}/callback` — validate `state`, exchange the
//!     `code`, read the userinfo, find-or-create the user, then issue the
//!     rsctf session cookie and land back on `returnUrl`.
//!
//! Credentials come from the environment (`RSCTF_{PROVIDER}_CLIENT_ID` /
//! `_CLIENT_SECRET`); an unconfigured provider is treated as disabled. Every
//! failure path (network, decode, CSRF, missing email, …) degrades to a graceful
//! redirect to `/account/login` — an external sign-in must never surface a 500 or
//! a JSON error envelope to the browser mid-redirect.

use std::time::Duration;

use crate::middlewares::rate_limiter::{limited, Policy};
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Set,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::set_session_cookie;
use crate::models::data::{config, user};
use crate::services::anti_cheat;
use crate::utils::crypto_utils::hash_password_async;
use crate::utils::enums::Role;
use crate::utils::error::AppError;

/// SPA login route; every unhappy path funnels here.
const LOGIN_PATH: &str = "/account/login";
const OAUTH_STATE_COOKIE: &str = "RSCTF_OAuthState";
const OAUTH_STATE_TTL: u64 = 10 * 60;

#[derive(Debug, Serialize, Deserialize)]
struct OAuthState {
    provider: String,
    return_url: String,
    code_verifier: String,
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/oauth/{provider}",
            limited(Policy::Register, get(start)),
        )
        .route("/api/oauth/{provider}/callback", get(callback))
}

// ---------------------------------------------------------------------------
// Provider table
// ---------------------------------------------------------------------------

/// OAuth2 endpoint set for one provider.
struct Provider {
    /// Canonical lowercase key used in URLs and the cached state value.
    name: &'static str,
    /// Env-var infix: `RSCTF_{env}_CLIENT_ID` / `_CLIENT_SECRET`.
    env: &'static str,
    authorize: String,
    token: String,
    userinfo: String,
    scope: &'static str,
}

/// A provider endpoint, overridable via `RSCTF_{env}_{kind}_URL` (e.g.
/// `RSCTF_GOOGLE_TOKEN_URL`). Overrides let a deployment point at an enterprise
/// or self-hosted IdP — and let the flow be exercised against a mock — while
/// defaulting to the real Google/Discord endpoints.
fn endpoint(env: &str, kind: &str, default: &str) -> String {
    std::env::var(format!("RSCTF_{env}_{kind}_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Resolve a provider by its (case-insensitive) name. Unknown -> `None`.
fn lookup_provider(name: &str) -> Option<Provider> {
    match name {
        "google" => Some(Provider {
            name: "google",
            env: "GOOGLE",
            authorize: endpoint(
                "GOOGLE",
                "AUTH",
                "https://accounts.google.com/o/oauth2/v2/auth",
            ),
            token: endpoint("GOOGLE", "TOKEN", "https://oauth2.googleapis.com/token"),
            userinfo: endpoint(
                "GOOGLE",
                "USERINFO",
                "https://www.googleapis.com/oauth2/v3/userinfo",
            ),
            scope: "openid email profile",
        }),
        "discord" => Some(Provider {
            name: "discord",
            env: "DISCORD",
            authorize: endpoint(
                "DISCORD",
                "AUTH",
                "https://discord.com/api/oauth2/authorize",
            ),
            token: endpoint("DISCORD", "TOKEN", "https://discord.com/api/oauth2/token"),
            userinfo: endpoint("DISCORD", "USERINFO", "https://discord.com/api/users/@me"),
            scope: "identify email",
        }),
        _ => None,
    }
}

/// A provider is configured when both credential env vars are non-empty. Mirrors
/// `info.rs::oauth_configured` (which is private, so the same check is inlined).
fn credentials(env: &str) -> Option<(String, String)> {
    let id = std::env::var(format!("RSCTF_{env}_CLIENT_ID")).unwrap_or_default();
    let secret = std::env::var(format!("RSCTF_{env}_CLIENT_SECRET")).unwrap_or_default();
    if id.trim().is_empty() || secret.trim().is_empty() {
        None
    } else {
        Some((id, secret))
    }
}

/// Public origin the provider redirects back to (`RSCTF_PUBLIC_URL`, trailing
/// slash trimmed). Empty when unset — the callback path stays site-relative,
/// which still works when the SPA and API share an origin.
fn public_url() -> String {
    std::env::var("RSCTF_PUBLIC_URL")
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
        .unwrap_or_default()
}

/// The redirect URI registered in the provider console for this provider.
fn callback_url(provider: &str) -> String {
    format!("{}/api/oauth/{provider}/callback", public_url())
}

/// Constrain `returnUrl` to a same-site absolute path (open-redirect guard):
/// it must start with a single `/` and not `//` (which the browser reads as a
/// protocol-relative external URL). Anything else falls back to `/`.
fn sanitize_return(raw: Option<&str>) -> String {
    // Must be a same-site absolute path: a single leading '/' not followed by
    // another '/' or a '\' (browsers normalize '\'->'/' for special schemes, so
    // `/\evil.com` would become the protocol-relative `//evil.com` = open
    // redirect). Also reject control/whitespace bytes.
    match raw {
        Some(u)
            if u.starts_with('/')
                && !u[1..].starts_with('/')
                && !u[1..].starts_with('\\')
                && !u.chars().any(|c| c.is_control() || c == ' ') =>
        {
            u.to_string()
        }
        _ => "/".to_string(),
    }
}

fn login_redirect() -> Response {
    Redirect::to(LOGIN_PATH).into_response()
}

fn oauth_state_cookie(value: &str, max_age: u64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{OAUTH_STATE_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax{secure}; Max-Age={max_age}"
    )
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let cookies = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    cookies.split(';').find_map(|pair| {
        pair.trim()
            .strip_prefix(name)
            .and_then(|value| value.strip_prefix('='))
    })
}

// ---------------------------------------------------------------------------
// GET /api/oauth/{provider} — begin sign-in
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct StartQuery {
    #[serde(rename = "returnUrl", default)]
    return_url: Option<String>,
}

/// Redirect the browser to the provider's authorize endpoint. A disabled or
/// unknown provider bounces to the login page instead.
async fn start(
    State(st): State<SharedState>,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
) -> Response {
    let provider = provider.to_lowercase();
    let Some(p) = lookup_provider(&provider) else {
        return login_redirect();
    };
    let Some((client_id, _secret)) = credentials(p.env) else {
        return login_redirect();
    };

    let return_url = sanitize_return(q.return_url.as_deref());

    // One-time CSRF token binding the callback to this request. `provider|returnUrl`
    // lets the callback both re-confirm the provider and recover where to land.
    let state = crate::utils::codec::random_token(32);
    let code_verifier = crate::utils::codec::random_token(32);
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(code_verifier.as_bytes()));
    let state_record = OAuthState {
        provider: p.name.to_string(),
        return_url,
        code_verifier,
    };
    let state_record = match serde_json::to_vec(&state_record) {
        Ok(record) => record,
        Err(_) => return login_redirect(),
    };
    st.cache
        .set(
            &format!("oauth_state:{state}"),
            &state_record,
            Some(Duration::from_secs(OAUTH_STATE_TTL)),
        )
        .await;

    let redirect_uri = callback_url(p.name);
    // `parse_with_params` percent-encodes every value (scope has spaces, the
    // redirect_uri has `:` and `/`), so we never hand-roll query encoding.
    let authorize = match reqwest::Url::parse_with_params(
        &p.authorize,
        &[
            ("client_id", client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", p.scope),
            ("state", state.as_str()),
            ("code_challenge", code_challenge.as_str()),
            ("code_challenge_method", "S256"),
        ],
    ) {
        Ok(u) => u,
        Err(_) => return login_redirect(),
    };

    let mut response = Redirect::to(authorize.as_str()).into_response();
    if let Ok(value) = HeaderValue::from_str(&oauth_state_cookie(
        &state,
        OAUTH_STATE_TTL,
        st.config.cookie_secure,
    )) {
        response.headers_mut().append(SET_COOKIE, value);
    }
    response
}

// ---------------------------------------------------------------------------
// GET /api/oauth/{provider}/callback — finish sign-in
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// Provider token-endpoint response (only `access_token` is needed).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: Option<String>,
}

/// Provider userinfo — one tolerant shape covering both providers. Google emits
/// `name`; Discord emits `username`. `email` may legitimately be absent/null
/// (unverified Discord accounts), so it is optional and its absence aborts.
#[derive(Debug, Deserialize)]
struct UserInfoResponse {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    username: Option<String>,
    /// Google uses `email_verified`; some compatible IdPs use the reversed alias.
    #[serde(default, alias = "verified_email")]
    email_verified: Option<bool>,
    /// Discord's verification flag.
    #[serde(default)]
    verified: Option<bool>,
}

fn provider_email_verified(provider: &str, user: &UserInfoResponse) -> bool {
    match provider {
        "google" => user.email_verified == Some(true),
        "discord" => user.verified == Some(true),
        _ => false,
    }
}

fn oauth_account_active(role: Role, email_confirmed: bool) -> bool {
    role != Role::Banned && email_confirmed
}

/// Outer handler: any error from the exchange becomes the login redirect, so the
/// browser is never shown a 500 or a JSON error body mid-flow.
async fn callback(
    State(st): State<SharedState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let secure = st.config.cookie_secure;
    let mut response = match callback_inner(st, provider, headers, q, peer).await {
        Ok(resp) => resp,
        Err(_) => login_redirect(),
    };
    if let Ok(value) = HeaderValue::from_str(&oauth_state_cookie("", 0, secure)) {
        response.headers_mut().append(SET_COOKIE, value);
    }
    response
}

async fn callback_inner(
    st: SharedState,
    provider: String,
    headers: HeaderMap,
    q: CallbackQuery,
    peer: SocketAddr,
) -> Result<Response, AppError> {
    let provider = provider.to_lowercase();
    let p = lookup_provider(&provider).ok_or_else(|| AppError::bad_request("unknown provider"))?;
    let (client_id, client_secret) =
        credentials(p.env).ok_or_else(|| AppError::bad_request("provider not configured"))?;

    let code = q
        .code
        .filter(|c| !c.is_empty())
        .ok_or_else(|| AppError::bad_request("missing code"))?;
    let state = q
        .state
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("missing state"))?;
    if cookie_value(&headers, OAUTH_STATE_COOKIE) != Some(state.as_str()) {
        return Err(AppError::bad_request(
            "state was not initiated by this browser",
        ));
    }

    // CSRF guard: the state must correspond to this browser and a live one-time
    // server record. Atomic consume prevents concurrent callback replay.
    let key = format!("oauth_state:{state}");
    let stored: OAuthState = st
        .cache
        .get_and_remove(&key)
        .await
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| AppError::bad_request("invalid state"))?;
    if stored.provider != p.name {
        return Err(AppError::bad_request("provider mismatch"));
    }
    let return_url = sanitize_return(Some(&stored.return_url));

    // Discord's userinfo is Cloudflare-fronted and 403s without a User-Agent.
    let client = reqwest::Client::builder()
        .user_agent("rsctf-oauth/1.0")
        .build()
        .map_err(|e| AppError::internal(format!("http client: {e}")))?;

    // Exchange the authorization code for an access token.
    let redirect_uri = callback_url(p.name);
    let token_resp = client
        .post(p.token.as_str())
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", stored.code_verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AppError::internal(format!("token request: {e}")))?;
    if !token_resp.status().is_success() {
        return Err(AppError::bad_request("token exchange failed"));
    }
    let token_body: TokenResponse = token_resp
        .json()
        .await
        .map_err(|e| AppError::internal(format!("token decode: {e}")))?;
    let access_token = token_body
        .access_token
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AppError::bad_request("no access token"))?;

    // Read the user profile.
    let ui_resp = client
        .get(p.userinfo.as_str())
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|e| AppError::internal(format!("userinfo request: {e}")))?;
    if !ui_resp.status().is_success() {
        return Err(AppError::bad_request("userinfo failed"));
    }
    let ui: UserInfoResponse = ui_resp
        .json()
        .await
        .map_err(|e| AppError::internal(format!("userinfo decode: {e}")))?;

    if !provider_email_verified(p.name, &ui) {
        return Err(AppError::bad_request("provider email is not verified"));
    }

    // A provider-verified email is required to find or create the account.
    let email = ui
        .email
        .map(|e| e.trim().to_lowercase())
        .filter(|e| e.contains('@'))
        .ok_or_else(|| AppError::bad_request("no email"))?;
    let display = ui.name.or(ui.username);
    let norm_email = email.to_uppercase();

    // Find by email, or provision a fresh external account (no password).
    let account = match user::Entity::find()
        .filter(user::Column::NormalizedEmail.eq(norm_email.clone()))
        .one(&st.db)
        .await?
    {
        Some(u) => u,
        None => create_external_user(&st, &email, &norm_email, display.as_deref(), p.name).await?,
    };

    if account.role == Role::Banned {
        return Err(AppError::Forbidden);
    }
    if !oauth_account_active(account.role, account.email_confirmed) {
        return Err(AppError::Unauthorized);
    }

    // Anti-cheat login gate (RSCTF `CheckAntiCheatConflictAsync`). The OAuth
    // redirect flow collects no browser fingerprint, so only the IP checks apply
    // (fingerprint = None). A conflict records a block and — mirroring RSCTF's
    // `OAuthError("anti_cheat")` — bounces to the login page without minting the
    // cookie (the outer handler turns this Err into `login_redirect()`).
    let current_ip = anti_cheat::client_ip(&headers, Some(peer.ip()));
    let policy = anti_cheat::load_policy_flags(&st.db).await?;
    if anti_cheat::check_login_conflict(&st.db, &policy, &account, current_ip.as_deref(), None)
        .await?
        .is_some()
    {
        return Err(AppError::Forbidden);
    }

    let id = account.id;
    let role = account.role;
    let user_name = account.user_name.clone().unwrap_or_default();
    let security_stamp = account
        .security_stamp
        .clone()
        .filter(|stamp| !stamp.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let needs_security_stamp = account.security_stamp.as_deref().is_none_or(str::is_empty);

    // Record the sign-in, matching the password login flow (RSCTF stamps the IP
    // via `UpdateByHttpContext`). Only overwrite `ip` when we captured one.
    let mut am: user::ActiveModel = account.into();
    am.last_signed_in_utc = Set(Utc::now());
    if needs_security_stamp {
        am.security_stamp = Set(Some(security_stamp.clone()));
    }
    if let Some(ip) = current_ip.as_deref() {
        am.ip = Set(ip.to_string());
    }
    am.update(&st.db).await?;

    // Issue the session cookie and land back on the requested page.
    let jwt = st.token.issue(id, role, &user_name, &security_stamp)?;
    let cookie = set_session_cookie(&jwt, st.config.jwt_ttl_secs, st.config.cookie_secure);
    let mut resp = Redirect::to(&return_url).into_response();
    let value = HeaderValue::from_str(&cookie)
        .map_err(|e| AppError::internal(format!("invalid Set-Cookie: {e}")))?;
    resp.headers_mut().insert(SET_COOKIE, value);
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Account provisioning
// ---------------------------------------------------------------------------

/// Create a new `Role::User` account for an externally-authenticated identity.
/// External users have no usable password, so a random one is hashed and stored;
/// the email is trusted (provider-verified) and marked confirmed. Field layout
/// mirrors `account::register`.
async fn create_external_user(
    st: &SharedState,
    email: &str,
    norm_email: &str,
    display: Option<&str>,
    provider: &str,
) -> Result<user::Model, AppError> {
    let domain_list = crate::controllers::account::load_email_domain_list(st).await?;
    if !crate::controllers::account::verify_email_domain(email, &domain_list) {
        return Err(AppError::bad_request("email domain is not allowed"));
    }
    let base = derive_base_name(display, email, provider);
    let now = Utc::now();
    let id = Uuid::now_v7();
    // No password login for external users — hash an unguessable random value so
    // the column is never empty and cannot be used to authenticate.
    let password_hash = hash_password_async(Uuid::new_v4().to_string()).await?;

    let txn = crate::controllers::account::locked_registration_transaction(st).await?;
    let has_admin = user::Entity::find()
        .filter(user::Column::Role.eq(Role::Admin))
        .count(&txn)
        .await?
        > 0;
    if !has_admin {
        txn.rollback().await?;
        return Err(AppError::bad_request(
            "bootstrap an administrator with password registration before OAuth provisioning",
        ));
    }
    let allow_register = config::Entity::find_by_id("AccountPolicy:AllowRegister".to_string())
        .one(&txn)
        .await?
        .and_then(|row| row.value)
        .map(|value| value == "true")
        .unwrap_or(st.config.account.allow_register);
    if !allow_register {
        txn.rollback().await?;
        return Err(AppError::bad_request("registration is disabled"));
    }
    let active_on_register =
        config::Entity::find_by_id("AccountPolicy:ActiveOnRegister".to_string())
            .one(&txn)
            .await?
            .and_then(|row| row.value)
            .map(|value| value == "true")
            .unwrap_or(st.config.account.active_on_register);
    // The pre-lock callback lookup may race another successful provider callback.
    if let Some(existing) = user::Entity::find()
        .filter(user::Column::NormalizedEmail.eq(norm_email))
        .one(&txn)
        .await?
    {
        txn.rollback().await?;
        return Ok(existing);
    }
    let user_name = unique_user_name(&txn, &base).await?;
    let norm_name = user_name.to_uppercase();

    let am = user::ActiveModel {
        id: Set(id),
        user_name: Set(Some(user_name.clone())),
        normalized_user_name: Set(Some(norm_name)),
        email: Set(Some(email.to_string())),
        normalized_email: Set(Some(norm_email.to_string())),
        email_confirmed: Set(active_on_register),
        password_hash: Set(Some(password_hash)),
        security_stamp: Set(Some(Uuid::new_v4().to_string())),
        concurrency_stamp: Set(Some(Uuid::new_v4().to_string())),
        phone_number: Set(None),
        phone_number_confirmed: Set(false),
        two_factor_enabled: Set(false),
        lockout_end: Set(None),
        lockout_enabled: Set(false),
        access_failed_count: Set(0),
        role: Set(Role::User),
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
    let saved = am.insert(&txn).await?;
    txn.commit().await?;
    Ok(saved)
}

/// Derive the base username: the provider display name, else the email local
/// part, else the provider name; trimmed and capped at 24 chars.
fn derive_base_name(display: Option<&str>, email: &str, provider: &str) -> String {
    let mut base = display
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            email
                .split('@')
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| provider.to_string());

    if base.chars().count() > 24 {
        base = base.chars().take(24).collect();
    }
    if base.trim().is_empty() {
        base = provider.to_string();
    }
    base
}

/// Return `base` if free, else `base_NNNN` with a random 4-digit suffix, probing
/// up to 50 times before falling back to a uuid suffix (RSCTF parity).
async fn unique_user_name<C: ConnectionTrait>(db: &C, base: &str) -> Result<String, AppError> {
    if !name_taken(db, base).await? {
        return Ok(base.to_string());
    }
    for _ in 0..50 {
        let candidate = format!("{base}_{}", rand_suffix());
        if !name_taken(db, &candidate).await? {
            return Ok(candidate);
        }
    }
    Ok(format!("{base}_{}", Uuid::new_v4().simple()))
}

async fn name_taken<C: ConnectionTrait>(db: &C, name: &str) -> Result<bool, AppError> {
    Ok(user::Entity::find()
        .filter(user::Column::NormalizedUserName.eq(name.to_uppercase()))
        .one(db)
        .await?
        .is_some())
}

/// A `[1000, 10000)` suffix drawn from random uuid bytes (avoids pulling in the
/// `rand` crate, which is not a direct dependency here).
fn rand_suffix() -> u32 {
    let bytes = Uuid::new_v4().into_bytes();
    1000 + (u16::from_le_bytes([bytes[0], bytes[1]]) as u32 % 9000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unverified_provider_email() {
        let discord: UserInfoResponse =
            serde_json::from_str(r#"{"email":"user@example.test","verified":false}"#).unwrap();
        assert!(!provider_email_verified("discord", &discord));

        let google: UserInfoResponse =
            serde_json::from_str(r#"{"email":"user@example.test","email_verified":true}"#).unwrap();
        assert!(provider_email_verified("google", &google));
        assert!(!provider_email_verified("discord", &google));
    }

    #[test]
    fn oauth_state_cookie_has_browser_binding_attributes() {
        let cookie = oauth_state_cookie("state", OAUTH_STATE_TTL, true);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn return_url_stays_same_origin() {
        assert_eq!(sanitize_return(Some("/games/1")), "/games/1");
        assert_eq!(sanitize_return(Some("//evil.test")), "/");
        assert_eq!(sanitize_return(Some("/\\evil.test")), "/");
    }

    #[test]
    fn oauth_never_activates_pending_or_banned_accounts() {
        assert!(oauth_account_active(Role::User, true));
        assert!(!oauth_account_active(Role::User, false));
        assert!(!oauth_account_active(Role::Banned, true));
    }
}
