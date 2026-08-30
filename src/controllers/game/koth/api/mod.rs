//! Signed, challenge-scoped Leaderboard evidence reporting.
//!
//! Managed targets (or legacy external reporters) submit bounded evidence
//! ratios, never points. The round checker is
//! the only component that can turn a stable, healthy snapshot into score.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

mod admin;
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

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
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
    .map_err(|error| AppError::internal(error.to_string()))
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
    .map_err(|error| AppError::internal(error.to_string()))
}

/// Public, non-secret fence bound to the exact container, cycle, and scoring tick.
async fn load_observer_context_data(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<(
    ActiveObserverContext,
    DateTime<Utc>,
    DateTime<Utc>,
    Vec<String>,
)> {
    let context = load_active_context(st.pg(), game_id, challenge_id)
        .await?
        .ok_or_else(|| AppError::conflict("Leaderboard KotH context is not active"))?;
    let (wave_window_starts_at, wave_window_ends_at) = context.wave_window();
    let eligible_capabilities = load_eligible_capabilities(st.pg(), game_id, challenge_id).await?;
    if eligible_capabilities.len() > super::api_contract::MAX_TEAM_ENTRIES {
        return Err(AppError::conflict(
            "Leaderboard KotH roster exceeds the supported 2,000 teams",
        ));
    }
    let eligible_tokens: Vec<_> = eligible_capabilities
        .iter()
        .map(|capability| capability.token.clone())
        .collect();
    Ok((
        context,
        wave_window_starts_at,
        wave_window_ends_at,
        eligible_tokens,
    ))
}

fn context_v2_requested(headers: &HeaderMap) -> AppResult<bool> {
    match headers.get(CONTEXT_API_VERSION_HEADER) {
        None => Ok(false),
        Some(value) if value.as_bytes() == b"v2" => Ok(true),
        Some(_) => Err(AppError::bad_request(
            "Leaderboard context API version is unsupported",
        )),
    }
}

fn versioned_context_response<T: Serialize>(model: T) -> Response {
    let mut response = RequestResponse::ok(model).into_response();
    response.headers_mut().insert(
        header::VARY,
        HeaderValue::from_static(CONTEXT_API_VERSION_HEADER),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub async fn observer_context(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let use_v2 = context_v2_requested(&headers)?;
    let (context, wave_window_starts_at, wave_window_ends_at, eligible_tokens) =
        load_observer_context_data(&st, game_id, challenge_id).await?;
    let token_hashes = || {
        eligible_tokens
            .iter()
            .map(|token| crate::services::ad::koth_api_capability::token_hash_hex(token))
            .collect()
    };
    if use_v2 {
        return Ok(versioned_context_response(KothObserverContextV2Model {
            api_version: "v2",
            context: context.opaque_context(game_id, challenge_id, &eligible_tokens),
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
            generated_at: Utc::now(),
        }));
    }
    Ok(versioned_context_response(KothObserverContextModel {
        api_version: "v1",
        context: context.opaque_context(game_id, challenge_id, &eligible_tokens),
        cycle_number: context.cycle_number,
        reset_attempt: context.reset_attempt,
        round_number: context.round_number,
        cycle_ends_at: context.cycle_ends_at,
        wave_window_starts_at,
        wave_window_ends_at,
        eligible_token_hashes: token_hashes(),
        objective_ids: context.objective_ids.clone().unwrap_or_default(),
        objective_schema_hash: context.objective_schema_hash.as_ref().map(hex::encode),
        generated_at: Utc::now(),
    }))
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

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn observation_rebase_removes_every_ineligible_identity_and_repairs_crowns() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "Participations" (
              id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              team_id INTEGER NOT NULL, status SMALLINT NOT NULL
            );
            CREATE TEMP TABLE "Teams" (
              id INTEGER PRIMARY KEY, captain_id INTEGER NOT NULL,
              deletion_pending BOOLEAN NOT NULL
            );
            CREATE TEMP TABLE "TeamMembers" (team_id INTEGER, user_id INTEGER);
            CREATE TEMP TABLE "AspNetUsers" (id INTEGER PRIMARY KEY, role SMALLINT NOT NULL);
            CREATE TEMP TABLE "KothOfficialConfigs" (
              game_id INTEGER PRIMARY KEY, roster_snapshot JSONB NOT NULL
            );
            CREATE TEMP TABLE "KothApiTeamTokens" (
              game_id INTEGER NOT NULL, challenge_id INTEGER NOT NULL,
              participation_id INTEGER NOT NULL, token TEXT NOT NULL UNIQUE,
              generation INTEGER NOT NULL DEFAULT 1,
              rotated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
              last_used_at TIMESTAMPTZ,
              revocation_pending BOOLEAN NOT NULL DEFAULT FALSE,
              PRIMARY KEY (game_id, challenge_id, participation_id)
            );
            CREATE TEMP TABLE "KothApiSnapshots" (
              target_id INTEGER PRIMARY KEY, game_id INTEGER NOT NULL,
              challenge_id INTEGER NOT NULL, snapshot_hash BYTEA NOT NULL
            );
            CREATE TEMP TABLE "KothApiSnapshotScores" (
              target_id INTEGER NOT NULL, wave_id TEXT NOT NULL,
              participation_id INTEGER NOT NULL,
              activity_earned BIGINT NOT NULL,
              activity_possible BIGINT NOT NULL,
              objective_earned BIGINT NOT NULL,
              objective_possible BIGINT NOT NULL,
              objective_count SMALLINT NOT NULL,
              is_crown BOOLEAN NOT NULL,
              PRIMARY KEY (target_id, wave_id, participation_id)
            );
            CREATE UNIQUE INDEX uq_test_koth_api_crown
              ON "KothApiSnapshotScores" (target_id, wave_id)
              WHERE is_crown;
            INSERT INTO "KothOfficialConfigs" VALUES
              (7, '[11,12,13,14,15]');
            INSERT INTO "Participations" VALUES
              (11, 7, 21, 1), (12, 7, 22, 3), (13, 7, 23, 1),
              (14, 7, 24, 1), (15, 7, 25, 1);
            INSERT INTO "Teams" VALUES
              (21, 101, FALSE), (22, 102, FALSE), (23, 103, TRUE),
              (24, 104, FALSE), (25, 105, FALSE);
            INSERT INTO "AspNetUsers" VALUES
              (101, 1), (102, 1), (103, 1), (104, 1), (105, 1),
              (204, 0);
            INSERT INTO "TeamMembers" VALUES (24, 204), (25, 205);
            INSERT INTO "KothApiTeamTokens"
              (game_id, challenge_id, participation_id, token) VALUES
              (7, 9, 11, 'koth_eligible_team'),
              (7, 9, 12, 'koth_suspended_team'),
              (7, 9, 13, 'koth_deleting_team'),
              (7, 9, 14, 'koth_banned_team'),
              (7, 9, 15, 'koth_missing_account');
            INSERT INTO "KothApiSnapshots" VALUES
              (3, 7, 9, decode(repeat('11', 32), 'hex'));
            INSERT INTO "KothApiSnapshotScores" VALUES
              (3, 'status', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'status', 12, 1, 1, 3, 4, 1, TRUE),
              (3, 'deletion', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'deletion', 13, 1, 1, 3, 4, 1, TRUE),
              (3, 'banned', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'banned', 14, 1, 1, 3, 4, 1, TRUE),
              (3, 'missing-account', 11, 1, 1, 1, 2, 1, FALSE),
              (3, 'missing-account', 15, 1, 1, 3, 4, 1, TRUE);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let before: Vec<u8> = sqlx::query_scalar(
            r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 3"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        let mut transaction = connection.begin().await.unwrap();
        let eligible = load_eligible_capabilities(&mut *transaction, 7, 9)
            .await
            .unwrap();
        assert_eq!(
            eligible
                .iter()
                .map(|capability| capability.participation_id)
                .collect::<Vec<_>>(),
            [11]
        );
        assert_eq!(
            crate::services::ad::koth_api_capability::retain_eligible_unsettled_scores(
                &mut transaction,
                7,
                9,
                3,
                &[11],
            )
            .await
            .unwrap(),
            4
        );
        let rows: Vec<(String, i32, bool)> = sqlx::query_as(
            r#"SELECT wave_id, participation_id, is_crown
                 FROM "KothApiSnapshotScores"
                ORDER BY wave_id, participation_id"#,
        )
        .fetch_all(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("banned".to_string(), 11, true),
                ("deletion".to_string(), 11, true),
                ("missing-account".to_string(), 11, true),
                ("status".to_string(), 11, true),
            ]
        );
        let after: Vec<u8> = sqlx::query_scalar(
            r#"SELECT snapshot_hash FROM "KothApiSnapshots" WHERE target_id = 3"#,
        )
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_ne!(after, before);

        let rotated_challenges =
            crate::services::ad::koth_api_capability::force_rotate_event_capabilities(
                &mut transaction,
                7,
                &[12, 14, 15],
            )
            .await
            .unwrap();
        assert_eq!(rotated_challenges.into_iter().collect::<Vec<_>>(), [9]);
        sqlx::raw_sql(
            r#"UPDATE "Participations" SET status = 1 WHERE id = 12;
               UPDATE "AspNetUsers" SET role = 1 WHERE id = 204;
               INSERT INTO "AspNetUsers" VALUES (205, 1);"#,
        )
        .execute(&mut *transaction)
        .await
        .unwrap();
        let restored = load_eligible_capabilities(&mut *transaction, 7, 9)
            .await
            .unwrap();
        assert_eq!(
            restored
                .iter()
                .map(|capability| capability.participation_id)
                .collect::<Vec<_>>(),
            [11, 12, 14, 15]
        );
        let restored_state: (i64, i64, bool) = sqlx::query_as(
            r#"SELECT COUNT(*),
                      COUNT(*) FILTER (WHERE generation = 2),
                      NOT EXISTS (
                        SELECT 1 FROM "KothApiTeamTokens"
                         WHERE token IN (
                           'koth_suspended_team',
                           'koth_banned_team',
                           'koth_missing_account'
                         )
                      )
                 FROM "KothApiTeamTokens"
                WHERE participation_id = ANY($1)"#,
        )
        .bind([11, 12, 14, 15].as_slice())
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
        assert_eq!(restored_state, (4, 3, true));
        transaction.commit().await.unwrap();
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
}
