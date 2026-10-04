//! Admin registry of accepted AI chat share-link providers: built-in matchers
//! can be switched on or off; custom providers carry their own URL regex.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    app_state::SharedState,
    middlewares::privilege_authentication::AdminUser,
    services::ai_chat_links::{self, ProviderRow, BUILTIN_PROVIDERS, MAX_CUSTOM_PROVIDERS},
    utils::error::{AppError, AppResult},
};

/// Serializes custom-provider writes so the provider cap cannot be exceeded
/// by concurrent administrators.
const REGISTRY_LOCK_KEY: &str = "ai-chat-providers";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatProviderModel {
    key: String,
    label: String,
    pattern: String,
    builtin: bool,
    enabled: bool,
    examples: Vec<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatProviderList {
    providers: Vec<AiChatProviderModel>,
    max_custom_providers: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveProvider {
    enabled: bool,
    label: Option<String>,
    pattern: Option<String>,
}

fn models(rows: &[ProviderRow]) -> Vec<AiChatProviderModel> {
    let mut providers = BUILTIN_PROVIDERS
        .iter()
        .map(|provider| {
            let row = rows
                .iter()
                .find(|row| row.builtin && row.provider_key == provider.key);
            AiChatProviderModel {
                key: provider.key.to_string(),
                label: provider.label.to_string(),
                pattern: provider.pattern.to_string(),
                builtin: true,
                enabled: row.is_none_or(|row| row.enabled),
                examples: provider.examples.iter().map(|e| e.to_string()).collect(),
                updated_at: row.map(|row| row.updated_at),
            }
        })
        .collect::<Vec<_>>();
    providers.extend(
        rows.iter()
            .filter(|row| !row.builtin)
            .map(|row| AiChatProviderModel {
                key: row.provider_key.clone(),
                label: row.label.clone().unwrap_or_default(),
                pattern: row.pattern.clone().unwrap_or_default(),
                builtin: false,
                enabled: row.enabled,
                examples: Vec::new(),
                updated_at: Some(row.updated_at),
            }),
    );
    providers
}

fn private_json(value: impl Serialize) -> Response {
    (
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Json(value),
    )
        .into_response()
}

fn db_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

/// `GET /api/admin/ai-chat-providers`
pub async fn list_providers(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<Response> {
    let rows = ai_chat_links::load_rows(st.pg()).await?;
    Ok(private_json(AiChatProviderList {
        providers: models(&rows),
        max_custom_providers: MAX_CUSTOM_PROVIDERS,
    }))
}

/// `PUT /api/admin/ai-chat-providers/{key}` — toggle a built-in, or create or
/// update a custom provider.
pub async fn save_provider(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(key): Path<String>,
    Json(model): Json<SaveProvider>,
) -> AppResult<Response> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(REGISTRY_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(db_error)?;
    if ai_chat_links::builtin(&key).is_some() {
        if model.label.is_some() || model.pattern.is_some() {
            return Err(AppError::bad_request(
                "Built-in providers accept only the enabled switch",
            ));
        }
        sqlx::query(
            r#"INSERT INTO "AiChatProviders" (provider_key, builtin, enabled, updated_by, updated_at)
               VALUES ($1, TRUE, $2, $3, now())
               ON CONFLICT (provider_key) DO UPDATE
                  SET enabled = EXCLUDED.enabled, updated_by = EXCLUDED.updated_by,
                      updated_at = now()
                WHERE "AiChatProviders".builtin"#,
        )
        .bind(&key)
        .bind(model.enabled)
        .bind(admin.id)
        .execute(&mut *transaction)
        .await
        .map_err(db_error)?;
    } else {
        ai_chat_links::validate_custom_key(&key)?;
        let label =
            ai_chat_links::validate_custom_label(model.label.as_deref().unwrap_or_default())?;
        let pattern =
            ai_chat_links::validate_custom_pattern(model.pattern.as_deref().unwrap_or_default())?;
        let written: Option<String> = sqlx::query_scalar(
            r#"INSERT INTO "AiChatProviders"
                    (provider_key, builtin, label, pattern, enabled, updated_by, updated_at)
               SELECT $1, FALSE, $2, $3, $4, $5, now()
                WHERE EXISTS (SELECT 1 FROM "AiChatProviders" WHERE provider_key = $1)
                   OR (SELECT COUNT(*) FROM "AiChatProviders" WHERE NOT builtin) < $6
               ON CONFLICT (provider_key) DO UPDATE
                  SET label = EXCLUDED.label, pattern = EXCLUDED.pattern,
                      enabled = EXCLUDED.enabled, updated_by = EXCLUDED.updated_by,
                      updated_at = now()
                WHERE NOT "AiChatProviders".builtin
               RETURNING provider_key"#,
        )
        .bind(&key)
        .bind(&label)
        .bind(&pattern)
        .bind(model.enabled)
        .bind(admin.id)
        .bind(MAX_CUSTOM_PROVIDERS)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db_error)?;
        if written.is_none() {
            return Err(AppError::bad_request(format!(
                "At most {MAX_CUSTOM_PROVIDERS} custom AI chat providers are allowed"
            )));
        }
    }
    transaction.commit().await.map_err(db_error)?;
    let rows = ai_chat_links::load_rows(st.pg()).await?;
    models(&rows)
        .into_iter()
        .find(|provider| provider.key == key)
        .map(private_json)
        .ok_or_else(|| AppError::internal("saved AI chat provider is missing"))
}

#[derive(Debug, Serialize)]
pub struct DeletedProvider {
    key: String,
}

/// `DELETE /api/admin/ai-chat-providers/{key}` — remove a custom provider.
/// Saved links keep their stored provider label and show as blocked.
pub async fn delete_provider(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(key): Path<String>,
) -> AppResult<Response> {
    if ai_chat_links::builtin(&key).is_some() {
        return Err(AppError::bad_request(
            "Built-in providers cannot be deleted; switch them off instead",
        ));
    }
    let deleted: Option<String> = sqlx::query_scalar(
        r#"DELETE FROM "AiChatProviders" WHERE provider_key = $1 AND NOT builtin
           RETURNING provider_key"#,
    )
    .bind(&key)
    .fetch_optional(st.pg())
    .await
    .map_err(db_error)?;
    deleted
        .map(|key| private_json(DeletedProvider { key }))
        .ok_or_else(|| AppError::not_found("AI chat provider not found"))
}
