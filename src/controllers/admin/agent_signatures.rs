//! Admin registry of agent-artifact signatures used by the cheat scanner, and
//! the per-event rescan of uploaded solvers and writeups.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    app_state::SharedState,
    middlewares::privilege_authentication::AdminUser,
    services::agent_artifacts::{self, SignatureRow, BUILTIN_SIGNATURES, MAX_CUSTOM_SIGNATURES},
    utils::error::{AppError, AppResult},
};

/// Serializes custom-signature writes so the cap cannot be exceeded by
/// concurrent administrators.
const REGISTRY_LOCK_KEY: &str = "agent-artifact-signatures";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSignatureModel {
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
pub struct AgentSignatureList {
    signatures: Vec<AgentSignatureModel>,
    max_custom_signatures: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SaveSignature {
    enabled: bool,
    label: Option<String>,
    pattern: Option<String>,
}

fn models(rows: &[SignatureRow]) -> Vec<AgentSignatureModel> {
    let mut signatures = BUILTIN_SIGNATURES
        .iter()
        .map(|signature| {
            let row = rows
                .iter()
                .find(|row| row.builtin && row.signature_key == signature.key);
            AgentSignatureModel {
                key: signature.key.to_string(),
                label: signature.label.to_string(),
                pattern: signature.pattern.to_string(),
                builtin: true,
                enabled: row.is_none_or(|row| row.enabled),
                examples: signature.examples.iter().map(|e| e.to_string()).collect(),
                updated_at: row.map(|row| row.updated_at),
            }
        })
        .collect::<Vec<_>>();
    signatures.extend(
        rows.iter()
            .filter(|row| !row.builtin)
            .map(|row| AgentSignatureModel {
                key: row.signature_key.clone(),
                label: row.label.clone().unwrap_or_default(),
                pattern: row.pattern.clone().unwrap_or_default(),
                builtin: false,
                enabled: row.enabled,
                examples: Vec::new(),
                updated_at: Some(row.updated_at),
            }),
    );
    signatures
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

/// `GET /api/admin/agent-signatures`
pub async fn list_signatures(
    State(st): State<SharedState>,
    _admin: AdminUser,
) -> AppResult<Response> {
    let rows = agent_artifacts::load_rows(st.pg()).await?;
    Ok(private_json(AgentSignatureList {
        signatures: models(&rows),
        max_custom_signatures: MAX_CUSTOM_SIGNATURES,
    }))
}

/// `PUT /api/admin/agent-signatures/{key}` — toggle a built-in, or create or
/// update a custom signature. Takes effect for the next scan or rescan.
pub async fn save_signature(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(key): Path<String>,
    Json(model): Json<SaveSignature>,
) -> AppResult<Response> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(REGISTRY_LOCK_KEY)
        .execute(&mut *transaction)
        .await
        .map_err(db_error)?;
    if agent_artifacts::builtin(&key).is_some() {
        if model.label.is_some() || model.pattern.is_some() {
            return Err(AppError::bad_request(
                "Built-in signatures accept only the enabled switch",
            ));
        }
        sqlx::query(
            r#"INSERT INTO "AgentArtifactSignatures"
                    (signature_key, builtin, enabled, updated_by, updated_at)
               VALUES ($1, TRUE, $2, $3, now())
               ON CONFLICT (signature_key) DO UPDATE
                  SET enabled = EXCLUDED.enabled, updated_by = EXCLUDED.updated_by,
                      updated_at = now()
                WHERE "AgentArtifactSignatures".builtin"#,
        )
        .bind(&key)
        .bind(model.enabled)
        .bind(admin.id)
        .execute(&mut *transaction)
        .await
        .map_err(db_error)?;
    } else {
        agent_artifacts::validate_custom_key(&key)?;
        let label =
            agent_artifacts::validate_custom_label(model.label.as_deref().unwrap_or_default())?;
        let pattern =
            agent_artifacts::validate_custom_pattern(model.pattern.as_deref().unwrap_or_default())?;
        let written: Option<String> = sqlx::query_scalar(
            r#"INSERT INTO "AgentArtifactSignatures"
                    (signature_key, builtin, label, pattern, enabled, updated_by, updated_at)
               SELECT $1, FALSE, $2, $3, $4, $5, now()
                WHERE EXISTS (SELECT 1 FROM "AgentArtifactSignatures" WHERE signature_key = $1)
                   OR (SELECT COUNT(*) FROM "AgentArtifactSignatures" WHERE NOT builtin) < $6
               ON CONFLICT (signature_key) DO UPDATE
                  SET label = EXCLUDED.label, pattern = EXCLUDED.pattern,
                      enabled = EXCLUDED.enabled, updated_by = EXCLUDED.updated_by,
                      updated_at = now()
                WHERE NOT "AgentArtifactSignatures".builtin
               RETURNING signature_key"#,
        )
        .bind(&key)
        .bind(&label)
        .bind(&pattern)
        .bind(model.enabled)
        .bind(admin.id)
        .bind(MAX_CUSTOM_SIGNATURES)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db_error)?;
        if written.is_none() {
            return Err(AppError::bad_request(format!(
                "At most {MAX_CUSTOM_SIGNATURES} custom signatures are allowed"
            )));
        }
    }
    transaction.commit().await.map_err(db_error)?;
    let rows = agent_artifacts::load_rows(st.pg()).await?;
    models(&rows)
        .into_iter()
        .find(|signature| signature.key == key)
        .map(private_json)
        .ok_or_else(|| AppError::internal("saved agent signature is missing"))
}

#[derive(Debug, Serialize)]
pub struct DeletedSignature {
    key: String,
}

/// `DELETE /api/admin/agent-signatures/{key}` — remove a custom signature.
/// Evidence already recorded keeps its stored signature label.
pub async fn delete_signature(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(key): Path<String>,
) -> AppResult<Response> {
    if agent_artifacts::builtin(&key).is_some() {
        return Err(AppError::bad_request(
            "Built-in signatures cannot be deleted; switch them off instead",
        ));
    }
    let deleted: Option<String> = sqlx::query_scalar(
        r#"DELETE FROM "AgentArtifactSignatures" WHERE signature_key = $1 AND NOT builtin
           RETURNING signature_key"#,
    )
    .bind(&key)
    .fetch_optional(st.pg())
    .await
    .map_err(db_error)?;
    deleted
        .map(|key| private_json(DeletedSignature { key }))
        .ok_or_else(|| AppError::not_found("Agent signature not found"))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RescanStarted {
    game_id: i32,
    started: bool,
}

/// `POST /api/admin/games/{id}/agent-artifacts/rescan` — rescan every solver
/// version and current writeup of the event in the background. Idempotent;
/// a rescan already running on this server is not started twice.
pub async fn rescan_game_artifacts(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(game_id): Path<i32>,
) -> AppResult<Response> {
    let exists: bool = sqlx::query_scalar(r#"SELECT EXISTS (SELECT 1 FROM "Games" WHERE id = $1)"#)
        .bind(game_id)
        .fetch_one(st.pg())
        .await
        .map_err(db_error)?;
    if !exists {
        return Err(AppError::not_found("Game not found"));
    }
    let key = format!("agent-artifact-rescan:{game_id}");
    let started = match crate::utils::single_flight::try_coalesce(&key) {
        Some(guard) => {
            let state = st.clone();
            tokio::spawn(async move {
                let _guard = guard;
                match agent_artifacts::rescan_game(&state, game_id).await {
                    Ok(summary) => {
                        tracing::info!(game_id, ?summary, "agent artifact rescan finished")
                    }
                    Err(error) => tracing::warn!(%error, game_id, "agent artifact rescan failed"),
                }
            });
            true
        }
        None => false,
    };
    Ok((
        StatusCode::ACCEPTED,
        [(axum::http::header::CACHE_CONTROL, "private, no-store")],
        Json(RescanStarted { game_id, started }),
    )
        .into_response())
}
