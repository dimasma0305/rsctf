//! Ported from RSCTF `Middlewares/PrivilegeAuthentication.cs` — resolves the
//! current principal from the bearer JWT and enforces role requirements as
//! axum extractors.

use axum::extract::{FromRef, FromRequestParts};
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::request::Parts;
use sea_orm::ActiveEnum;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::services::token::Claims;
use crate::utils::enums::Role;
use crate::utils::error::AppError;

/// Cookie used by browser sessions.
pub const SESSION_COOKIE: &str = "RSCTF_Token";
const MAX_SESSION_TOKEN_BYTES: usize = 4_096;
/// Live authorization is deliberately much shorter-lived than ordinary read
/// caches. A ban, role edit, or security-stamp rotation can therefore remain
/// visible for at most one second on a replica that just served a cache hit.
const LIVE_AUTHORIZATION_TTL: std::time::Duration = std::time::Duration::from_secs(1);

pub fn session_cookie_value(cookies: &str) -> Option<&str> {
    cookies.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == SESSION_COOKIE && !value.is_empty()).then_some(value)
    })
}

/// `Set-Cookie` value that persists a session for `ttl_secs`.
pub fn set_session_cookie(token: &str, ttl_secs: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax{secure}; Max-Age={ttl_secs}")
}

/// `Set-Cookie` value that clears the session (used on logout).
pub fn clear_session_cookie(secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax{secure}; Max-Age=0")
}

use serde::{Deserialize, Serialize};

/// The authenticated principal, resolved from `Authorization: Bearer <jwt>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentUser {
    pub id: Uuid,
    pub role: Role,
    pub name: String,
}

/// Bounded-cache representation of the four-column authorization projection.
/// Missing users are cached too, preventing a burst of already-verified tokens
/// for a deleted account from repeatedly reaching Postgres.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum CachedAuthorization {
    Found {
        user: CurrentUser,
        security_stamp: Option<String>,
    },
    Missing,
}

static LIVE_AUTHORIZATION_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

fn live_authorization_window() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn live_authorization_cache_key(id: Uuid, window: u64) -> String {
    // The one-second window is part of the key so promoting an almost-expired
    // Redis entry into the process L1 cannot extend authorization staleness for
    // another L1 TTL. Old generations expire through the bounded Cache.
    format!("_LiveAuthorization_{id}_{window:016x}")
}

fn decode_cached_authorization(bytes: &[u8]) -> Option<CachedAuthorization> {
    serde_json::from_slice(bytes).ok()
}

async fn cached_authorization(app: &SharedState, key: &str) -> Option<CachedAuthorization> {
    let bytes = app.cache.get(key).await?;
    decode_cached_authorization(&bytes)
}

/// Load the live account projection once per user and one-second window. The
/// shared `Cache` supplies bounded storage (and cross-replica Redis when
/// configured); `SingleFlight` collapses a synchronized miss to one detached DB
/// read per replica without holding a blocking guard across `.await`.
async fn load_cached_authorization(
    app: &SharedState,
    id: Uuid,
) -> Result<CachedAuthorization, AppError> {
    let key = live_authorization_cache_key(id, live_authorization_window());
    if let Some(entry) = cached_authorization(app, &key).await {
        return Ok(entry);
    }

    let app = app.clone();
    let fill_key = key.clone();
    let encoded = LIVE_AUTHORIZATION_SF
        .run(&key, move || async move {
            if let Some(bytes) = app.cache.get(&fill_key).await {
                if decode_cached_authorization(&bytes).is_some() {
                    return Some(bytes);
                }
            }

            let row = match sqlx::query_as::<_, (Uuid, i16, Option<String>, Option<String>)>(
                r#"SELECT id, role, user_name, security_stamp
                     FROM "AspNetUsers"
                    WHERE id = $1"#,
            )
            .bind(id)
            .fetch_optional(app.pg())
            .await
            {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(user_id = %id, %error, "live authorization cache fill failed");
                    return None;
                }
            };

            let entry = match row {
                Some((id, role, name, security_stamp)) => {
                    let role = match <Role as ActiveEnum>::try_from_value(&role) {
                        Ok(role) => role,
                        Err(error) => {
                            tracing::warn!(user_id = %id, %error, "invalid live authorization role");
                            return None;
                        }
                    };
                    CachedAuthorization::Found {
                        user: CurrentUser {
                            id,
                            role,
                            name: name.unwrap_or_default(),
                        },
                        security_stamp,
                    }
                }
                None => CachedAuthorization::Missing,
            };
            let bytes = match serde_json::to_vec(&entry) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(user_id = %id, %error, "live authorization cache encoding failed");
                    return None;
                }
            };
            app.cache
                .set(&fill_key, &bytes, Some(LIVE_AUTHORIZATION_TTL))
                .await;
            Some(bytes::Bytes::from(bytes))
        })
        .await
        .ok_or_else(|| AppError::internal("live authorization cache fill failed"))?;

    decode_cached_authorization(&encoded)
        .ok_or_else(|| AppError::internal("invalid live authorization cache entry"))
}

