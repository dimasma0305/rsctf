//! Signed, challenge-scoped KotH observer API.
//!
//! The observer reports a control capability, never a score. The round checker
//! remains the only component allowed to turn a stable, healthy observation
//! into crown evidence.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::utils::enums::{ChallengeType, ParticipationStatus};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::{MessageResponse, RequestResponse};

use super::admin::require_game_admin;

const TIMESTAMP_HEADER: &str = "x-rsctf-timestamp";
const SIGNATURE_HEADER: &str = "x-rsctf-signature";
const SIGNATURE_PREFIX: &str = "sha256=";
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1_000;
const MAX_BODY_BYTES: usize = 1_024;
const OBSERVER_SECRET_BYTES: usize = 32;
const OBSERVER_SECRET_PREFIX: &str = "koth_api_";
const INSERT_REPLAY_SQL: &str = r#"INSERT INTO "KothApiRequestReplays"
             (request_hash, challenge_id, expires_at)
           VALUES ($1, $2, clock_timestamp() + interval '10 minutes')
           ON CONFLICT (request_hash) DO NOTHING"#;
const UPSERT_OBSERVATION_SQL: &str = r#"INSERT INTO "KothApiObservations"
             (target_id, game_id, challenge_id, cycle_id, reset_attempt,
              container_id, token_id, context_hash, request_timestamp_ms,
              accepted_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
           ON CONFLICT (target_id) DO UPDATE SET
             game_id = EXCLUDED.game_id,
             challenge_id = EXCLUDED.challenge_id,
             cycle_id = EXCLUDED.cycle_id,
             reset_attempt = EXCLUDED.reset_attempt,
             container_id = EXCLUDED.container_id,
             token_id = EXCLUDED.token_id,
             context_hash = EXCLUDED.context_hash,
             request_timestamp_ms = EXCLUDED.request_timestamp_ms,
             accepted_at = EXCLUDED.accepted_at
           WHERE "KothApiObservations".request_timestamp_ms
                 < EXCLUDED.request_timestamp_ms
              OR "KothApiObservations".cycle_id <> EXCLUDED.cycle_id
              OR "KothApiObservations".reset_attempt <> EXCLUDED.reset_attempt
              OR "KothApiObservations".container_id <> EXCLUDED.container_id"#;

#[derive(Clone, Debug, sqlx::FromRow)]
struct ActiveObserverContext {
    target_id: i32,
    cycle_id: i64,
    cycle_number: i32,
    reset_attempt: i32,
    container_id: String,
}

impl ActiveObserverContext {
    fn opaque_context(&self, game_id: i32, challenge_id: i32) -> String {
        opaque_context(
            game_id,
            challenge_id,
            self.target_id,
            self.cycle_id,
            self.reset_attempt,
            &self.container_id,
        )
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothObserverContextModel {
    api_version: &'static str,
    context: String,
    cycle_number: i32,
    reset_attempt: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    generated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KothObservationInput {
    context: String,
    token: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothObservationAcceptedModel {
    accepted: bool,
    cycle_number: i32,
    reset_attempt: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    accepted_at: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminKothObserverModel {
    pub challenge_id: i32,
    pub claim_source: String,
    pub configured: bool,
    pub secret_hint: Option<String>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub rotated_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub last_observation_at: Option<DateTime<Utc>>,
    pub context_path: String,
    pub observation_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

type ObserverMetaRow = (
    Option<String>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    bool,
);

fn paths(game_id: i32, challenge_id: i32) -> (String, String) {
    let base = format!("/api/v1/koth/games/{game_id}/challenges/{challenge_id}");
    (format!("{base}/context"), format!("{base}/observations"))
}

async fn observer_model(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    secret: Option<String>,
) -> AppResult<AdminKothObserverModel> {
    let row = sqlx::query_as::<_, ObserverMetaRow>(
        r#"SELECT
                  CASE
                    WHEN config.game_id IS NOT NULL
                      THEN COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker')
                    WHEN observer.challenge_id IS NOT NULL THEN 'Api'
                    ELSE 'Marker'
                  END AS claim_source,
                  observer.secret_hint, observer.created_at, observer.rotated_at,
                  observer.last_used_at, observation.accepted_at,
                  observer.challenge_id IS NOT NULL AS configured
             FROM "GameChallenges" challenge
             LEFT JOIN "KothOfficialConfigs" config
               ON config.game_id = challenge.game_id
             LEFT JOIN LATERAL (
               SELECT item
                 FROM jsonb_array_elements(config.hills_snapshot) item
                WHERE (item->>'challengeId')::integer = challenge.id
                LIMIT 1
             ) frozen ON TRUE
             LEFT JOIN "KothApiObservers" observer
               ON observer.game_id = challenge.game_id
              AND observer.challenge_id = challenge.id
             LEFT JOIN "KothTargets" target
               ON target.game_id = challenge.game_id
              AND target.challenge_id = challenge.id
             LEFT JOIN "KothApiObservations" observation
               ON observation.target_id = target.id
            WHERE challenge.game_id = $1 AND challenge.id = $2
              AND challenge."Type" = $3"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH challenge not found"))?;
    let (context_path, observation_path) = paths(game_id, challenge_id);
    Ok(AdminKothObserverModel {
        challenge_id,
        claim_source: row.0.unwrap_or_else(|| "Marker".to_string()),
        configured: row.6,
        secret_hint: row.1,
        created_at: row.2,
        rotated_at: row.3,
        last_used_at: row.4,
        last_observation_at: row.5,
        context_path,
        observation_path,
        secret,
    })
}

async fn require_observer_can_be_enabled(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let row = sqlx::query_as::<_, (Option<String>, bool)>(
        r#"SELECT frozen.item->>'claimSource' AS frozen_source,
                  config.game_id IS NOT NULL AS snapshotted
             FROM "GameChallenges" challenge
             LEFT JOIN "KothOfficialConfigs" config
               ON config.game_id = challenge.game_id
             LEFT JOIN LATERAL (
               SELECT item
                 FROM jsonb_array_elements(config.hills_snapshot) item
                WHERE (item->>'challengeId')::integer = challenge.id
                LIMIT 1
             ) frozen ON TRUE
            WHERE challenge.game_id = $1 AND challenge.id = $2
              AND challenge."Type" = $3
            FOR SHARE OF challenge"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH challenge not found"))?;
    if row.1 && row.0.as_deref() != Some("Api") {
        return Err(AppError::conflict(
            "the official KotH snapshot fixed this hill to marker observations",
        ));
    }
    Ok(())
}

/// Manager-visible observer metadata. The HMAC secret is never returned here.
pub async fn get_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<AdminKothObserverModel>> {
    require_game_admin(&st, &user, game_id).await?;
    Ok(RequestResponse::ok(
        observer_model(&st, game_id, challenge_id, None).await?,
    ))
}

/// Enable or rotate the observer. The plaintext secret is returned exactly once.
pub async fn rotate_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    require_game_admin(&st, &user, game_id).await?;
    let secret = format!(
        "{OBSERVER_SECRET_PREFIX}{}",
        crate::utils::codec::random_token(OBSERVER_SECRET_BYTES)
    );
    let hint = format!("…{}", &secret[secret.len() - 6..]);
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    require_observer_can_be_enabled(control.transaction_mut(), game_id, challenge_id).await?;
    sqlx::query(
        r#"INSERT INTO "KothApiObservers"
             (challenge_id, game_id, hmac_secret, secret_hint,
              created_at, rotated_at, last_used_at)
           VALUES ($2, $1, $3, $4, clock_timestamp(), clock_timestamp(), NULL)
           ON CONFLICT (challenge_id) DO UPDATE SET
             game_id = EXCLUDED.game_id,
             hmac_secret = EXCLUDED.hmac_secret,
             secret_hint = EXCLUDED.secret_hint,
             rotated_at = clock_timestamp(),
             last_used_at = NULL"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(&secret)
    .bind(&hint)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "KothApiObservations" observation
            USING "KothTargets" target
            WHERE observation.target_id = target.id
              AND target.game_id = $1 AND target.challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(r#"DELETE FROM "KothApiRequestReplays" WHERE challenge_id = $1"#)
        .bind(challenge_id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut response =
        RequestResponse::ok(observer_model(&st, game_id, challenge_id, Some(secret)).await?)
            .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
}

/// Revoke the observer. A snapshotted API hill stays in API mode and reports no
/// claim until a manager creates a fresh credential.
pub async fn revoke_observer(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    require_game_admin(&st, &user, game_id).await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    require_observer_can_be_enabled(control.transaction_mut(), game_id, challenge_id).await?;
    sqlx::query(
        r#"DELETE FROM "KothApiObservations" observation
            USING "KothTargets" target
            WHERE observation.target_id = target.id
              AND target.game_id = $1 AND target.challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"DELETE FROM "KothApiObservers"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(MessageResponse::ok("KotH API observer revoked"))
}

async fn load_active_context<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Option<ActiveObserverContext>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ActiveObserverContext>(
        r#"SELECT target.id AS target_id, cycle.id AS cycle_id,
                  cycle.cycle_number, cycle.reset_attempt,
                  target.container_id AS container_id
             FROM "KothTargets" target
             JOIN "KothOfficialConfigs" config
               ON config.game_id = target.game_id
             JOIN LATERAL (
               SELECT item
                 FROM jsonb_array_elements(config.hills_snapshot) item
                WHERE (item->>'challengeId')::integer = target.challenge_id
                  AND COALESCE(NULLIF(item->>'claimSource', ''), 'Marker') = 'Api'
                LIMIT 1
             ) frozen ON TRUE
             JOIN "KothApiObservers" observer
               ON observer.game_id = target.game_id
              AND observer.challenge_id = target.challenge_id
             JOIN LATERAL (
               SELECT crown.id, crown.cycle_number, crown.reset_attempt,
                      crown.replacement_container_id
                 FROM "KothCrownCycles" crown
                WHERE crown.game_id = target.game_id
                  AND crown.challenge_id = target.challenge_id
                  AND crown.phase = 'Active'
                  AND crown.replacement_container_id = target.container_id
                ORDER BY crown.cycle_number DESC
                LIMIT 1
             ) cycle ON TRUE
            WHERE target.game_id = $1 AND target.challenge_id = $2
              AND NULLIF(BTRIM(target.container_id), '') IS NOT NULL
            FOR SHARE OF target"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Public, non-secret generation fence used in the signed observation body.
pub async fn observer_context(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<KothObserverContextModel>> {
    let context = load_active_context(st.pg(), game_id, challenge_id)
        .await?
        .ok_or_else(|| AppError::conflict("KotH API observer context is not active"))?;
    Ok(RequestResponse::ok(KothObserverContextModel {
        api_version: "v1",
        context: context.opaque_context(game_id, challenge_id),
        cycle_number: context.cycle_number,
        reset_attempt: context.reset_attempt,
        generated_at: Utc::now(),
    }))
}

fn parse_timestamp(headers: &HeaderMap, now_ms: i64) -> AppResult<(i64, &str)> {
    let raw = headers
        .get(TIMESTAMP_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::Unauthorized)?;
    let timestamp = raw.parse::<i64>().map_err(|_| AppError::Unauthorized)?;
    if now_ms.abs_diff(timestamp) > MAX_CLOCK_SKEW_MS {
        return Err(AppError::Unauthorized);
    }
    Ok((timestamp, raw))
}

fn parse_signature(headers: &HeaderMap) -> AppResult<[u8; 32]> {
    let raw = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix(SIGNATURE_PREFIX))
        .ok_or(AppError::Unauthorized)?;
    let decoded = hex::decode(raw).map_err(|_| AppError::Unauthorized)?;
    decoded.try_into().map_err(|_| AppError::Unauthorized)
}

fn verify_signature(
    secret: &str,
    timestamp: &str,
    game_id: i32,
    challenge_id: i32,
    body: &[u8],
    signature: &[u8; 32],
) -> AppResult<()> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| AppError::Unauthorized)?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(game_id.to_string().as_bytes());
    mac.update(b".");
    mac.update(challenge_id.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.verify_slice(signature)
        .map_err(|_| AppError::Unauthorized)
}

fn opaque_context(
    game_id: i32,
    challenge_id: i32,
    target_id: i32,
    cycle_id: i64,
    reset_attempt: i32,
    container_id: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(game_id.to_be_bytes());
    digest.update(challenge_id.to_be_bytes());
    digest.update(target_id.to_be_bytes());
    digest.update(cycle_id.to_be_bytes());
    digest.update(reset_attempt.to_be_bytes());
    digest.update((container_id.len() as u64).to_be_bytes());
    digest.update(container_id.as_bytes());
    hex::encode(digest.finalize())
}

fn validate_input(input: &KothObservationInput) -> AppResult<()> {
    if input.context.len() != 64
        || !input
            .context
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(AppError::bad_request("invalid KotH observer context"));
    }
    if input
        .token
        .as_deref()
        .is_some_and(|token| token.is_empty() || token.len() > 256)
    {
        return Err(AppError::bad_request("invalid KotH observation token"));
    }
    Ok(())
}

fn parse_input(body: &[u8]) -> AppResult<KothObservationInput> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| AppError::bad_request("invalid JSON body"))?;
    if !value
        .as_object()
        .is_some_and(|object| object.contains_key("token"))
    {
        return Err(AppError::bad_request(
            "KotH observation must include token (use null for no controller)",
        ));
    }
    serde_json::from_value(value).map_err(|_| AppError::bad_request("invalid JSON body"))
}

async fn resolve_token(
    connection: &mut sqlx::PgConnection,
    context: &ActiveObserverContext,
    game_id: i32,
    challenge_id: i32,
    token: Option<&str>,
) -> AppResult<Option<i32>> {
    let Some(token) = token else {
        return Ok(None);
    };
    sqlx::query_scalar(
        r#"SELECT capability.id
             FROM "KothTokens" capability
             JOIN "Participations" participation
               ON participation.id = capability.participation_id
              AND participation.status = $7
            WHERE capability.target_id = $1
              AND capability.cycle_id = $2
              AND capability.challenge_id = $3
              AND capability.reset_attempt = $4
              AND capability.token = $5
              AND capability.revoked_at IS NULL
              AND participation.game_id = $6
            LIMIT 1"#,
    )
    .bind(context.target_id)
    .bind(context.cycle_id)
    .bind(challenge_id)
    .bind(context.reset_attempt)
    .bind(token)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::bad_request("invalid KotH observation token"))
    .map(Some)
}

/// Accept one signed current-control observation. This stores input evidence
/// only; crown state and points remain untouched until the checker observes the
/// same value around a healthy functional probe.
pub async fn submit_observation(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<RequestResponse<KothObservationAcceptedModel>> {
    Ok(RequestResponse::ok(
        accept_observation(st.pg(), game_id, challenge_id, &headers, &body).await?,
    ))
}

async fn accept_observation(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
    headers: &HeaderMap,
    body: &[u8],
) -> AppResult<KothObservationAcceptedModel> {
    if body.len() > MAX_BODY_BYTES {
        return Err(AppError::payload_too_large(
            "KotH observation body must be at most 1024 bytes",
        ));
    }
    let now = Utc::now();
    let (timestamp, timestamp_raw) = parse_timestamp(headers, now.timestamp_millis())?;
    let signature = parse_signature(headers)?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let secret: Option<String> = sqlx::query_scalar(
        r#"SELECT observer.hmac_secret
             FROM "KothApiObservers" observer
             JOIN "GameChallenges" challenge
               ON challenge.game_id = observer.game_id
              AND challenge.id = observer.challenge_id
              AND challenge."Type" = $3
            WHERE observer.game_id = $1 AND observer.challenge_id = $2
            FOR SHARE OF observer"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let secret = secret.ok_or(AppError::Unauthorized)?;
    verify_signature(
        &secret,
        timestamp_raw,
        game_id,
        challenge_id,
        body,
        &signature,
    )?;
    let input = parse_input(body)?;
    validate_input(&input)?;
    let context = load_active_context(&mut *transaction, game_id, challenge_id)
        .await?
        .ok_or_else(|| AppError::conflict("KotH API observer context is not active"))?;
    if input.context != context.opaque_context(game_id, challenge_id) {
        return Err(AppError::conflict(
            "KotH observer context changed; fetch context and retry",
        ));
    }
    let token_id = resolve_token(
        &mut transaction,
        &context,
        game_id,
        challenge_id,
        input.token.as_deref(),
    )
    .await?;

    sqlx::query(
        r#"DELETE FROM "KothApiRequestReplays"
            WHERE request_hash IN (
              SELECT request_hash FROM "KothApiRequestReplays"
               WHERE expires_at < clock_timestamp()
               ORDER BY expires_at
               LIMIT 128
            )"#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let request_hash: [u8; 32] = Sha256::digest(signature).into();
    let replay_inserted = sqlx::query(INSERT_REPLAY_SQL)
        .bind(request_hash.as_slice())
        .bind(challenge_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if replay_inserted != 1 {
        return Err(AppError::conflict(
            "KotH observer request was already accepted",
        ));
    }
    let accepted_at = Utc::now();
    let observation_written = sqlx::query(UPSERT_OBSERVATION_SQL)
        .bind(context.target_id)
        .bind(game_id)
        .bind(challenge_id)
        .bind(context.cycle_id)
        .bind(context.reset_attempt)
        .bind(&context.container_id)
        .bind(token_id)
        .bind(&input.context)
        .bind(timestamp)
        .bind(accepted_at)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    if observation_written != 1 {
        return Err(AppError::conflict(
            "KotH observer timestamp is older than the accepted observation",
        ));
    }
    sqlx::query(
        r#"UPDATE "KothApiObservers"
              SET last_used_at = clock_timestamp()
            WHERE challenge_id = $1
              AND (last_used_at IS NULL
                   OR last_used_at < clock_timestamp() - interval '30 seconds')"#,
    )
    .bind(challenge_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(KothObservationAcceptedModel {
        accepted: true,
        cycle_number: context.cycle_number,
        reset_attempt: context.reset_attempt,
        accepted_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use sqlx::postgres::PgPoolOptions;
    use sqlx::{Connection, PgConnection};

    fn signed_headers(
        secret: &str,
        timestamp: &str,
        game_id: i32,
        challenge_id: i32,
        body: &[u8],
    ) -> HeaderMap {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(game_id.to_string().as_bytes());
        mac.update(b".");
        mac.update(challenge_id.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        let signature = hex::encode(mac.finalize().into_bytes());
        let mut headers = HeaderMap::new();
        headers.insert(TIMESTAMP_HEADER, HeaderValue::from_str(timestamp).unwrap());
        headers.insert(
            SIGNATURE_HEADER,
            HeaderValue::from_str(&format!("{SIGNATURE_PREFIX}{signature}")).unwrap(),
        );
        headers
    }

    #[test]
    fn signature_binds_timestamp_game_challenge_and_raw_body() {
        let body = br#"{"context":"abc","token":null}"#;
        let headers = signed_headers("secret", "123", 7, 9, body);
        let signature = parse_signature(&headers).unwrap();
        assert!(verify_signature("secret", "123", 7, 9, body, &signature).is_ok());
        assert!(verify_signature("secret", "124", 7, 9, body, &signature).is_err());
        assert!(verify_signature("secret", "123", 8, 9, body, &signature).is_err());
        assert!(verify_signature("secret", "123", 7, 10, body, &signature).is_err());
        assert!(verify_signature("secret", "123", 7, 9, b"{}", &signature).is_err());
    }

    #[test]
    fn timestamp_has_a_strict_five_minute_window() {
        let now = 1_000_000_i64;
        let mut headers = HeaderMap::new();
        headers.insert(
            TIMESTAMP_HEADER,
            HeaderValue::from_str(&(now - MAX_CLOCK_SKEW_MS as i64).to_string()).unwrap(),
        );
        assert!(parse_timestamp(&headers, now).is_ok());
        headers.insert(
            TIMESTAMP_HEADER,
            HeaderValue::from_str(&(now - MAX_CLOCK_SKEW_MS as i64 - 1).to_string()).unwrap(),
        );
        assert!(parse_timestamp(&headers, now).is_err());
    }

    #[test]
    fn context_changes_with_every_exact_runtime_identity() {
        let base = opaque_context(7, 9, 3, 41, 1, "container-a");
        assert_eq!(base.len(), 64);
        assert_ne!(base, opaque_context(8, 9, 3, 41, 1, "container-a"));
        assert_ne!(base, opaque_context(7, 10, 3, 41, 1, "container-a"));
        assert_ne!(base, opaque_context(7, 9, 4, 41, 1, "container-a"));
        assert_ne!(base, opaque_context(7, 9, 3, 42, 1, "container-a"));
        assert_ne!(base, opaque_context(7, 9, 3, 41, 2, "container-a"));
        assert_ne!(base, opaque_context(7, 9, 3, 41, 1, "container-b"));
    }

    #[test]
    fn input_rejects_noncanonical_contexts_and_oversized_tokens() {
        let valid = KothObservationInput {
            context: "a".repeat(64),
            token: None,
        };
        assert!(validate_input(&valid).is_ok());
        assert!(validate_input(&KothObservationInput {
            context: "A".repeat(64),
            token: None,
        })
        .is_err());
        assert!(validate_input(&KothObservationInput {
            context: "a".repeat(64),
            token: Some("x".repeat(257)),
        })
        .is_err());
        assert!(parse_input(
            br#"{"context":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#
        )
        .is_err());
    }

    async fn write_observation(
        connection: &mut PgConnection,
        cycle_id: i64,
        reset_attempt: i32,
        container_id: &str,
        timestamp: i64,
    ) -> u64 {
        sqlx::query(UPSERT_OBSERVATION_SQL)
            .bind(3_i32)
            .bind(7_i32)
            .bind(9_i32)
            .bind(cycle_id)
            .bind(reset_attempt)
            .bind(container_id)
            .bind(None::<i32>)
            .bind("a".repeat(64))
            .bind(timestamp)
            .bind(Utc::now())
            .execute(connection)
            .await
            .unwrap()
            .rows_affected()
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn accepted_observations_are_monotonic_within_context_but_reset_safe() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothApiObservations" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, cycle_id BIGINT NOT NULL,
              reset_attempt INTEGER NOT NULL, container_id TEXT NOT NULL,
              token_id INTEGER, context_hash CHAR(64) NOT NULL,
              request_timestamp_ms BIGINT NOT NULL,
              accepted_at TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            write_observation(&mut connection, 41, 0, "runtime-a", 100).await,
            1
        );
        assert_eq!(
            write_observation(&mut connection, 41, 0, "runtime-a", 99).await,
            0
        );
        assert_eq!(
            write_observation(&mut connection, 41, 0, "runtime-a", 101).await,
            1
        );
        // A new exact reset/container context gets a fresh monotonic sequence.
        assert_eq!(
            write_observation(&mut connection, 41, 1, "runtime-b", 50).await,
            1
        );
        let stored: (i32, String, i64) = sqlx::query_as(
            r#"SELECT reset_attempt, container_id, request_timestamp_ms
                 FROM "KothApiObservations" WHERE target_id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert_eq!(stored, (1, "runtime-b".to_string(), 50));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn an_accepted_signature_can_be_inserted_only_once() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "KothApiRequestReplays" (
              request_hash BYTEA PRIMARY KEY,
              challenge_id INTEGER NOT NULL,
              expires_at TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        let digest = [7_u8; 32];
        for expected in [1, 0] {
            let inserted = sqlx::query(INSERT_REPLAY_SQL)
                .bind(digest.as_slice())
                .bind(9_i32)
                .execute(&mut connection)
                .await
                .unwrap()
                .rows_affected();
            assert_eq!(inserted, expected);
        }
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn signed_api_input_stages_only_exact_claim_evidence() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "GameChallenges" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, "Type" SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "KothOfficialConfigs" (
              game_id INTEGER PRIMARY KEY, hills_snapshot JSONB NOT NULL
            );
            CREATE TEMP TABLE "KothTargets" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, container_id TEXT
            );
            CREATE TEMP TABLE "KothCrownCycles" (
              id BIGINT PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, cycle_number INTEGER NOT NULL,
              reset_attempt INTEGER NOT NULL, replacement_container_id TEXT,
              phase TEXT NOT NULL
            );
            CREATE TEMP TABLE "KothApiObservers" (
              challenge_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              hmac_secret TEXT NOT NULL, last_used_at TIMESTAMPTZ
            );
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "KothTokens" (
              id INTEGER PRIMARY KEY, target_id INTEGER NOT NULL,
              cycle_id BIGINT NOT NULL, challenge_id INTEGER NOT NULL,
              reset_attempt INTEGER NOT NULL, token TEXT NOT NULL,
              revoked_at TIMESTAMPTZ, participation_id INTEGER NOT NULL
            );
            CREATE TEMP TABLE "KothApiObservations" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, cycle_id BIGINT NOT NULL,
              reset_attempt INTEGER NOT NULL, container_id TEXT NOT NULL,
              token_id INTEGER, context_hash CHAR(64) NOT NULL,
              request_timestamp_ms BIGINT NOT NULL,
              accepted_at TIMESTAMPTZ NOT NULL
            );
            CREATE TEMP TABLE "KothApiRequestReplays" (
              request_hash BYTEA PRIMARY KEY, challenge_id INTEGER NOT NULL,
              expires_at TIMESTAMPTZ NOT NULL
            );

            INSERT INTO "GameChallenges" VALUES (9, 7, 5);
            INSERT INTO "KothOfficialConfigs" VALUES
              (7, '[{"challengeId":9,"claimSource":"Api"}]');
            INSERT INTO "KothTargets" VALUES (3, 7, 9, 'runtime-a');
            INSERT INTO "KothCrownCycles" VALUES
              (41, 7, 9, 4, 2, 'runtime-a', 'Active');
            INSERT INTO "KothApiObservers" VALUES
              (9, 7, 'observer-secret', NULL);
            INSERT INTO "Participations" VALUES (11, 7, 1);
            INSERT INTO "KothTokens" VALUES
              (101, 3, 41, 9, 2, 'current-token', NULL, 11);
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let context = load_active_context(&pool, 7, 9)
            .await
            .unwrap()
            .unwrap()
            .opaque_context(7, 9);
        let body = serde_json::to_vec(&serde_json::json!({
            "context": context,
            "token": "current-token",
        }))
        .unwrap();
        let timestamp = Utc::now().timestamp_millis().to_string();
        let headers = signed_headers("observer-secret", &timestamp, 7, 9, &body);
        let accepted = accept_observation(&pool, 7, 9, &headers, &body)
            .await
            .unwrap();
        assert!(accepted.accepted);
        assert_eq!((accepted.cycle_number, accepted.reset_attempt), (4, 2));
        let staged: (i64, i32, String, Option<i32>) = sqlx::query_as(
            r#"SELECT cycle_id, reset_attempt, container_id, token_id
                 FROM "KothApiObservations" WHERE target_id = 3"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(staged, (41, 2, "runtime-a".to_string(), Some(101)));
        let replay = accept_observation(&pool, 7, 9, &headers, &body)
            .await
            .unwrap_err();
        assert_eq!(replay.status(), axum::http::StatusCode::CONFLICT);
    }
}
