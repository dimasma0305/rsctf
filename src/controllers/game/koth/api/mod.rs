//! Signed, challenge-scoped Leaderboard evidence reporting.
//!
//! Managed targets (or legacy external reporters) submit bounded evidence
//! ratios, never points. The round checker is
//! the only component that can turn a stable, healthy snapshot into score.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

mod admin;
mod authentication;
mod submission;

pub use admin::{get_observer, recover_observer_operation, revoke_observer, rotate_observer};
pub use authentication::authenticate_capability;
pub use submission::submit_observation;

pub(super) const TIMESTAMP_HEADER: &str = "x-rsctf-timestamp";
pub(super) const SIGNATURE_HEADER: &str = "x-rsctf-signature";
pub(super) const SIGNATURE_PREFIX: &str = "sha256=";
pub(super) const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1_000;
pub(super) const INSERT_REPLAY_SQL: &str = r#"INSERT INTO "KothApiRequestReplays"
             (request_hash, challenge_id, expires_at)
           VALUES ($1, $2, clock_timestamp() + interval '10 minutes')
           ON CONFLICT (request_hash) DO NOTHING"#;

#[derive(Clone, Debug, sqlx::FromRow)]
pub(super) struct ActiveObserverContext {
    pub(super) target_id: i32,
    pub(super) cycle_id: i64,
    pub(super) cycle_number: i32,
    pub(super) reset_attempt: i32,
    pub(super) reporting_revision: i64,
    pub(super) container_id: String,
    pub(super) round_id: i32,
    pub(super) round_number: i32,
    pub(super) game_starts_at: DateTime<Utc>,
    /// Compatibility name on the wire; Leaderboard uses the event cutoff.
    pub(super) cycle_ends_at: DateTime<Utc>,
    pub(super) round_starts_at: DateTime<Utc>,
    pub(super) round_ends_at: DateTime<Utc>,
    pub(super) objective_ids: Option<Vec<String>>,
    pub(super) objective_schema_hash: Option<Vec<u8>>,
}

impl ActiveObserverContext {
    pub(super) fn opaque_context(
        &self,
        game_id: i32,
        challenge_id: i32,
        eligible_tokens: &[String],
    ) -> String {
        opaque_context(OpaqueContext {
            game_id,
            challenge_id,
            target_id: self.target_id,
            cycle_id: self.cycle_id,
            reset_attempt: self.reset_attempt,
            reporting_revision: self.reporting_revision,
            container_id: &self.container_id,
            round_id: self.round_id,
            objective_schema_hash: self.objective_schema_hash.as_deref(),
            eligible_tokens,
        })
    }

    pub(super) fn replay_scope(
        &self,
        game_id: i32,
        challenge_id: i32,
        eligible_tokens: &[String],
    ) -> String {
        // The first accepted observation freezes the objective schema, so that
        // one field may change while an exact retry is in flight. Every other
        // live target, reporter, round, and roster fence remains authoritative.
        opaque_context(OpaqueContext {
            game_id,
            challenge_id,
            target_id: self.target_id,
            cycle_id: self.cycle_id,
            reset_attempt: self.reset_attempt,
            reporting_revision: self.reporting_revision,
            container_id: &self.container_id,
            round_id: self.round_id,
            objective_schema_hash: None,
            eligible_tokens,
        })
    }

