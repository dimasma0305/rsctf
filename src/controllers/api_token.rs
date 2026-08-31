//! Administrator-managed personal API credentials.
//!
//! Plaintext credentials are returned once. PostgreSQL stores only their
//! SHA-256 digest and an owner security-generation digest.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::middlewares::rate_limiter::{limited, Policy};
use crate::services::managed_api_token;
use crate::utils::codec::random_token;
use crate::utils::enums::Role;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{MessageResponse, RequestResponse};

const DEFAULT_PAGE_SIZE: u16 = 50;
const MAX_PAGE_SIZE: u16 = 100;
const MAX_PAGE: u32 = 100;
const MAX_ACTIVE_TOKENS_PER_OWNER: i64 = 32;

const LIST_SQL: &str = r#"
SELECT token.id,
       token.name,
       token.creator_id,
       token.created_at,
       token.expires_at,
       token.last_used_at,
       token.is_revoked,
       account.user_name,
       token.audience,
       token.scopes
  FROM "ApiTokens" token
  LEFT JOIN "AspNetUsers" account ON account.id = token.creator_id
 ORDER BY token.created_at DESC, token.id DESC
 LIMIT $1 OFFSET $2
"#;

const CREATE_SQL: &str = r#"
INSERT INTO "ApiTokens"
       (id, name, token_hash, creator_id, created_at, expires_at,
        last_used_at, is_revoked, token_version, audience, scopes,
        owner_security_stamp_digest)
SELECT $1, $2, $3, account.id, $4, $5,
       NULL, FALSE, $6, $7, $8, $9
  FROM "AspNetUsers" account
 WHERE account.id = $10
   AND account.role = $11
   AND account.security_stamp = $12
   AND account.security_stamp <> ''
   AND (
       SELECT count(*)
         FROM "ApiTokens" existing
        WHERE existing.creator_id = account.id
          AND existing.token_version = $6
          AND NOT existing.is_revoked
          AND (existing.expires_at IS NULL OR existing.expires_at > clock_timestamp())
   ) < $13
RETURNING id, name, creator_id, created_at, expires_at, last_used_at,
          is_revoked, NULL::TEXT, audience, scopes
"#;

const REVOKE_SQL: &str = r#"
WITH target AS MATERIALIZED (
    SELECT id FROM "ApiTokens" WHERE id = $1
), updated AS (
    UPDATE "ApiTokens" token
       SET is_revoked = TRUE
      FROM target
     WHERE token.id = target.id
       AND NOT token.is_revoked
    RETURNING token.id
)
SELECT EXISTS(SELECT 1 FROM target), EXISTS(SELECT 1 FROM updated)
"#;

type ApiTokenRow = (
    Uuid,
    String,
    Option<Uuid>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    bool,
    Option<String>,
    String,
    Vec<String>,
);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiTokenCreateModel {
    pub name: String,
    /// Duration for which the token stays valid, in days. `None`/`0` = no
    /// calendar expiry; owner role/security-generation checks still apply.
    #[serde(default)]
    pub expires_in: Option<u32>,
    /// Explicit authority. Omitted credentials are read-only.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiTokenListQuery {
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub page_size: Option<u16>,
}

/// Safe metadata for one credential. It never includes the secret, its digest,
/// or its owner security-generation digest.
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
    pub creator: Option<String>,
    pub audience: String,
    pub scopes: Vec<String>,
}

