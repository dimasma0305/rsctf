//! Signed, challenge-scoped API-arena referee.
//!
//! Referees report bounded evidence ratios, never points. The round checker is
//! the only component that can turn a stable, healthy snapshot into score.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

mod admin;
mod submission;

pub use admin::{get_observer, revoke_observer, rotate_observer};
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
    pub(super) container_id: String,
    pub(super) round_id: i32,
    pub(super) round_number: i32,
    pub(super) round_starts_at: DateTime<Utc>,
    pub(super) round_ends_at: DateTime<Utc>,
}

impl ActiveObserverContext {
    pub(super) fn opaque_context(&self, game_id: i32, challenge_id: i32) -> String {
        opaque_context(
            game_id,
            challenge_id,
            self.target_id,
            self.cycle_id,
            self.reset_attempt,
            &self.container_id,
            self.round_id,
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
    #[serde(with = "crate::utils::datetime::millis")]
    round_starts_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    round_ends_at: DateTime<Utc>,
    eligible_token_hashes: Vec<String>,
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
                  target.container_id AS container_id,
                  round.id AS round_id, round.number AS round_number,
                  round.start_time_utc AS round_starts_at,
                  round.end_time_utc AS round_ends_at
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
             JOIN LATERAL (
               SELECT crown.id, crown.cycle_number, crown.reset_attempt,
                      crown.replacement_container_id,
                      crown.planned_start_round, crown.planned_end_round
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
                  AND scoring.number BETWEEN cycle.planned_start_round
                                         AND cycle.planned_end_round
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

/// Public, non-secret fence bound to the exact container, cycle, and scoring tick.
pub async fn observer_context(
    State(st): State<SharedState>,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<KothObserverContextModel>> {
    let context = load_active_context(st.pg(), game_id, challenge_id)
        .await?
        .ok_or_else(|| AppError::conflict("KotH API arena context is not active"))?;
    let eligible_tokens: Vec<String> = sqlx::query_scalar(
        r#"SELECT token.token
             FROM "KothTokens" token
             JOIN "Participations" participation
               ON participation.id = token.participation_id
              AND participation.game_id = $5
              AND participation.status = $6
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "KothOfficialConfigs" config ON config.game_id = $5
             JOIN LATERAL jsonb_array_elements(config.roster_snapshot) roster(item)
               ON participation.id = CASE jsonb_typeof(roster.item)
                    WHEN 'number' THEN (roster.item #>> '{}')::integer
                    WHEN 'object' THEN
                      NULLIF(roster.item->>'participationId', '')::integer
                    ELSE NULL
                  END
            WHERE token.target_id = $1
              AND token.cycle_id = $2
              AND token.challenge_id = $3
              AND token.reset_attempt = $4
              AND token.revoked_at IS NULL
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
                     WHERE account.id IS NULL OR account.role = $7
              )
            ORDER BY token.participation_id"#,
    )
    .bind(context.target_id)
    .bind(context.cycle_id)
    .bind(challenge_id)
    .bind(context.reset_attempt)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if eligible_tokens.len() > super::api_contract::MAX_TEAM_ENTRIES {
        return Err(AppError::conflict(
            "KotH API arena roster exceeds the supported 2,000 teams",
        ));
    }
    Ok(RequestResponse::ok(KothObserverContextModel {
        api_version: "v1",
        context: context.opaque_context(game_id, challenge_id),
        cycle_number: context.cycle_number,
        reset_attempt: context.reset_attempt,
        round_number: context.round_number,
        round_starts_at: context.round_starts_at,
        round_ends_at: context.round_ends_at,
        eligible_token_hashes: eligible_tokens
            .iter()
            .map(|token| hex::encode(Sha256::digest(token.as_bytes())))
            .collect(),
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

fn opaque_context(
    game_id: i32,
    challenge_id: i32,
    target_id: i32,
    cycle_id: i64,
    reset_attempt: i32,
    container_id: &str,
    round_id: i32,
) -> String {
    let mut digest = Sha256::new();
    digest.update(game_id.to_be_bytes());
    digest.update(challenge_id.to_be_bytes());
    digest.update(target_id.to_be_bytes());
    digest.update(cycle_id.to_be_bytes());
    digest.update(reset_attempt.to_be_bytes());
    digest.update((container_id.len() as u64).to_be_bytes());
    digest.update(container_id.as_bytes());
    digest.update(round_id.to_be_bytes());
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
        let base = opaque_context(7, 9, 3, 41, 1, "container-a", 51);
        assert_eq!(base.len(), 64);
        assert_ne!(base, opaque_context(8, 9, 3, 41, 1, "container-a", 51));
        assert_ne!(base, opaque_context(7, 9, 4, 41, 1, "container-a", 51));
        assert_ne!(base, opaque_context(7, 9, 3, 42, 1, "container-a", 51));
        assert_ne!(base, opaque_context(7, 9, 3, 41, 2, "container-a", 51));
        assert_ne!(base, opaque_context(7, 9, 3, 41, 1, "container-b", 51));
        assert_ne!(base, opaque_context(7, 9, 3, 41, 1, "container-a", 52));
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