    pub(super) fn wave_window(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let lag = chrono::Duration::seconds(
            crate::services::ad::engine::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS,
        );
        let shifted_start = if self.round_number <= 1 {
            self.round_starts_at
        } else {
            self.round_starts_at - lag
        };
        (
            std::cmp::max(self.game_starts_at, shifted_start),
            self.round_ends_at - lag,
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
    round_number: i32,
    /// Kept as `cycleEndsAt` for existing reporters; this is the event cutoff.
    #[serde(with = "crate::utils::datetime::millis")]
    cycle_ends_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    wave_window_starts_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    wave_window_ends_at: DateTime<Utc>,
    eligible_token_hashes: Vec<String>,
    objective_ids: Vec<String>,
    objective_schema_hash: Option<String>,
    #[serde(with = "crate::utils::datetime::millis")]
    generated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothObservationAcceptedModel {
    pub(super) accepted: bool,
    pub(super) cycle_number: i32,
    pub(super) reset_attempt: i32,
    pub(super) round_number: i32,
    pub(super) submitted_waves: usize,
    pub(super) submitted_teams: usize,
    pub(super) recognized_teams: usize,
    #[serde(with = "crate::utils::datetime::millis")]
    pub(super) accepted_at: DateTime<Utc>,
}

const OBSERVER_CONTEXT_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const OBSERVER_CONTEXT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
const OBSERVER_CONTEXT_MAX_BYTES: usize = 512 * 1024;
const OBSERVER_CONTEXT_CONCURRENCY: usize = 16;
static OBSERVER_CONTEXT_ADMISSION: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(OBSERVER_CONTEXT_CONCURRENCY));

fn observer_context_cache_key(game_id: i32, challenge_id: i32, generation: i64) -> String {
    format!("_KothObserverContextV3_{game_id}_{challenge_id}_{generation}")
}

pub(crate) async fn invalidate_observer_context(st: &SharedState, game_id: i32, challenge_id: i32) {
    if let Ok(Some(generation)) = sqlx::query_scalar::<_, i64>(
        r#"SELECT generation FROM "KothObserverContextGenerations"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    {
        st.cache
            .remove(&observer_context_cache_key(
                game_id,
                challenge_id,
                generation,
            ))
            .await;
    }
}

pub(super) fn retry_after_response(error: AppError, seconds: u64) -> Response {
    let mut response = error.into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&seconds.max(1).to_string())
            .expect("positive integer Retry-After is a valid header"),
    );
    response
}
static OBSERVER_CONTEXT_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

pub(super) async fn load_active_context<'e, E>(
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
                  COALESCE(revision.revision, 0)::bigint AS reporting_revision,
                  target.container_id AS container_id,
                  round.id AS round_id, round.number AS round_number,
                  game.start_time_utc AS game_starts_at,
                  game.end_time_utc AS cycle_ends_at,
                  round.start_time_utc AS round_starts_at,
                  round.end_time_utc AS round_ends_at,
                  scheme.objective_ids,
                  scheme.objective_schema_hash
             FROM "KothTargets" target
             JOIN "Games" game
               ON game.id = target.game_id
              AND clock_timestamp() >= game.start_time_utc
              AND clock_timestamp() < game.end_time_utc
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
        LEFT JOIN "KothApiObserverRevisions" revision
               ON revision.game_id = target.game_id
              AND revision.challenge_id = target.challenge_id
        LEFT JOIN "KothApiArenaSchemes" scheme
               ON scheme.game_id = target.game_id
              AND scheme.challenge_id = target.challenge_id
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
             JOIN LATERAL (
               SELECT scoring.id, scoring.number,
                      scoring.start_time_utc, scoring.end_time_utc
                 FROM "AdRounds" scoring
                WHERE scoring.game_id = target.game_id
                  AND scoring.finalized = FALSE
                  AND clock_timestamp() >= scoring.start_time_utc
                  AND clock_timestamp() < scoring.end_time_utc
                ORDER BY scoring.number DESC
                LIMIT 1
             ) round ON TRUE
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

