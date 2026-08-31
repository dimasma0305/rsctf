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
use std::collections::HashMap;

use crate::app_state::SharedState;
use crate::services::event_security::{
    ingest_batch, issue_solve_receipt, IssueSolveReceipt, IssuedSolveReceipt, SensorFlagPattern,
    SensorGameSnapshot, SensorPeer, SensorSnapshot, TelemetryBatch, TelemetryIngestResult,
    MAX_PATTERNS, MAX_PATTERN_BYTES, MAX_SENSOR_PEERS,
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
                    AND OCTET_LENGTH(flag.flag) BETWEEN 1 AND $3
                    AND NOT rsctf_flag_has_boundary_whitespace(flag.flag)
                 UNION ALL
                 SELECT variant.challenge_id, variant.participation_id,
                        variant.manifest->>'flag'
                   FROM "ChallengeVariants" variant
                   JOIN "GameChallenges" challenge ON challenge.id = variant.challenge_id
                  WHERE variant.game_id = $1 AND variant.frozen_at_utc IS NOT NULL
                    AND challenge.game_id = $1
                    AND challenge.is_enabled = TRUE AND challenge.review_status = 0
                    AND jsonb_typeof(variant.manifest->'flag') = 'string'
                    AND OCTET_LENGTH(variant.manifest->>'flag') BETWEEN 1 AND $3
                    AND NOT rsctf_flag_has_boundary_whitespace(variant.manifest->>'flag')
             ) source
            ORDER BY source.challenge_id, source.owning_participation_id
            LIMIT $2"#,
    )
    .bind(game_id)
    .bind(i64::try_from(MAX_PATTERNS + 1).unwrap_or(i64::MAX))
    .bind(i32::try_from(crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES).unwrap_or(127))
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
    let mut cache = st.event_sensor_snapshot.lock().await;
    if let Some((created, snapshot)) = cache.as_ref() {
        if created.elapsed() < std::time::Duration::from_secs(5) {
            return Ok((
                [
                    (header::CACHE_CONTROL, "private, no-store"),
                    (header::PRAGMA, "no-cache"),
                ],
                Json(snapshot.clone()),
            )
                .into_response());
        }
    }
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
    let game_ids = games.iter().map(|game| game.0).collect::<Vec<_>>();
    let mut peers_by_game = HashMap::<i32, Vec<SensorPeer>>::new();
    let peers = sqlx::query_as::<_, (i32, uuid::Uuid, uuid::Uuid, i32, String, String, i32)>(
        r#"SELECT game_id, id, user_id, participation_id, public_key, address, generation
             FROM "EventVpnUserPeers"
            WHERE game_id = ANY($1) AND revoked_at_utc IS NULL
            ORDER BY game_id, id
            LIMIT $2"#,
    )
    .bind(&game_ids)
    .bind(i64::try_from(MAX_SENSOR_PEERS + 1).unwrap_or(i64::MAX))
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if peers.len() > MAX_SENSOR_PEERS {
        return Err(AppError::unavailable(
            "Active Event-VPN peers exceed the sensor snapshot limit",
        ));
    }
    for (game_id, peer_id, user_id, participation_id, public_key, address, generation) in peers {
        peers_by_game.entry(game_id).or_default().push(SensorPeer {
            endpoint: live_endpoints
                .get(&public_key)
                .map(|endpoint| endpoint.ip().to_string()),
            peer_id,
            user_id,
            participation_id,
            public_key,
            address,
            generation,
        });
    }
    let pattern_rows = sqlx::query_as::<_, (i32, i32, i32, String)>(
        r#"SELECT source.game_id, source.challenge_id,
                  source.owning_participation_id, source.flag
             FROM (
                 SELECT challenge.game_id, challenge.id AS challenge_id,
                        instance.participation_id AS owning_participation_id, flag.flag
                   FROM "GameChallenges" challenge
                   JOIN "Games" game ON game.id = challenge.game_id
                    AND game.vpn_flag_scan_enabled = TRUE
                   JOIN "GameInstances" instance ON instance.challenge_id = challenge.id
                   JOIN "FlagContexts" flag ON flag.id = instance.flag_id
                  WHERE challenge.game_id = ANY($1) AND challenge.is_enabled = TRUE
                    AND challenge.review_status = 0 AND challenge."Type" NOT IN (4, 5)
                 UNION ALL
                 SELECT variant.game_id, variant.challenge_id, variant.participation_id,
                        variant.manifest->>'flag'
                   FROM "ChallengeVariants" variant
                   JOIN "Games" game ON game.id = variant.game_id
                    AND game.vpn_flag_scan_enabled = TRUE
                   JOIN "GameChallenges" challenge
                     ON challenge.game_id = variant.game_id AND challenge.id = variant.challenge_id
                  WHERE variant.game_id = ANY($1) AND variant.frozen_at_utc IS NOT NULL
                    AND challenge.is_enabled = TRUE AND challenge.review_status = 0
                    AND jsonb_typeof(variant.manifest->'flag') = 'string'
             ) source
            ORDER BY source.game_id, source.challenge_id, source.owning_participation_id
            LIMIT $2"#,
    )
    .bind(&game_ids)
    .bind(i64::try_from(MAX_PATTERNS + 1).unwrap_or(i64::MAX))
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let pattern_bytes = pattern_rows
        .iter()
        .map(|row| row.3.len())
        .fold(0_usize, usize::saturating_add);
    if pattern_rows.len() > MAX_PATTERNS || pattern_bytes > MAX_PATTERN_BYTES {
        return Err(AppError::unavailable(
            "Active event flag patterns exceed the sensor limit",
        ));
    }
    let mut patterns_by_game = HashMap::<i32, Vec<SensorFlagPattern>>::new();
    for (game_id, challenge_id, owning_participation_id, pattern) in pattern_rows {
        let value_hash = crate::services::event_security::flag_value_hash(
            &st.config.event_vpn_credential_key,
            game_id,
            challenge_id,
            &pattern,
        )?;
        patterns_by_game
            .entry(game_id)
            .or_default()
            .push(SensorFlagPattern {
                challenge_id,
                owning_participation_id,
                pattern,
                value_hash: hex::encode(value_hash),
            });
    }
    let mut output = Vec::with_capacity(games.len());
    for (game_id, behavior, flag_scan, dns, asn, devices) in games {
        output.push(SensorGameSnapshot {
            game_id,
            behavior_telemetry_enabled: behavior,
            flag_scan_enabled: flag_scan,
            provider_dns_telemetry_enabled: dns,
            source_asn_telemetry_enabled: asn,
            device_sharing_telemetry_enabled: devices,
            peers: peers_by_game.remove(&game_id).unwrap_or_default(),
            flag_patterns: if flag_scan {
                patterns_by_game.remove(&game_id).unwrap_or_default()
            } else {
                Vec::new()
            },
        });
    }
    let snapshot = SensorSnapshot {
        generated_at_utc: chrono::Utc::now(),
        games: output,
    };
    *cache = Some((tokio::time::Instant::now(), snapshot.clone()));
    Ok((
        [
            (header::CACHE_CONTROL, "private, no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(snapshot),
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
        assert!(authorize(&HeaderMap::new(), secret).is_err());
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic machine-secret-0123456789abcdef012345"),
        );
        assert!(authorize(&headers, secret).is_err());
    }
}
