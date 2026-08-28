//! Ported from RSCTF `Controllers/ApiTokenController.cs` (+ `ApiTokenRepository`).
//!
//! Admin-only management of API tokens for programmatic access. Route prefix
//! `/api/tokens`. The plaintext secret is generated once, returned once, and
//! only its SHA-256 hash is ever persisted in `ApiTokens.token_hash`.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::{AdminUser, CurrentUser};
use crate::models::data::api_token;
use crate::utils::codec::{random_token, sha256_str};
use crate::utils::enums::Role;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{ArrayResponse, MessageResponse, PageParams, RequestResponse};

pub(crate) const PERSONAL_TOKEN_PREFIX: &str = "rsctf_pat_v1_";
const MAX_PERSONAL_TOKEN_BYTES: usize = 128;
const PERSONAL_TOKEN_SECRET_BYTES: usize = 32;
const PERSONAL_TOKEN_SECRET_CHARS: usize = 43;
const MAX_TOKENS_PER_OWNER: i64 = 32;
const LIST_LIMIT: i64 = 100;
const NEGATIVE_LOOKUP_TTL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Clone)]
pub(crate) struct VerifiedPersonalToken {
    pub user: CurrentUser,
    pub partition_key: String,
    pub audience: String,
}

pub(crate) fn is_well_formed(token: &str) -> bool {
    let Some(secret) = token.strip_prefix(PERSONAL_TOKEN_PREFIX) else {
        return false;
    };
    secret.len() == PERSONAL_TOKEN_SECRET_CHARS
        && token.len() <= MAX_PERSONAL_TOKEN_BYTES
        && secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) async fn authenticate(
    st: &SharedState,
    token: &str,
) -> AppResult<VerifiedPersonalToken> {
    if !is_well_formed(token) {
        return Err(AppError::Unauthorized);
    }
    let hash = sha256_str(token);
    let negative_key = format!("_PersonalTokenNegative_{hash}");
    if st.cache.get(&negative_key).await.is_some() {
        return Err(AppError::Unauthorized);
    }
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Option<Uuid>,
            bool,
            bool,
            String,
            Option<String>,
            i16,
            Option<String>,
            Option<String>,
        ),
    >(
        r#"SELECT token.id, token.creator_id,
                  token.expires_at IS NOT NULL AND token.expires_at <= clock_timestamp(),
                  token.is_revoked,
                  token.audience, token.security_stamp_hash, account.role,
                  account.user_name, account.security_stamp
             FROM "ApiTokens" token
             JOIN "AspNetUsers" account ON account.id = token.creator_id
            WHERE token.token_hash = $1 AND token.is_revoked = FALSE"#,
    )
    .bind(hash)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(row) = row else {
        st.cache
            .set(&negative_key, b"1", Some(NEGATIVE_LOOKUP_TTL))
            .await;
        return Err(AppError::Unauthorized);
    };
    let Some(owner_id) = row.1 else {
        st.cache
            .set(&negative_key, b"1", Some(NEGATIVE_LOOKUP_TTL))
            .await;
        return Err(AppError::Unauthorized);
    };
    let live_stamp = row.8.ok_or(AppError::Unauthorized)?;
    let live_stamp_hash = sha256_str(&live_stamp);
    if row.3
        || row.2
        || row.4 != "admin_api"
        || row.5.as_deref() != Some(live_stamp_hash.as_str())
        || row.6 != Role::Admin as i16
    {
        st.cache
            .set(&negative_key, b"1", Some(NEGATIVE_LOOKUP_TTL))
            .await;
        return Err(AppError::Unauthorized);
    }

    // Throttle metadata writes so a hot API client does not turn every read
    // into a row update. The predicate is concurrency-safe across replicas.
    sqlx::query(
        r#"UPDATE "ApiTokens" SET last_used_at = clock_timestamp()
            WHERE id = $1 AND (last_used_at IS NULL
                  OR last_used_at < clock_timestamp() - INTERVAL '5 minutes')"#,
    )
    .bind(row.0)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(VerifiedPersonalToken {
        user: CurrentUser {
            id: owner_id,
            role: Role::Admin,
            name: row.7.unwrap_or_default(),
            security_stamp: live_stamp,
        },
        partition_key: format!("personal-token:{}", row.0),
        audience: row.4,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_token_grammar_is_versioned_bounded_and_non_overlapping() {
        let valid = format!(
            "{PERSONAL_TOKEN_PREFIX}{}",
            "a".repeat(PERSONAL_TOKEN_SECRET_CHARS)
        );
        assert!(is_well_formed(&valid));
        assert!(!is_well_formed("rsctf_ad_v1_abc"));
        assert!(!is_well_formed("header.payload.signature"));
        assert!(!is_well_formed("rsctf_pat_v1_"));
        assert!(!is_well_formed(&format!(
            "{PERSONAL_TOKEN_PREFIX}{}",
            "a".repeat(PERSONAL_TOKEN_SECRET_CHARS - 1)
        )));
        assert!(!is_well_formed(&format!(
            "{PERSONAL_TOKEN_PREFIX}{}",
            "a".repeat(PERSONAL_TOKEN_SECRET_CHARS + 1)
        )));
        assert!(!is_well_formed(&format!(
            "{PERSONAL_TOKEN_PREFIX}{}",
            "a".repeat(MAX_PERSONAL_TOKEN_BYTES)
        )));
        assert!(!is_well_formed("rsctf_pat_v1_not+base64url"));
    }
}

/// Request body for `POST /api/tokens`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiTokenCreateModel {
    pub name: String,
    /// Duration for which the token stays valid, in days. `None`/`0` = never expires.
    #[serde(default)]
    pub expires_in: Option<u32>,
}

