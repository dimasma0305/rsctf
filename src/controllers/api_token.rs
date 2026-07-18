//! Ported from RSCTF `Controllers/ApiTokenController.cs` (+ `ApiTokenRepository`).
//!
//! Admin-only management of API tokens for programmatic access. Route prefix
//! `/api/tokens`. The plaintext secret is generated once, returned once, and
//! only its SHA-256 hash is ever persisted in `ApiTokens.token_hash`.

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, QueryOrder, Set};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::AdminUser;
use crate::models::data::api_token;
use crate::utils::codec::{random_token, sha256_str};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{MessageResponse, RequestResponse};

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
) -> AppResult<RequestResponse<Vec<ApiToken>>> {
    let tokens = api_token::Entity::find()
        .order_by_desc(api_token::Column::CreatedAt)
        .all(&st.db)
        .await?;
    let data = tokens.into_iter().map(ApiToken::from).collect();
    Ok(RequestResponse::ok(data))
}

/// `POST /api/tokens` — generate a new token and return the plaintext secret once.
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

    // Generate the opaque bearer secret; persist only its SHA-256 hash.
    let secret = random_token(32);
    let token_hash = sha256_str(&secret);

    let now = Utc::now();
    // Guard against a huge `expiresIn` overflowing the date arithmetic (chrono
    // `Add` panics on overflow) — reject with a 400 instead.
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

    let am = api_token::ActiveModel {
        id: Set(Uuid::now_v7()),
        name: Set(name),
        token_hash: Set(token_hash),
        creator_id: Set(Some(user.id)),
        created_at: Set(now),
        expires_at: Set(expires_at),
        last_used_at: Set(None),
        is_revoked: Set(false),
    };
    let saved = am.insert(&st.db).await?;

    Ok(RequestResponse::ok(ApiTokenResponse {
        token: secret,
        info: saved.into(),
    }))
}

/// `DELETE /api/tokens/{id}` — soft revoke (set `is_revoked = true`).
pub async fn revoke_token(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let token = api_token::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Token not found"))?;

    let mut am: api_token::ActiveModel = token.into();
    am.is_revoked = Set(true);
    am.update(&st.db).await?;
    Ok(MessageResponse::ok("Token revoked"))
}

/// `POST /api/tokens/{id}/restore` — un-revoke (set `is_revoked = false`).
pub async fn restore_token(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> AppResult<MessageResponse> {
    let token = api_token::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Token not found"))?;

    let mut am: api_token::ActiveModel = token.into();
    am.is_revoked = Set(false);
    am.update(&st.db).await?;
    Ok(MessageResponse::ok("Token restored"))
}