pub(super) async fn load_eligible_tokens<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Vec<String>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_scalar(
        r#"SELECT token.token
             FROM "KothApiTeamTokens" token
             JOIN "Participations" participation
               ON participation.id = token.participation_id
              AND participation.game_id = $1
              AND participation.status = $3
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "KothOfficialConfigs" config ON config.game_id = $1
             JOIN LATERAL jsonb_array_elements(config.roster_snapshot) roster(item)
               ON participation.id = CASE jsonb_typeof(roster.item)
                    WHEN 'number' THEN (roster.item #>> '{}')::integer
                    WHEN 'object' THEN
                      NULLIF(roster.item->>'participationId', '')::integer
                    ELSE NULL
                  END
            WHERE token.game_id = $1
              AND token.challenge_id = $2
              AND NOT team.deletion_pending
              AND NOT EXISTS (
                    SELECT 1
                      FROM (
                          SELECT team.captain_id AS user_id
                          UNION
                          SELECT member.user_id
                            FROM "TeamMembers" member
                           WHERE member.team_id = team.id
                      ) roster_member
                      LEFT JOIN "AspNetUsers" account
                        ON account.id = roster_member.user_id
                     WHERE account.id IS NULL OR account.role = $4
              )
            ORDER BY token.participation_id
            FOR SHARE OF token"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_all(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Public, non-secret fence bound to the exact container, cycle, and scoring tick.
async fn observer_context_body(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<bytes::Bytes> {
    let generation: i64 = sqlx::query_scalar(
        r#"SELECT generation FROM "KothObserverContextGenerations"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("Leaderboard KotH context is not active"))?;
    let key = observer_context_cache_key(game_id, challenge_id, generation);
    if let Some(body) = st.cache.get(&key).await {
        return Ok(body);
    }
    let st = st.clone();
    let cache_key = key.clone();
    OBSERVER_CONTEXT_SF
        .run(&key, move || async move {
            if let Some(body) = st.cache.get(&cache_key).await {
                return Some(body);
            }
            let context = load_active_context(st.pg(), game_id, challenge_id)
                .await
                .ok()??;
            let eligible_tokens = load_eligible_tokens(st.pg(), game_id, challenge_id)
                .await
                .ok()?;
            if eligible_tokens.len() > super::api_contract::MAX_TEAM_ENTRIES {
                return None;
            }
            let (wave_window_starts_at, wave_window_ends_at) = context.wave_window();
            let body = bytes::Bytes::from(
                serde_json::to_vec(&KothObserverContextModel {
                    api_version: "v1",
                    context: context.opaque_context(game_id, challenge_id, &eligible_tokens),
                    cycle_number: context.cycle_number,
                    reset_attempt: context.reset_attempt,
                    round_number: context.round_number,
                    cycle_ends_at: context.cycle_ends_at,
                    wave_window_starts_at,
                    wave_window_ends_at,
                    eligible_token_hashes: eligible_tokens
                        .iter()
                        .map(|token| {
                            crate::services::ad::koth_api_capability::token_hash_hex(token)
                        })
                        .collect(),
                    objective_ids: context.objective_ids.clone().unwrap_or_default(),
                    objective_schema_hash: context.objective_schema_hash.as_ref().map(hex::encode),
                    // Stable for the full context generation so cache expiry
                    // alone never changes the ETag.
                    generated_at: context.round_starts_at,
                })
                .ok()?,
            );
            if body.len() > OBSERVER_CONTEXT_MAX_BYTES {
                return None;
            }
            st.cache
                .set(&cache_key, &body, Some(OBSERVER_CONTEXT_TTL))
                .await;
            Some(body)
        })
        .await
        .ok_or_else(|| AppError::conflict("Leaderboard KotH context is not active"))
}

/// Public, non-secret fence bound to the exact container, cycle, and scoring tick.
pub async fn observer_context(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let Ok(_permit) = OBSERVER_CONTEXT_ADMISSION.try_acquire() else {
        return Ok(retry_after_response(AppError::too_many_requests(1), 1));
    };
    let body = match tokio::time::timeout(
        OBSERVER_CONTEXT_DEADLINE,
        observer_context_body(&st, game_id, challenge_id),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Ok(retry_after_response(
                AppError::unavailable("Leaderboard context timed out; retry later"),
                1,
            ));
        }
    };
    let validator = format!("\"{}\"", hex::encode(Sha256::digest(&body)));
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|candidates| {
            candidates.split(',').map(str::trim).any(|candidate| {
                candidate == validator || candidate.strip_prefix("W/") == Some(validator.as_str())
            })
        })
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&validator)
                .map_err(|error| AppError::internal(error.to_string()))?,
        );
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
        return Ok(response);
    }
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&validator).map_err(|error| AppError::internal(error.to_string()))?,
    );
    Ok(response)
}