impl From<ApiTokenRow> for ApiToken {
    fn from(row: ApiTokenRow) -> Self {
        Self {
            id: row.0,
            name: row.1,
            creator_id: row.2,
            created_at: row.3,
            expires_at: row.4,
            last_used_at: row.5,
            is_revoked: row.6,
            creator: row.7,
            audience: row.8,
            scopes: row.9,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiTokenResponse {
    /// Plaintext bearer secret. It cannot be retrieved again.
    pub token: String,
    pub info: ApiToken,
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/tokens",
            get(list_tokens).merge(limited(Policy::CredentialMutation, post(generate_token))),
        )
        .route(
            "/api/tokens/{id}",
            limited(Policy::CredentialMutation, delete(revoke_token)),
        )
        .route(
            "/api/tokens/{id}/restore",
            limited(Policy::CredentialMutation, post(restore_token)),
        )
}

pub async fn list_tokens(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Query(query): Query<ApiTokenListQuery>,
) -> AppResult<RequestResponse<Vec<ApiToken>>> {
    let page = query.page.unwrap_or(0);
    if page > MAX_PAGE {
        return Err(AppError::bad_request("page is outside the supported range"));
    }
    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return Err(AppError::bad_request("pageSize must be between 1 and 100"));
    }
    let offset = i64::from(page) * i64::from(page_size);
    let rows = sqlx::query_as::<_, ApiTokenRow>(LIST_SQL)
        .bind(i64::from(page_size))
        .bind(offset)
        .fetch_all(st.pg())
        .await
        .map_err(database_error)?;
    Ok(RequestResponse::ok(
        rows.into_iter().map(ApiToken::from).collect(),
    ))
}

pub async fn generate_token(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Json(model): Json<ApiTokenCreateModel>,
) -> AppResult<RequestResponse<ApiTokenResponse>> {
    let name = model.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::bad_request("Token name is required"));
    }
    if name.chars().count() > 128 {
        return Err(AppError::bad_request(
            "Token name must be at most 128 characters",
        ));
    }
    if user.security_stamp.is_empty() {
        return Err(AppError::conflict(
            "The administrator security generation is unavailable",
        ));
    }
    let scopes = normalize_scopes(model.scopes)?;
    let now = Utc::now();
    let expires_at = checked_expiry(now, model.expires_in)?;
    let secret = format!("{}{}", managed_api_token::PREFIX, random_token(32));
    let token_hash = managed_api_token::hash(&secret);
    let stamp_digest = managed_api_token::owner_stamp_digest(&user.security_stamp);
    let id = Uuid::now_v7();

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(database_error)?;
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        &mut transaction,
        &format!("managed-api-token-owner:{}", user.id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let saved = sqlx::query_as::<_, ApiTokenRow>(CREATE_SQL)
        .bind(id)
        .bind(name)
        .bind(&token_hash)
        .bind(now)
        .bind(expires_at)
        .bind(managed_api_token::VERSION)
        .bind(managed_api_token::AUDIENCE)
        .bind(&scopes)
        .bind(&stamp_digest)
        .bind(user.id)
        .bind(Role::Admin as i16)
        .bind(&user.security_stamp)
        .bind(MAX_ACTIVE_TOKENS_PER_OWNER)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or_else(|| {
            AppError::conflict(
                "The active token limit was reached or the administrator identity changed",
            )
        })?;
    transaction.commit().await.map_err(database_error)?;
    st.cache
        .remove(&managed_api_token::negative_cache_key(&token_hash))
        .await;
    let mut info: ApiToken = saved.into();
    info.creator = Some(user.name);

    Ok(RequestResponse::ok(ApiTokenResponse {
        token: secret,
        info,
    }))
}

pub async fn revoke_token(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let (exists, changed): (bool, bool) = sqlx::query_as(REVOKE_SQL)
        .bind(id)
        .fetch_one(st.pg())
        .await
        .map_err(database_error)?;
    if !exists {
        return Err(AppError::not_found("Token not found"));
    }
    Ok(MessageResponse::ok(if changed {
        "Token revoked"
    } else {
        "Token already revoked"
    }))
}