/// Metadata for a single token — never carries the secret or its hash.
/// Matches Api.ts `ApiToken`: timestamps are serialized as `uint64` Unix
/// **milliseconds** (numbers) via the global `DateTimeOffsetJsonConverter`,
/// not ISO strings.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiToken {
    pub id: Uuid,
    pub name: String,
    pub creator_id: Option<Uuid>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub created_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_used_at: Option<DateTime<Utc>>,
    pub is_revoked: bool,
    /// The username of the token's creator. Not resolved here (no join).
    pub creator: Option<String>,
}

impl From<api_token::Model> for ApiToken {
    fn from(m: api_token::Model) -> Self {
        Self {
            id: m.id,
            name: m.name,
            creator_id: m.creator_id,
            created_at: m.created_at,
            expires_at: m.expires_at,
            last_used_at: m.last_used_at,
            is_revoked: m.is_revoked,
            creator: None,
        }
    }
}

/// Response for `POST /api/tokens` — the plaintext secret is shown exactly once.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiTokenResponse {
    /// The plaintext bearer secret. Store it now; it cannot be retrieved later.
    pub token: String,
    pub info: ApiToken,
}

fn private_token_response(model: ApiTokenResponse) -> Response {
    let mut response = RequestResponse::ok(model).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn generated_plaintext_is_private_and_non_cacheable() {
        let response = private_token_response(ApiTokenResponse {
            token: format!(
                "{PERSONAL_TOKEN_PREFIX}{}",
                "a".repeat(PERSONAL_TOKEN_SECRET_CHARS)
            ),
            info: ApiToken {
                id: Uuid::nil(),
                name: "automation".to_string(),
                creator_id: Some(Uuid::nil()),
                created_at: Utc::now(),
                expires_at: None,
                last_used_at: None,
                is_revoked: false,
                creator: Some("admin".to_string()),
            },
        });
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "private, no-store"
        );
        assert_eq!(response.headers().get(header::PRAGMA).unwrap(), "no-cache");
    }
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/tokens", get(list_tokens).post(generate_token))
        .route("/api/tokens/{id}", delete(revoke_token))
        .route("/api/tokens/{id}/restore", post(restore_token))
}

/// `GET /api/tokens` — list all tokens, newest first. Never exposes the secret.
pub async fn list_tokens(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(page): Query<PageParams>,
) -> AppResult<ArrayResponse<ApiToken>> {
    let total: i64 = sqlx::query_scalar(r#"SELECT COUNT(*)::BIGINT FROM "ApiTokens""#)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            Option<Uuid>,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            bool,
            Option<String>,
        ),
    >(
        r#"SELECT token.id, token.name, token.creator_id, token.created_at,
                  token.expires_at, token.last_used_at, token.is_revoked,
                  account.user_name
            FROM "ApiTokens" token
             LEFT JOIN "AspNetUsers" account ON account.id = token.creator_id
            ORDER BY token.created_at DESC, token.id DESC
            LIMIT $1 OFFSET $2"#,
    )
    .bind(i64::try_from(page.count.clamp(1, LIST_LIMIT as u64)).unwrap_or(LIST_LIMIT))
    .bind(i64::try_from(page.skip).unwrap_or(i64::MAX))
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let data = rows
        .into_iter()
        .map(|row| ApiToken {
            id: row.0,
            name: row.1,
            creator_id: row.2,
            created_at: row.3,
            expires_at: row.4,
            last_used_at: row.5,
            is_revoked: row.6,
            creator: row.7,
        })
        .collect();
    Ok(ArrayResponse::new(data, total))
}

