//! Narrow machine API for the event-network sensor and trusted solve verifier.
//!
//! These routes are exposed only by the all/control network surface, never by a
//! stateless web replica. Authentication uses dedicated deployment secrets; the
//! VPN credential-encryption key is deliberately not shared with either client.

use axum::extract::State;
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::app_state::SharedState;
use crate::services::event_security::{
    ingest_batch, issue_solve_receipt, IssueSolveReceipt, IssuedSolveReceipt, SensorFlagPattern,
    SensorGameSnapshot, SensorPeer, SensorSnapshot, TelemetryBatch, TelemetryIngestResult,
    MAX_PATTERNS, MAX_PATTERN_BYTES,
};
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/internal/event-security/telemetry", post(telemetry))
        .route(
            "/api/internal/event-security/flag-patterns/{gameId}",
            get(flag_patterns),
        )
        .route(
            "/api/internal/event-security/snapshot",
            get(sensor_snapshot),
        )
        .route(
            "/api/internal/event-security/solve-receipts",
            post(solve_receipt),
        )
}

async fn load_sensor_flag_patterns(
    st: &SharedState,
    game_id: i32,
) -> AppResult<Vec<SensorFlagPattern>> {
    let rows = sqlx::query_as::<_, (i32, i32, String)>(
        r#"SELECT source.challenge_id, source.owning_participation_id, source.flag
             FROM (
                 SELECT challenge.id AS challenge_id,
                        instance.participation_id AS owning_participation_id,
                        flag.flag
                   FROM "GameChallenges" challenge
                   JOIN "GameInstances" instance ON instance.challenge_id = challenge.id
                   JOIN "FlagContexts" flag ON flag.id = instance.flag_id
                  WHERE challenge.game_id = $1
                    AND challenge.is_enabled = TRUE AND challenge.review_status = 0
                    AND challenge."Type" NOT IN (4, 5)
                 UNION ALL
                 SELECT variant.challenge_id, variant.participation_id,
                        variant.manifest->>'flag'
                   FROM "ChallengeVariants" variant
                   JOIN "GameChallenges" challenge ON challenge.id = variant.challenge_id
                  WHERE variant.game_id = $1 AND variant.frozen_at_utc IS NOT NULL
                    AND challenge.game_id = $1
                    AND challenge.is_enabled = TRUE AND challenge.review_status = 0
                    AND jsonb_typeof(variant.manifest->'flag') = 'string'
             ) source
            ORDER BY source.challenge_id, source.owning_participation_id
            LIMIT $2"#,
    )
    .bind(game_id)
    .bind(i64::try_from(MAX_PATTERNS + 1).unwrap_or(i64::MAX))
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if rows.len() > MAX_PATTERNS {
        return Err(AppError::unavailable(
            "Active event flag-pattern count exceeds the sensor limit",
        ));
    }
    let pattern_bytes = rows
        .iter()
        .map(|(_, _, pattern)| pattern.len())
        .try_fold(0usize, usize::checked_add)
        .unwrap_or(usize::MAX);
    if pattern_bytes > MAX_PATTERN_BYTES {
        return Err(AppError::unavailable(
            "Active event flag patterns exceed the sensor memory budget",
        ));
    }
    rows.into_iter()
        .map(|(challenge_id, owning_participation_id, pattern)| {
            let value_hash = crate::services::event_security::flag_value_hash(
                &st.config.event_vpn_credential_key,
                game_id,
                challenge_id,
                &pattern,
            )?;
            Ok(SensorFlagPattern {
                challenge_id,
                owning_participation_id,
                pattern,
                value_hash: hex::encode(value_hash),
            })
        })
        .collect()
}