fn authorize_cached_entry(
    entry: CachedAuthorization,
    expected_id: Uuid,
    expected_stamp: &str,
) -> Result<CurrentUser, AppError> {
    let CachedAuthorization::Found {
        user,
        security_stamp,
    } = entry
    else {
        return Err(AppError::Unauthorized);
    };
    if user.id != expected_id || security_stamp.as_deref() != Some(expected_stamp) {
        return Err(AppError::Unauthorized);
    }
    if user.role == Role::Banned {
        return Err(AppError::Forbidden);
    }
    Ok(user)
}

impl CurrentUser {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
    pub fn is_monitor(&self) -> bool {
        matches!(self.role, Role::Monitor | Role::Admin)
    }
    /// Enforce a minimum role (ordered Banned < User < Monitor < Admin).
    pub fn require_role(&self, min: Role) -> Result<(), AppError> {
        if self.role.into_value() >= min.into_value() && self.role != Role::Banned {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}

/// Extract the raw session token from either `Authorization: Bearer <jwt>` or
/// an rsctf session cookie.
pub(crate) fn session_token(headers: &axum::http::HeaderMap) -> Option<String> {
    // 1. Authorization: Bearer <jwt> (A&D tokens / API clients)
    if let Some(h) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        if let Some(t) = h
            .strip_prefix("Bearer ")
            .or_else(|| h.strip_prefix("bearer "))
        {
            let token = t.trim();
            return (!token.is_empty() && token.len() <= MAX_SESSION_TOKEN_BYTES)
                .then(|| token.to_string());
        }
    }
    // 2. Session cookie (the SPA's normal auth)
    let cookies = headers.get(COOKIE).and_then(|v| v.to_str().ok())?;
    session_cookie_value(cookies)
        .filter(|token| token.len() <= MAX_SESSION_TOKEN_BYTES)
        .map(str::to_owned)
}

/// Signature-verified claims cached by the global limiter for the request. The
/// live authorization projection is still checked below, so bans, role edits,
/// and security-stamp rotations take effect within the documented one-second
/// cache window without verifying the same JWT twice.
#[derive(Clone)]
pub(crate) struct VerifiedSessionClaims(pub(crate) Claims);

/// Verify a raw JWT and resolve its live principal. This is shared by HTTP
/// extractors and WebSocket hubs so neither path ever authorizes from stale role
/// claims. Credential changes and logout rotate `security_stamp`; a mismatch
/// invalidates the session after at most the one-second live-row cache window.
pub async fn authenticate_token(app: &SharedState, token: &str) -> Result<CurrentUser, AppError> {
    let claims = app.token.verify(token)?;
    authenticate_claims(app, claims).await
}

async fn authenticate_claims(app: &SharedState, claims: Claims) -> Result<CurrentUser, AppError> {
    let id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let entry = load_cached_authorization(app, id).await?;
    authorize_cached_entry(entry, id, &claims.stamp)
}

/// Resolve the live principal for a request: verify the session JWT, then re-load
/// the user row so bans, role changes, and security-stamp rotations take effect
/// no later than one second after the database change.
async fn live_user(parts: &mut Parts, app: &SharedState) -> Result<CurrentUser, AppError> {
    if let Some(user) = parts.extensions.get::<CurrentUser>() {
        return Ok(user.clone());
    }
    // A verified participation token is intentionally not a user session. The
    // optional-user extractor should not spend another HMAC verification trying
    // to reinterpret it as a JWT.
    if parts
        .extensions
        .get::<crate::services::ad::api_token::VerifiedTeamToken>()
        .is_some()
        || parts
            .extensions
            .get::<crate::services::ad::api_token::RejectedTeamToken>()
            .is_some()
    {
        return Err(AppError::Unauthorized);
    }
    let user = if let Some(claims) = parts.extensions.get::<VerifiedSessionClaims>() {
        authenticate_claims(app, claims.0.clone()).await?
    } else {
        let token = session_token(&parts.headers).ok_or(AppError::Unauthorized)?;
        authenticate_token(app, &token).await?
    };
    if let Some(activity) = parts
        .extensions
        .get::<crate::middlewares::user_activity::RequestActivityContext>()
    {
        activity.mark_authenticated(user.id);
    }
    parts.extensions.insert(user.clone());
    Ok(user)
}

impl<S> FromRequestParts<S> for CurrentUser
where
    SharedState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = <SharedState as FromRef<S>>::from_ref(state);
        live_user(parts, &app).await
    }
}