pub async fn restore_token(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(database_error)?;
    let initial: Option<(Option<Uuid>,)> =
        sqlx::query_as(r#"SELECT creator_id FROM "ApiTokens" WHERE id = $1"#)
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
    let creator_id = initial
        .ok_or_else(|| AppError::not_found("Token not found"))?
        .0
        .ok_or_else(|| AppError::conflict("Legacy tokens cannot be restored"))?;
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        &mut transaction,
        &format!("managed-api-token-owner:{creator_id}"),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let state: (
        bool,
        Option<DateTime<Utc>>,
        i16,
        Option<Vec<u8>>,
        Option<i16>,
        Option<String>,
        String,
    ) = sqlx::query_as(
        r#"SELECT token.is_revoked, token.expires_at, token.token_version,
                  token.owner_security_stamp_digest, account.role, account.security_stamp,
                  token.token_hash
             FROM "ApiTokens" token
             LEFT JOIN "AspNetUsers" account ON account.id = token.creator_id
            WHERE token.id = $1
              FOR UPDATE OF token"#,
    )
    .bind(id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if state.2 != managed_api_token::VERSION {
        return Err(AppError::conflict("Legacy tokens cannot be restored"));
    }
    let Some(current_stamp) = state
        .5
        .as_deref()
        .filter(|stamp| !stamp.is_empty() && state.4 == Some(Role::Admin as i16))
    else {
        return Err(AppError::conflict(
            "The token owner or security generation is no longer valid",
        ));
    };
    let expected_stamp_digest = managed_api_token::owner_stamp_digest(current_stamp);
    if state.3.as_deref() != Some(expected_stamp_digest.as_slice()) {
        return Err(AppError::conflict(
            "The token owner or security generation is no longer valid",
        ));
    }
    if state.1.is_some_and(|expires_at| expires_at <= Utc::now()) {
        return Err(AppError::conflict("Expired tokens cannot be restored"));
    }
    if !state.0 {
        transaction.commit().await.map_err(database_error)?;
        st.cache
            .remove(&managed_api_token::negative_cache_key(&state.6))
            .await;
        return Ok(MessageResponse::ok("Token already active"));
    }

    let active: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM "ApiTokens"
            WHERE creator_id = $1
              AND token_version = $2
              AND NOT is_revoked
              AND (expires_at IS NULL OR expires_at > clock_timestamp())"#,
    )
    .bind(creator_id)
    .bind(managed_api_token::VERSION)
    .fetch_one(&mut *transaction)
    .await
    .map_err(database_error)?;
    if active >= MAX_ACTIVE_TOKENS_PER_OWNER {
        return Err(AppError::conflict("The active token limit was reached"));
    }
    let changed = sqlx::query(
        r#"UPDATE "ApiTokens" SET is_revoked = FALSE
            WHERE id = $1
              AND is_revoked
              AND (expires_at IS NULL OR expires_at > clock_timestamp())"#,
    )
    .bind(id)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?
    .rows_affected();
    if changed != 1 {
        return Err(AppError::conflict("Token state changed; retry the request"));
    }
    transaction.commit().await.map_err(database_error)?;
    st.cache
        .remove(&managed_api_token::negative_cache_key(&state.6))
        .await;
    Ok(MessageResponse::ok("Token restored"))
}

fn normalize_scopes(scopes: Option<Vec<String>>) -> AppResult<Vec<String>> {
    let mut scopes = scopes.unwrap_or_else(|| vec![managed_api_token::READ_SCOPE.to_string()]);
    scopes.sort_unstable();
    scopes.dedup();
    if scopes.is_empty()
        || scopes.len() > 2
        || scopes.iter().any(|scope| {
            !matches!(
                scope.as_str(),
                managed_api_token::READ_SCOPE | managed_api_token::WRITE_SCOPE
            )
        })
    {
        return Err(AppError::bad_request(
            "scopes must contain api:read, api:write, or both",
        ));
    }
    Ok(scopes)
}

fn checked_expiry(now: DateTime<Utc>, expires_in: Option<u32>) -> AppResult<Option<DateTime<Utc>>> {
    match expires_in {
        Some(days) if days > 0 => {
            let duration = Duration::try_days(i64::from(days))
                .ok_or_else(|| AppError::bad_request("expiresIn is too large"))?;
            now.checked_add_signed(duration)
                .map(Some)
                .ok_or_else(|| AppError::bad_request("expiresIn is too large"))
        }
        _ => Ok(None),
    }
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_bounds_and_scope_defaults_are_explicit() {
        assert_eq!(
            normalize_scopes(None).unwrap(),
            vec![managed_api_token::READ_SCOPE.to_string()]
        );
        assert_eq!(
            normalize_scopes(Some(vec![
                managed_api_token::WRITE_SCOPE.into(),
                managed_api_token::READ_SCOPE.into(),
                managed_api_token::WRITE_SCOPE.into(),
            ]))
            .unwrap(),
            vec![
                managed_api_token::READ_SCOPE.to_string(),
                managed_api_token::WRITE_SCOPE.to_string()
            ]
        );
        assert!(normalize_scopes(Some(vec!["api:*".into()])).is_err());
        assert!(MAX_ACTIVE_TOKENS_PER_OWNER > 0);
        assert!(MAX_PAGE_SIZE <= 100);
    }

    #[test]
    fn writes_are_atomic_and_restore_never_revives_expired_credentials() {
        assert!(CREATE_SQL.contains("account.security_stamp = $12"));
        assert!(CREATE_SQL.contains("existing.token_version = $6"));
        assert!(CREATE_SQL.contains("RETURNING id"));
        assert!(REVOKE_SQL.contains("AND NOT token.is_revoked"));
    }

    #[test]
    fn expiry_arithmetic_is_checked() {
        let now = Utc::now();
        assert_eq!(checked_expiry(now, Some(0)).unwrap(), None);
        assert!(checked_expiry(DateTime::<Utc>::MAX_UTC, Some(u32::MAX)).is_err());
    }
}