async fn sensor_snapshot(State(st): State<SharedState>, headers: HeaderMap) -> AppResult<Response> {
    authorize(&headers, &st.config.event_sensor_token)?;
    let live_endpoints = if crate::services::ad_vpn::enabled() {
        match crate::services::ad_vpn::live_peer_endpoints().await {
            Ok(endpoints) => endpoints,
            Err(error) => {
                tracing::warn!(%error, "event sensor endpoint snapshot omitted");
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    let games = sqlx::query_as::<_, (i32, bool, bool, bool, bool, bool)>(
        r#"SELECT id, vpn_behavior_telemetry_enabled, vpn_flag_scan_enabled,
                  vpn_provider_dns_telemetry_enabled,
                  vpn_source_asn_telemetry_enabled,
                  vpn_device_sharing_telemetry_enabled
             FROM "Games"
            WHERE deletion_pending = FALSE
              AND start_time_utc <= clock_timestamp()
              AND clock_timestamp() < end_time_utc
              AND (
                  vpn_behavior_telemetry_enabled OR vpn_flag_scan_enabled
                  OR vpn_provider_dns_telemetry_enabled
                  OR vpn_source_asn_telemetry_enabled
                  OR vpn_device_sharing_telemetry_enabled
              )
            ORDER BY id"#,
    )
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut output = Vec::with_capacity(games.len());
    let mut total_patterns = 0usize;
    for (game_id, behavior, flag_scan, dns, asn, devices) in games {
        let peers = sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid, i32, String, String, i32)>(
            r#"SELECT id, user_id, participation_id, public_key, address, generation
                 FROM "EventVpnUserPeers"
                WHERE game_id = $1 AND revoked_at_utc IS NULL
                ORDER BY id"#,
        )
        .bind(game_id)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .into_iter()
        .map(
            |(peer_id, user_id, participation_id, public_key, address, generation)| SensorPeer {
                endpoint: live_endpoints
                    .get(&public_key)
                    .map(|endpoint| endpoint.ip().to_string()),
                peer_id,
                user_id,
                participation_id,
                public_key,
                address,
                generation,
            },
        )
        .collect::<Vec<_>>();
        let mut flag_patterns = Vec::new();
        if flag_scan {
            flag_patterns = load_sensor_flag_patterns(&st, game_id).await?;
            total_patterns = total_patterns.saturating_add(flag_patterns.len());
            if total_patterns > MAX_PATTERNS {
                return Err(AppError::unavailable(
                    "Active event flag-pattern count exceeds the sensor limit",
                ));
            }
        }
        output.push(SensorGameSnapshot {
            game_id,
            behavior_telemetry_enabled: behavior,
            flag_scan_enabled: flag_scan,
            provider_dns_telemetry_enabled: dns,
            source_asn_telemetry_enabled: asn,
            device_sharing_telemetry_enabled: devices,
            peers,
            flag_patterns,
        });
    }
    Ok((
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(SensorSnapshot {
            generated_at_utc: chrono::Utc::now(),
            games: output,
        }),
    )
        .into_response())
}

fn authorize(headers: &HeaderMap, expected: &str) -> AppResult<()> {
    if expected.len() < 32 || expected.chars().any(char::is_whitespace) {
        return Err(AppError::unavailable(
            "Event-security machine credentials are not configured",
        ));
    }
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .ok_or(AppError::Unauthorized)?;
    crate::utils::crypto_utils::ct_eq(expected, presented)
        .then_some(())
        .ok_or(AppError::Unauthorized)
}

async fn telemetry(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(batch): Json<TelemetryBatch>,
) -> AppResult<RequestResponse<TelemetryIngestResult>> {
    authorize(&headers, &st.config.event_sensor_token)?;
    Ok(RequestResponse::ok(ingest_batch(&st, &batch).await?))
}

async fn flag_patterns(
    State(st): State<SharedState>,
    headers: HeaderMap,
    axum::extract::Path(game_id): axum::extract::Path<i32>,
) -> AppResult<Response> {
    authorize(&headers, &st.config.event_sensor_token)?;
    let enabled: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "Games"
                WHERE id = $1 AND vpn_flag_scan_enabled = TRUE
                  AND start_time_utc <= clock_timestamp()
                  AND clock_timestamp() < end_time_utc
                  AND deletion_pending = FALSE
           )"#,
    )
    .bind(game_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !enabled {
        return Err(AppError::not_found("Active flag scanning is not enabled"));
    }
    let patterns = load_sensor_flag_patterns(&st, game_id).await?;
    Ok((
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(patterns),
    )
        .into_response())
}

async fn solve_receipt(
    State(st): State<SharedState>,
    headers: HeaderMap,
    Json(request): Json<IssueSolveReceipt>,
) -> AppResult<RequestResponse<IssuedSolveReceipt>> {
    authorize(&headers, &st.config.solve_receipt_issuer_token)?;
    Ok(RequestResponse::ok(
        issue_solve_receipt(&st, request).await?,
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap, HeaderValue};

    use super::authorize;

    #[test]
    fn machine_auth_is_dedicated_and_exact() {
        let secret = "machine-secret-0123456789abcdef012345";
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
        );
        assert!(authorize(&headers, secret).is_ok());
        assert!(authorize(&headers, "different-secret-0123456789abcdef0").is_err());
        assert!(authorize(&headers, "short").is_err());
    }
}