/// `POST /api/tokens` — generate a new token and return the plaintext secret once.
pub async fn generate_token(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Json(model): Json<ApiTokenCreateModel>,
) -> AppResult<Response> {
    let name = model.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("Token name is required"));
    }
    if name.chars().count() > 128 {
        return Err(AppError::bad_request(
            "Token name must be at most 128 characters",
        ));
    }

    let secret = format!(
        "{PERSONAL_TOKEN_PREFIX}{}",
        random_token(PERSONAL_TOKEN_SECRET_BYTES)
    );
    let token_hash = sha256_str(&secret);

    let now = Utc::now();
    // Guard against a huge `expiresIn` overflowing the date arithmetic (chrono
    // `Add` panics on overflow) — reject with a 400 instead.
    if model.expires_in.is_some_and(|days| days > 3_650) {
        return Err(AppError::bad_request("expiresIn must not exceed 3650 days"));
    }
    let expires_at = match model.expires_in {
        Some(days) if days > 0 => {
            let dur = Duration::try_days(days as i64)
                .ok_or_else(|| AppError::bad_request("expiresIn is too large"))?;
            Some(
                now.checked_add_signed(dur)
                    .ok_or_else(|| AppError::bad_request("expiresIn is too large"))?,
            )
        }
        _ => None,
    };

    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let live = sqlx::query_as::<_, (i16, Option<String>)>(
        r#"SELECT role, security_stamp FROM "AspNetUsers"
            WHERE id = $1 FOR UPDATE"#,
    )
    .bind(user.id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or(AppError::Unauthorized)?;
    if live.0 != Role::Admin as i16 || live.1.as_deref() != Some(user.security_stamp.as_str()) {
        return Err(AppError::Unauthorized);
    }
    sqlx::query(
        r#"DELETE FROM "ApiTokens" old
            WHERE old.creator_id = $1 AND old.is_revoked = TRUE
              AND (old.expires_at < clock_timestamp() - INTERVAL '30 days'
                   OR old.id IN (
                       SELECT id FROM "ApiTokens"
                        WHERE creator_id = $1 AND is_revoked = TRUE
                        ORDER BY created_at DESC, id DESC
                       OFFSET 16
                   ))"#,
    )
    .bind(user.id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let count: i64 =
        sqlx::query_scalar(r#"SELECT COUNT(*)::BIGINT FROM "ApiTokens" WHERE creator_id = $1"#)
            .bind(user.id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if count >= MAX_TOKENS_PER_OWNER {
        return Err(AppError::bad_request("API token creation limit reached"));
    }
    let token_id = Uuid::now_v7();
    sqlx::query(
        r#"INSERT INTO "ApiTokens"
                  (id, name, token_hash, creator_id, created_at, expires_at,
                   last_used_at, is_revoked, audience, security_stamp_hash)
           VALUES ($1, $2, $3, $4, $5, $6, NULL, FALSE,
                   'admin_api', $7)"#,
    )
    .bind(token_id)
    .bind(&name)
    .bind(token_hash)
    .bind(user.id)
    .bind(now)
    .bind(expires_at)
    .bind(sha256_str(&user.security_stamp))
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let saved = ApiToken {
        id: token_id,
        name,
        creator_id: Some(user.id),
        created_at: now,
        expires_at,
        last_used_at: None,
        is_revoked: false,
        creator: Some(user.name),
    };

    Ok(private_token_response(ApiTokenResponse {
        token: secret,
        info: saved,
    }))
}

/// `DELETE /api/tokens/{id}` — soft revoke (set `is_revoked = true`).
pub async fn revoke_token(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let result = sqlx::query(
        r#"UPDATE "ApiTokens" SET is_revoked = TRUE
            WHERE id = $1 AND is_revoked = FALSE"#,
    )
    .bind(id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if result.rows_affected() == 0 {
        let exists: bool =
            sqlx::query_scalar(r#"SELECT EXISTS(SELECT 1 FROM "ApiTokens" WHERE id = $1)"#)
                .bind(id)
                .fetch_one(st.pg())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        if !exists {
            return Err(AppError::not_found("Token not found"));
        }
    }
    Ok(MessageResponse::ok("Token revoked"))
}

/// `POST /api/tokens/{id}/restore` — un-revoke (set `is_revoked = false`).
pub async fn restore_token(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let row = sqlx::query_as::<
        _,
        (
            bool,
            bool,
            Option<String>,
            Option<Uuid>,
            Option<String>,
            Option<i16>,
            String,
        ),
    >(
        r#"SELECT token.is_revoked,
                  token.expires_at IS NOT NULL AND token.expires_at <= clock_timestamp(),
                  token.security_stamp_hash, token.creator_id,
                  account.security_stamp, account.role, token.token_hash
             FROM "ApiTokens" token
             LEFT JOIN "AspNetUsers" account ON account.id = token.creator_id
            WHERE token.id = $1
            FOR UPDATE OF token"#,
    )
    .bind(id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Token not found"))?;
    if row.1 {
        return Err(AppError::conflict("Expired API tokens cannot be restored"));
    }
    let live_stamp = row
        .4
        .as_deref()
        .ok_or_else(|| AppError::conflict("API token owner is unavailable"))?;
    if row.2.as_deref() != Some(sha256_str(live_stamp).as_str())
        || row.3.is_none()
        || row.5 != Some(Role::Admin as i16)
    {
        return Err(AppError::conflict(
            "Legacy or owner-invalid API tokens cannot be restored",
        ));
    }
    if row.0 {
        sqlx::query(r#"UPDATE "ApiTokens" SET is_revoked = FALSE WHERE id = $1"#)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    st.cache
        .remove(&format!("_PersonalTokenNegative_{}", row.6))
        .await;
    Ok(MessageResponse::ok("Token restored"))
}

#[cfg(test)]
#[path = "api_token_tests.rs"]
mod postgres_tests;
