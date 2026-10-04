//! Signed, challenge-scoped Leaderboard evidence reporting.
//!
//! Managed targets (or legacy external reporters) submit bounded evidence
//! ratios, never points. The round checker is
//! the only component that can turn a stable, healthy snapshot into score.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

mod admin;
mod admission;
mod authentication;
mod submission;
#[cfg(test)]
#[path = "version_tests.rs"]
mod version_tests;

pub use admin::{get_observer, recover_observer_operation, revoke_observer, rotate_observer};
pub use authentication::authenticate_capability;
pub use submission::submit_observation;

pub(super) const TIMESTAMP_HEADER: &str = "x-rsctf-timestamp";
pub(super) const SIGNATURE_HEADER: &str = "x-rsctf-signature";
pub(super) const SIGNATURE_PREFIX: &str = "sha256=";
pub(super) const CONTEXT_API_VERSION_HEADER: &str = "x-rsctf-api-version";
pub(super) const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1_000;
pub(super) const CONTEXT_INACTIVE_MESSAGE: &str = "Leaderboard KotH context is not active";
pub(super) const STALE_CONTEXT_MESSAGE: &str =
    "Leaderboard KotH context changed; fetch context and retry";
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
    pub(super) scoring_starts_at: DateTime<Utc>,
    /// Compatibility name on the wire; Leaderboard uses the event cutoff.
    pub(super) cycle_ends_at: DateTime<Utc>,
    /// Latest evidence end the platform can settle before the event deadline.
    pub(super) scoring_ends_at: DateTime<Utc>,
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

    pub(super) fn wave_window(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let lag = chrono::Duration::seconds(
            crate::services::ad::engine::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS,
        );
        let shifted_start = if self.round_number <= 1 {
            self.round_starts_at
        } else {
            self.round_starts_at - lag
        };
        let settled_end = std::cmp::min(self.round_ends_at - lag, self.scoring_ends_at);
        // Windows are half-open so an ordinary boundary belongs to the next
        // round. The event's final admissible millisecond has no successor;
        // expose an exclusive bound one millisecond later so scoringEndsAt
        // itself remains admissible.
        let window_end = if settled_end == self.scoring_ends_at {
            settled_end + chrono::Duration::milliseconds(1)
        } else {
            settled_end
        };
        (
            std::cmp::max(self.scoring_starts_at, shifted_start),
            window_end,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothObserverContextV2Model {
    api_version: &'static str,
    context: String,
    cycle_number: i32,
    reset_attempt: i32,
    round_number: i32,
    /// Stable first official scoring-round start used to recover the permanent
    /// lifecycle-wide wave grid. This is deliberately later than warmup and
    /// any readiness-delayed pre-scoring rounds.
    #[serde(with = "crate::utils::datetime::millis")]
    cycle_starts_at: DateTime<Utc>,
    /// Kept as `cycleEndsAt` for existing reporters; this is the event cutoff.
    #[serde(with = "crate::utils::datetime::millis")]
    cycle_ends_at: DateTime<Utc>,
    /// Platform cutoff for admissible evidence. Arena-specific cadence grids
    /// may end earlier but must never extend past this timestamp.
    #[serde(with = "crate::utils::datetime::millis")]
    scoring_ends_at: DateTime<Utc>,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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
const OBSERVER_CONTEXT_FILL_DEADLINE: std::time::Duration = std::time::Duration::from_millis(1_800);
const OBSERVER_CONTEXT_MAX_BYTES: usize = 512 * 1_024;
const OBSERVER_CONTEXT_VALIDATOR_BYTES: usize = 32;
const OBSERVER_CONTEXT_CACHE_MAX_BYTES: usize =
    OBSERVER_CONTEXT_VALIDATOR_BYTES + OBSERVER_CONTEXT_MAX_BYTES;
const OBSERVER_CONTEXT_GLOBAL_WEIGHT: usize = 16;
const OBSERVER_CONTEXT_CHALLENGE_WEIGHT: usize = 4;
static OBSERVER_CONTEXT_ADMISSION: std::sync::LazyLock<admission::WeightedAdmission> =
    std::sync::LazyLock::new(|| admission::WeightedAdmission::new(OBSERVER_CONTEXT_GLOBAL_WEIGHT));
static OBSERVER_CONTEXT_SF: std::sync::LazyLock<
    crate::utils::single_flight::SingleFlight<ContextFill>,
> = std::sync::LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Clone, Default)]
enum ContextFill {
    Ready {
        generation: i64,
        context: CachedObserverContext,
    },
    Inactive,
    TooLarge,
    #[default]
    Failed,
}

#[derive(Clone)]
struct CachedObserverContext {
    body: bytes::Bytes,
    validator: [u8; OBSERVER_CONTEXT_VALIDATOR_BYTES],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RetryableRefereeError {
    title: String,
    status: u16,
    code: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextVersion {
    V1,
    V2,
}

impl ContextVersion {
    fn cache_suffix(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

fn requested_context_version(headers: &HeaderMap) -> AppResult<ContextVersion> {
    match headers.get(CONTEXT_API_VERSION_HEADER) {
        None => Ok(ContextVersion::V1),
        Some(value) if value.as_bytes() == b"v2" => Ok(ContextVersion::V2),
        Some(_) => Err(AppError::bad_request(
            "Leaderboard context API version is unsupported",
        )),
    }
}

#[cfg(test)]
fn context_v2_requested(headers: &HeaderMap) -> AppResult<bool> {
    requested_context_version(headers).map(|version| version == ContextVersion::V2)
}

pub(super) fn retry_after_response(error: AppError, code: &'static str, seconds: u64) -> Response {
    let status = error.status();
    let retry_after_seconds = seconds.max(1);
    tracing::warn!(
        referee_retry_code = code,
        http_status = status.as_u16(),
        retry_after_seconds,
        "KotH referee request returned a retryable response"
    );
    let mut response = (
        status,
        Json(RetryableRefereeError {
            title: error.to_string(),
            status: status.as_u16(),
            code,
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_str(&retry_after_seconds.to_string())
            .expect("positive integer Retry-After is a valid header"),
    );
    response
}

fn observer_context_cache_key(
    game_id: i32,
    challenge_id: i32,
    generation: i64,
    version: ContextVersion,
) -> String {
    format!(
        "_KothObserverContextV6_{game_id}_{challenge_id}_{generation}_{}",
        version.cache_suffix()
    )
}

fn fresh_observer_context(body: bytes::Bytes) -> CachedObserverContext {
    CachedObserverContext {
        validator: Sha256::digest(&body).into(),
        body,
    }
}

fn encode_observer_context_cache(context: &CachedObserverContext) -> bytes::Bytes {
    let mut encoded = Vec::with_capacity(OBSERVER_CONTEXT_VALIDATOR_BYTES + context.body.len());
    encoded.extend_from_slice(&context.validator);
    encoded.extend_from_slice(&context.body);
    bytes::Bytes::from(encoded)
}

fn decode_observer_context_cache(encoded: bytes::Bytes) -> Option<CachedObserverContext> {
    if encoded.len() <= OBSERVER_CONTEXT_VALIDATOR_BYTES
        || encoded.len() > OBSERVER_CONTEXT_CACHE_MAX_BYTES
    {
        return None;
    }
    let mut validator = [0_u8; OBSERVER_CONTEXT_VALIDATOR_BYTES];
    validator.copy_from_slice(&encoded[..OBSERVER_CONTEXT_VALIDATOR_BYTES]);
    Some(CachedObserverContext {
        body: encoded.slice(OBSERVER_CONTEXT_VALIDATOR_BYTES..),
        validator,
    })
}

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
                  scoring_boundary.start_time_utc AS scoring_starts_at,
                  game.end_time_utc AS cycle_ends_at,
                  game.end_time_utc
                    - ($3::bigint * interval '1 second') AS scoring_ends_at,
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
             JOIN "AdRounds" scoring_boundary
               ON scoring_boundary.game_id = config.game_id
              AND scoring_boundary.number = config.scoring_start_round
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
              AND game.end_time_utc
                    - ($3::bigint * interval '1 second')
                    > scoring_boundary.start_time_utc
              AND round.end_time_utc
                    - ($3::bigint * interval '1 second')
                    > GREATEST(
                        scoring_boundary.start_time_utc,
                        CASE WHEN round.number <= 1
                             THEN round.start_time_utc
                             ELSE round.start_time_utc
                                  - ($3::bigint * interval '1 second')
                        END
                    )
            FOR SHARE OF target"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(crate::services::ad::engine::koth_api::API_WAVE_SETTLEMENT_LAG_SECONDS)
    .fetch_optional(executor)
    .await
    .map_err(|error| {
        admission::referee_database_error(
            error,
            "Leaderboard KotH context is temporarily unavailable",
        )
    })
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub(super) struct EligibleApiCapability {
    pub(super) participation_id: i32,
    pub(super) token: String,
}

pub(super) async fn load_eligible_capabilities<'e, E>(
    executor: E,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Vec<EligibleApiCapability>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as(
        r#"SELECT token.participation_id, token.token
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
              AND NOT token.revocation_pending
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
    .map_err(|error| {
        admission::referee_database_error(
            error,
            "Leaderboard KotH context is temporarily unavailable",
        )
    })
}

async fn context_generation(
    pool: &sqlx::PgPool,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<Option<i64>> {
    sqlx::query_scalar(
        r#"SELECT generation FROM "KothObserverContextGenerations"
            WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| {
        admission::referee_database_error(
            error,
            "Leaderboard KotH context is temporarily unavailable",
        )
    })
}

async fn build_observer_context(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    version: ContextVersion,
) -> AppResult<ContextFill> {
    let mut transaction = crate::utils::database::begin_repeatable_read(st.pg())
        .await
        .map_err(|error| {
            admission::referee_database_error(
                error,
                "Leaderboard KotH context is temporarily unavailable",
            )
        })?;
    let generation: Option<(i64, DateTime<Utc>)> = sqlx::query_as(
        r#"SELECT generation, generated_at
              FROM "KothObserverContextGenerations"
             WHERE game_id = $1 AND challenge_id = $2"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((generation, generated_at)) = generation else {
        return Ok(ContextFill::Inactive);
    };
    let Some(context) = load_active_context(&mut *transaction, game_id, challenge_id).await? else {
        return Ok(ContextFill::Inactive);
    };
    let eligible_capabilities =
        load_eligible_capabilities(&mut *transaction, game_id, challenge_id).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if eligible_capabilities.len() > super::api_contract::MAX_TEAM_ENTRIES {
        return Ok(ContextFill::TooLarge);
    }
    let eligible_tokens: Vec<_> = eligible_capabilities
        .into_iter()
        .map(|capability| capability.token)
        .collect();
    let (wave_window_starts_at, wave_window_ends_at) = context.wave_window();
    let token_hashes = || {
        eligible_tokens
            .iter()
            .map(|token| crate::services::ad::koth_api_capability::token_hash_hex(token))
            .collect()
    };
    let opaque = || context.opaque_context(game_id, challenge_id, &eligible_tokens);
    let serialized = match version {
        ContextVersion::V1 => serde_json::to_vec(&KothObserverContextModel {
            api_version: "v1",
            context: opaque(),
            cycle_number: context.cycle_number,
            reset_attempt: context.reset_attempt,
            round_number: context.round_number,
            cycle_ends_at: context.cycle_ends_at,
            wave_window_starts_at,
            wave_window_ends_at,
            eligible_token_hashes: token_hashes(),
            objective_ids: context.objective_ids.clone().unwrap_or_default(),
            objective_schema_hash: context.objective_schema_hash.as_ref().map(hex::encode),
            generated_at,
        }),
        ContextVersion::V2 => serde_json::to_vec(&KothObserverContextV2Model {
            api_version: "v2",
            context: opaque(),
            cycle_number: context.cycle_number,
            reset_attempt: context.reset_attempt,
            round_number: context.round_number,
            cycle_starts_at: context.scoring_starts_at,
            cycle_ends_at: context.cycle_ends_at,
            scoring_ends_at: context.scoring_ends_at,
            wave_window_starts_at,
            wave_window_ends_at,
            eligible_token_hashes: token_hashes(),
            objective_ids: context.objective_ids.clone().unwrap_or_default(),
            objective_schema_hash: context.objective_schema_hash.as_ref().map(hex::encode),
            generated_at,
        }),
    }
    .map_err(|error| AppError::internal(error.to_string()))?;
    let body = bytes::Bytes::from(serialized);
    if body.len() > OBSERVER_CONTEXT_MAX_BYTES {
        return Ok(ContextFill::TooLarge);
    }
    Ok(ContextFill::Ready {
        generation,
        context: fresh_observer_context(body),
    })
}

async fn observer_context_body(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    version: ContextVersion,
) -> AppResult<CachedObserverContext> {
    let generation = context_generation(st.pg(), game_id, challenge_id)
        .await?
        .ok_or_else(|| AppError::conflict(CONTEXT_INACTIVE_MESSAGE))?;
    let key = observer_context_cache_key(game_id, challenge_id, generation, version);
    if let Some(encoded) = st.cache.get(&key).await {
        if let Some(context) = decode_observer_context_cache(encoded) {
            tracing::debug!(
                referee_operation = "context",
                cache_status = "hit",
                game_id,
                challenge_id,
                generation,
                response_bytes = context.body.len(),
                "served cached KotH referee context"
            );
            return Ok(context);
        }
        st.cache.remove(&key).await;
    }
    let st_for_fill = st.clone();
    let key_for_fill = key.clone();
    let filled = OBSERVER_CONTEXT_SF
        .run_with_timeout(&key, OBSERVER_CONTEXT_FILL_DEADLINE, move || async move {
            if let Some(encoded) = st_for_fill.cache.get(&key_for_fill).await {
                if let Some(context) = decode_observer_context_cache(encoded) {
                    return ContextFill::Ready {
                        generation,
                        context,
                    };
                }
                st_for_fill.cache.remove(&key_for_fill).await;
            }
            let fill_started = std::time::Instant::now();
            match build_observer_context(&st_for_fill, game_id, challenge_id, version).await {
                Ok(ContextFill::Ready {
                    generation: built_generation,
                    context,
                }) => {
                    let built_key = observer_context_cache_key(
                        game_id,
                        challenge_id,
                        built_generation,
                        version,
                    );
                    let encoded = encode_observer_context_cache(&context);
                    st_for_fill
                        .cache
                        .set(&built_key, &encoded, Some(OBSERVER_CONTEXT_TTL))
                        .await;
                    tracing::info!(
                        referee_operation = "context_fill",
                        game_id,
                        challenge_id,
                        generation = built_generation,
                        response_bytes = context.body.len(),
                        elapsed_ms = fill_started.elapsed().as_millis(),
                        "built KotH referee context"
                    );
                    ContextFill::Ready {
                        generation: built_generation,
                        context,
                    }
                }
                Ok(other) => other,
                Err(error) => {
                    tracing::warn!(
                        game_id,
                        challenge_id,
                        error = %error,
                        "KotH observer context cache fill failed"
                    );
                    ContextFill::Failed
                }
            }
        })
        .await;
    match filled {
        ContextFill::Ready { context, .. } => Ok(context),
        ContextFill::Inactive => Err(AppError::conflict(CONTEXT_INACTIVE_MESSAGE)),
        ContextFill::TooLarge => Err(AppError::conflict(
            "Leaderboard KotH context exceeds the supported response size",
        )),
        ContextFill::Failed => Err(AppError::unavailable(
            "Leaderboard KotH context is temporarily unavailable",
        )),
    }
}

fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    fn weak_value(value: &str) -> &str {
        let value = value.trim();
        value.strip_prefix("W/").unwrap_or(value)
    }
    headers.get_all(header::IF_NONE_MATCH).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || weak_value(candidate) == weak_value(etag)
            })
        })
    })
}

fn context_response(context: CachedObserverContext, headers: &HeaderMap) -> AppResult<Response> {
    let etag = format!("\"rsctf-koth-context-{}\"", hex::encode(context.validator));
    let mut response = if if_none_match(headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        context.body.into_response()
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    response.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static(CONTEXT_API_VERSION_HEADER),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|error| AppError::internal(error.to_string()))?,
    );
    if response.status() != StatusCode::NOT_MODIFIED {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    Ok(response)
}

/// Public, non-secret fence bound to the exact container, cycle, and scoring tick.
pub async fn observer_context(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let version = requested_context_version(&headers)?;
    let scope = format!("context:{game_id}:{challenge_id}");
    let Some(_permit) =
        OBSERVER_CONTEXT_ADMISSION.try_acquire(scope, 1, OBSERVER_CONTEXT_CHALLENGE_WEIGHT)
    else {
        return Ok(retry_after_response(
            AppError::too_many_requests(1),
            "koth_context_admission",
            1,
        ));
    };
    let body = match tokio::time::timeout(
        OBSERVER_CONTEXT_DEADLINE,
        observer_context_body(&st, game_id, challenge_id, version),
    )
    .await
    {
        Ok(Ok(body)) => body,
        Ok(Err(error @ AppError::ServiceUnavailable(_))) => {
            return Ok(retry_after_response(error, "koth_context_unavailable", 1));
        }
        Ok(Err(AppError::Conflict(title))) if title == CONTEXT_INACTIVE_MESSAGE => {
            return Ok(retry_after_response(
                AppError::Conflict(title),
                "stale_context",
                1,
            ));
        }
        Ok(Err(error)) => return Err(error),
        Err(_) => {
            return Ok(retry_after_response(
                AppError::unavailable("Leaderboard KotH context timed out; retry later"),
                "koth_context_timeout",
                1,
            ));
        }
    };
    context_response(body, &headers)
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
#[path = "tests.rs"]
mod tests;