pub(super) fn parse_timestamp(headers: &HeaderMap, now_ms: i64) -> AppResult<(i64, &str)> {
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

pub(super) fn parse_signature(headers: &HeaderMap) -> AppResult<[u8; 32]> {
    let raw = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix(SIGNATURE_PREFIX))
        .ok_or(AppError::Unauthorized)?;
    let decoded = hex::decode(raw).map_err(|_| AppError::Unauthorized)?;
    decoded.try_into().map_err(|_| AppError::Unauthorized)
}

pub(super) fn verify_signature(
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

struct OpaqueContext<'a> {
    game_id: i32,
    challenge_id: i32,
    target_id: i32,
    cycle_id: i64,
    reset_attempt: i32,
    reporting_revision: i64,
    container_id: &'a str,
    round_id: i32,
    objective_schema_hash: Option<&'a [u8]>,
    eligible_tokens: &'a [String],
}

fn opaque_context(context: OpaqueContext<'_>) -> String {
    let mut digest = Sha256::new();
    digest.update(context.game_id.to_be_bytes());
    digest.update(context.challenge_id.to_be_bytes());
    digest.update(context.target_id.to_be_bytes());
    digest.update(context.cycle_id.to_be_bytes());
    digest.update(context.reset_attempt.to_be_bytes());
    digest.update(context.reporting_revision.to_be_bytes());
    digest.update((context.container_id.len() as u64).to_be_bytes());
    digest.update(context.container_id.as_bytes());
    digest.update(context.round_id.to_be_bytes());
    digest.update(context.objective_schema_hash.unwrap_or(&[0_u8; 32]));
    digest.update((context.eligible_tokens.len() as u64).to_be_bytes());
    for token in context.eligible_tokens {
        digest.update(crate::services::ad::koth_api_capability::token_hash(token));
    }
    hex::encode(digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

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
        let mut headers = HeaderMap::new();
        headers.insert(TIMESTAMP_HEADER, HeaderValue::from_static("123"));
        headers.insert(
            SIGNATURE_HEADER,
            HeaderValue::from_str(&format!(
                "{SIGNATURE_PREFIX}{}",
                hex::encode(mac.finalize().into_bytes())
            ))
            .unwrap(),
        );
        headers
    }

    #[test]
    fn signature_binds_timestamp_scope_and_exact_body() {
        let body = br#"{"context":"abc","teams":[]}"#;
        let headers = signed_headers("secret", "123", 7, 9, body);
        let signature = parse_signature(&headers).unwrap();
        assert!(verify_signature("secret", "123", 7, 9, body, &signature).is_ok());
        assert!(verify_signature("secret", "124", 7, 9, body, &signature).is_err());
        assert!(verify_signature("secret", "123", 8, 9, body, &signature).is_err());
        assert!(verify_signature("secret", "123", 7, 10, body, &signature).is_err());
        assert!(verify_signature("secret", "123", 7, 9, b"{}", &signature).is_err());
    }

    #[test]
    fn context_changes_for_every_runtime_and_scoring_window() {
        let context = |game_id,
                       challenge_id,
                       target_id,
                       cycle_id,
                       reset_attempt,
                       reporting_revision,
                       container_id,
                       round_id,
                       objective_schema_hash,
                       eligible_tokens| {
            opaque_context(OpaqueContext {
                game_id,
                challenge_id,
                target_id,
                cycle_id,
                reset_attempt,
                reporting_revision,
                container_id,
                round_id,
                objective_schema_hash,
                eligible_tokens,
            })
        };
        let tokens = vec!["token-a".to_string(), "token-b".to_string()];
        let base = context(7, 9, 3, 41, 1, 5, "container-a", 51, None, &tokens);
        assert_eq!(base.len(), 64);
        assert_ne!(
            base,
            context(8, 9, 3, 41, 1, 5, "container-a", 51, None, &tokens)
        );
        assert_ne!(
            base,
            context(7, 9, 4, 41, 1, 5, "container-a", 51, None, &tokens)
        );
        assert_ne!(
            base,
            context(7, 9, 3, 42, 1, 5, "container-a", 51, None, &tokens)
        );
        assert_ne!(
            base,
            context(7, 9, 3, 41, 2, 5, "container-a", 51, None, &tokens)
        );
        assert_ne!(
            base,
            context(7, 9, 3, 41, 1, 6, "container-a", 51, None, &tokens)
        );
        assert_ne!(
            base,
            context(7, 9, 3, 41, 1, 5, "container-b", 51, None, &tokens)
        );
        assert_ne!(
            base,
            context(7, 9, 3, 41, 1, 5, "container-a", 52, None, &tokens)
        );
        assert_ne!(
            base,
            context(
                7,
                9,
                3,
                41,
                1,
                5,
                "container-a",
                51,
                Some(&[1; 32]),
                &tokens
            )
        );
        assert_ne!(
            base,
            context(
                7,
                9,
                3,
                41,
                1,
                5,
                "container-a",
                51,
                None,
                &["token-a".to_string(), "rotated-token".to_string()]
            )
        );
    }

    #[test]
    fn wave_windows_are_contiguous_and_never_include_warmup() {
        let at = |seconds| DateTime::from_timestamp(seconds, 0).unwrap();
        let context = |round_number, round_start, round_end| ActiveObserverContext {
            target_id: 3,
            cycle_id: 41,
            cycle_number: 1,
            reset_attempt: 0,
            reporting_revision: 1,
            container_id: "runtime-a".to_string(),
            round_id: round_number,
            round_number,
            game_starts_at: at(100),
            cycle_ends_at: at(310),
            round_starts_at: at(round_start),
            round_ends_at: at(round_end),
            objective_ids: None,
            objective_schema_hash: None,
        };
        let first = context(1, 130, 190).wave_window();
        let second = context(2, 190, 250).wave_window();
        assert_eq!(first, (at(130), at(170)));
        assert_eq!(second, (at(170), at(230)));
        assert_eq!(first.1, second.0);
    }

    #[test]
    fn observer_context_wire_contract_includes_the_cycle_deadline_in_milliseconds() {
        let at = |seconds| DateTime::from_timestamp(seconds, 123_000_000).unwrap();
        let value = serde_json::to_value(KothObserverContextModel {
            api_version: "v1",
            context: "a".repeat(64),
            cycle_number: 4,
            reset_attempt: 2,
            round_number: 9,
            cycle_ends_at: at(310),
            wave_window_starts_at: at(170),
            wave_window_ends_at: at(230),
            eligible_token_hashes: vec!["b".repeat(64)],
            objective_ids: vec!["proof-strength".to_string()],
            objective_schema_hash: Some("c".repeat(64)),
            generated_at: at(171),
        })
        .unwrap();
        assert_eq!(value["apiVersion"], "v1");
        assert_eq!(value["cycleEndsAt"], 310_123_i64);
        assert_eq!(value["waveWindowStartsAt"], 170_123_i64);
        assert_eq!(value["waveWindowEndsAt"], 230_123_i64);
        assert!(value.get("cycle_ends_at").is_none());
        assert_eq!(value.as_object().unwrap().len(), 12);
    }

    #[test]
    fn observer_context_work_is_explicitly_bounded() {
        assert!(std::hint::black_box(OBSERVER_CONTEXT_CONCURRENCY) <= 16);
        assert!(OBSERVER_CONTEXT_DEADLINE <= std::time::Duration::from_secs(2));
        assert!(OBSERVER_CONTEXT_TTL <= std::time::Duration::from_secs(5));
        assert!(std::hint::black_box(OBSERVER_CONTEXT_MAX_BYTES) <= 512 * 1024);
    }

    #[test]
    fn timestamp_window_is_strict() {
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
    fn observer_context_cache_identity_includes_durable_generation() {
        assert_ne!(
            observer_context_cache_key(4, 9, 10),
            observer_context_cache_key(4, 9, 11)
        );
    }
}