/// Optional principal — `None` when unauthenticated instead of rejecting.
#[derive(Debug, Clone)]
pub struct MaybeUser(pub Option<CurrentUser>);

impl<S> FromRequestParts<S> for MaybeUser
where
    SharedState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = <SharedState as FromRef<S>>::from_ref(state);
        // Same live-row resolution as `CurrentUser`, but optional: an absent or
        // invalid token, a deleted account, or a banned row all resolve to `None`
        // rather than rejecting. This keeps a demoted/banned principal from
        // retaining stale privileges (e.g. hidden-game visibility gated on
        // `MaybeUser::is_monitor`) via the still-valid 7-day token claim.
        Ok(MaybeUser(live_user(parts, &app).await.ok()))
    }
}

/// Extractor that requires `Role::Admin`.
pub struct AdminUser(pub CurrentUser);

impl<S> FromRequestParts<S> for AdminUser
where
    SharedState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        user.require_role(Role::Admin)?;
        Ok(AdminUser(user))
    }
}

/// Extractor that requires at least `Role::Monitor`.
pub struct MonitorUser(pub CurrentUser);

impl<S> FromRequestParts<S> for MonitorUser
where
    SharedState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        user.require_role(Role::Monitor)?;
        Ok(MonitorUser(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookie_is_secure_by_default_at_call_sites() {
        let cookie = set_session_cookie("token", 60, true);
        assert!(cookie.contains("; HttpOnly"));
        assert!(cookie.contains("; SameSite=Lax"));
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains("; Path=/"));
    }

    #[test]
    fn local_http_cookie_can_explicitly_disable_secure() {
        assert!(!set_session_cookie("token", 60, false).contains("; Secure"));
        assert!(clear_session_cookie(true).contains("; Secure"));
    }

    #[test]
    fn session_cookie_parser_accepts_only_the_rsctf_cookie() {
        assert_eq!(
            session_cookie_value("Other=x; RSCTF_Token=current-session"),
            Some("current-session")
        );
        assert_eq!(
            session_cookie_value("Unknown_Token=stale-session; RSCTF_Token=current-session"),
            Some("current-session")
        );
        assert_eq!(session_cookie_value("Unknown_Token=stale-session"), None);
        assert_eq!(session_cookie_value("NotRSCTF_Token=session"), None);
    }

    #[test]
    fn cached_authorization_uses_the_live_role_name_and_stamp() {
        let id = Uuid::new_v4();
        let entry = CachedAuthorization::Found {
            user: CurrentUser {
                id,
                role: Role::Monitor,
                name: "live-name".to_string(),
            },
            security_stamp: Some("live-stamp".to_string()),
        };
        let user = authorize_cached_entry(entry, id, "live-stamp").unwrap();
        assert_eq!(user.role, Role::Monitor);
        assert_eq!(user.name, "live-name");
    }

    #[test]
    fn cached_authorization_rejects_rotated_deleted_and_banned_accounts() {
        let id = Uuid::new_v4();
        let found = |role| CachedAuthorization::Found {
            user: CurrentUser {
                id,
                role,
                name: "player".to_string(),
            },
            security_stamp: Some("new-stamp".to_string()),
        };

        assert!(matches!(
            authorize_cached_entry(found(Role::User), id, "old-stamp"),
            Err(AppError::Unauthorized)
        ));
        assert!(matches!(
            authorize_cached_entry(found(Role::User), Uuid::new_v4(), "new-stamp"),
            Err(AppError::Unauthorized)
        ));
        assert!(matches!(
            authorize_cached_entry(CachedAuthorization::Missing, id, "new-stamp"),
            Err(AppError::Unauthorized)
        ));
        assert!(matches!(
            authorize_cached_entry(found(Role::Banned), id, "new-stamp"),
            Err(AppError::Forbidden)
        ));
    }

    #[test]
    fn live_authorization_cache_window_and_key_are_strictly_bounded() {
        assert!(LIVE_AUTHORIZATION_TTL > std::time::Duration::ZERO);
        assert!(LIVE_AUTHORIZATION_TTL <= std::time::Duration::from_secs(1));
        let first = live_authorization_cache_key(Uuid::nil(), 0);
        let second = live_authorization_cache_key(Uuid::from_u128(u128::MAX), u64::MAX);
        assert_eq!(first.len(), second.len());
        assert!(first.len() < 80);
    }
}
