//! Idempotent, attribution-checked inserts of sensor telemetry rows. Each
//! row must belong to a peer the game issued, inside the event window and
//! before the peer's revocation.

use super::*;

pub(super) async fn insert_flows(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    rows: &[FlowBucketInput],
) -> AppResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let json = flow_database_rows(rows);
    let count: i64 = sqlx::query_scalar(
        r#"WITH input AS (
               SELECT * FROM jsonb_to_recordset($2::jsonb) AS row(
                   "userId" uuid, "participationId" integer, "peerId" uuid,
                   "challengeId" integer, "containerGeneration" integer,
                   "bucketStartUtc" timestamptz, "packetsUp" bigint,
                   "packetsDown" bigint, "bytesUp" bigint, "bytesDown" bigint,
                   "distinctDestinations" integer, "connectionCount" integer,
                   "activeSeconds" integer
               )
           ), inserted AS (
               INSERT INTO "VpnFlowTelemetryBuckets"
                 (game_id, user_id, participation_id, peer_id, challenge_id,
                  container_generation, bucket_start_utc, packets_up, packets_down,
                  bytes_up, bytes_down, distinct_destinations, connection_count,
                  active_seconds)
               SELECT $1, input."userId", input."participationId", input."peerId",
                      input."challengeId", input."containerGeneration",
                      input."bucketStartUtc", input."packetsUp", input."packetsDown",
                      input."bytesUp", input."bytesDown", input."distinctDestinations",
                      input."connectionCount", input."activeSeconds"
                 FROM input
                 JOIN "EventVpnUserPeers" peer
                   ON peer.id = input."peerId" AND peer.game_id = $1
                  AND peer.user_id = input."userId"
                  AND peer.participation_id = input."participationId"
                  -- Rows reach ingest up to one flush after they were
                  -- observed, so a peer revoked since (at the end, or when a
                  -- member leaves) still owns what it did before revocation.
                  AND (peer.revoked_at_utc IS NULL
                       OR input."bucketStartUtc" < peer.revoked_at_utc)
                 JOIN "Games" game ON game.id = peer.game_id
                WHERE game.vpn_behavior_telemetry_enabled = TRUE
                  -- Five-minute buckets are clock-aligned: the one holding
                  -- the start began before it.
                  AND input."bucketStartUtc" > game.start_time_utc - INTERVAL '5 minutes'
                  AND input."bucketStartUtc" < game.end_time_utc
               ON CONFLICT DO NOTHING RETURNING 1
           ) SELECT COUNT(*)::bigint FROM inserted"#,
    )
    .bind(game_id)
    .bind(sqlx::types::Json(json))
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

/// The public wire format uses Unix milliseconds, while PostgreSQL's
/// `jsonb_to_recordset(... timestamptz)` expects an RFC 3339 value. Build the
/// internal bulk payload explicitly so a valid API timestamp cannot turn into
/// a database parse error.
pub(super) fn flow_database_rows(rows: &[FlowBucketInput]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|row| {
            serde_json::json!({
                "userId": row.user_id,
                "participationId": row.participation_id,
                "peerId": row.peer_id,
                "challengeId": row.challenge_id,
                "containerGeneration": row.container_generation,
                "bucketStartUtc": row.bucket_start_utc,
                "packetsUp": row.packets_up,
                "packetsDown": row.packets_down,
                "bytesUp": row.bytes_up,
                "bytesDown": row.bytes_down,
                "distinctDestinations": row.distinct_destinations,
                "connectionCount": row.connection_count,
                "activeSeconds": row.active_seconds,
            })
        })
        .collect()
}

pub(super) async fn insert_dns(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    rows: &[DnsProviderBucketInput],
) -> AppResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let json = dns_database_rows(rows);
    let count: i64 = sqlx::query_scalar(
        r#"WITH input AS (
               SELECT * FROM jsonb_to_recordset($2::jsonb) AS row(
                   "userId" uuid, "participationId" integer, "peerId" uuid,
                   "providerCategory" smallint, "bucketStartUtc" timestamptz,
                   "queryCount" integer, "firstSeenAtUtc" timestamptz,
                   "lastSeenAtUtc" timestamptz
               )
           ), deduped_input AS MATERIALIZED (
               SELECT DISTINCT ON (
                          "userId", "participationId", "peerId",
                          "providerCategory", "bucketStartUtc"
                      ) *
                 FROM input
                ORDER BY "userId", "participationId", "peerId",
                         "providerCategory", "bucketStartUtc",
                         "lastSeenAtUtc" DESC, "firstSeenAtUtc", "queryCount" DESC
           ), inserted AS (
               INSERT INTO "VpnDnsProviderBuckets"
                 (game_id, user_id, participation_id, peer_id, provider_category,
                  bucket_start_utc, query_count, first_seen_at_utc, last_seen_at_utc)
               SELECT $1, input."userId", input."participationId", input."peerId",
                      input."providerCategory", input."bucketStartUtc", input."queryCount",
                      input."firstSeenAtUtc", input."lastSeenAtUtc"
                 FROM deduped_input input
                 JOIN "EventVpnUserPeers" peer
                   ON peer.id = input."peerId" AND peer.game_id = $1
                  AND peer.user_id = input."userId"
                  AND peer.participation_id = input."participationId"
                  AND (peer.revoked_at_utc IS NULL
                       OR input."firstSeenAtUtc" < peer.revoked_at_utc)
                 JOIN "Games" game ON game.id = peer.game_id
                WHERE game.vpn_provider_dns_telemetry_enabled = TRUE
                  AND input."firstSeenAtUtc" >= game.start_time_utc
                  AND input."lastSeenAtUtc" < game.end_time_utc
                  -- Reconciliation uses a BEFORE INSERT stamp. Reject an
                  -- exact replay before the trigger; ON CONFLICT still fences
                  -- concurrent races after the input batch is deduplicated.
                  AND NOT EXISTS (
                      SELECT 1 FROM "VpnDnsProviderBuckets" existing
                       WHERE existing.game_id = $1
                         AND existing.user_id = input."userId"
                         AND existing.participation_id = input."participationId"
                         AND existing.peer_id = input."peerId"
                         AND existing.provider_category = input."providerCategory"
                         AND existing.bucket_start_utc = input."bucketStartUtc"
                  )
               ON CONFLICT DO NOTHING RETURNING 1
           ) SELECT COUNT(*)::bigint FROM inserted"#,
    )
    .bind(game_id)
    .bind(sqlx::types::Json(json))
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

pub(super) fn dns_database_rows(rows: &[DnsProviderBucketInput]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|row| {
            serde_json::json!({
                "userId": row.user_id,
                "participationId": row.participation_id,
                "peerId": row.peer_id,
                "providerCategory": row.provider_category,
                "bucketStartUtc": row.bucket_start_utc,
                "queryCount": row.query_count,
                "firstSeenAtUtc": row.first_seen_at_utc,
                "lastSeenAtUtc": row.last_seen_at_utc,
            })
        })
        .collect()
}

pub(super) async fn insert_networks(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    rows: &[PeerNetworkInput],
) -> AppResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let values = rows
        .iter()
        .map(|row| {
            Ok(serde_json::json!({
                "userId": row.user_id,
                "participationId": row.participation_id,
                "peerId": row.peer_id,
                "endpointHash": hex::encode(decode_hash(&row.endpoint_hash)?),
                "sourceAsn": row.source_asn,
                "networkClass": row.network_class,
                "firstSeenAtUtc": row.first_seen_at_utc,
                "lastSeenAtUtc": row.last_seen_at_utc,
                "handshakeCount": row.handshake_count,
            }))
        })
        .collect::<AppResult<Vec<_>>>()?;
    let count: i64 = sqlx::query_scalar(
        r#"WITH input AS (
               SELECT * FROM jsonb_to_recordset($2::jsonb) AS row(
                   "userId" uuid, "participationId" integer, "peerId" uuid,
                   "endpointHash" text, "sourceAsn" bigint, "networkClass" smallint,
                   "firstSeenAtUtc" timestamptz, "lastSeenAtUtc" timestamptz,
                   "handshakeCount" integer
               )
           ), deduped_input AS MATERIALIZED (
               SELECT DISTINCT ON ("peerId", "endpointHash", "firstSeenAtUtc") *
                 FROM input
                ORDER BY "peerId", "endpointHash", "firstSeenAtUtc",
                         "lastSeenAtUtc" DESC, "handshakeCount" DESC
           ), inserted AS (
               INSERT INTO "VpnPeerNetworkObservations"
                 (game_id, user_id, participation_id, peer_id, endpoint_hash,
                  source_asn, network_class, first_seen_at_utc, last_seen_at_utc,
                  handshake_count)
               SELECT $1, input."userId", input."participationId", input."peerId",
                      decode(input."endpointHash", 'hex'), input."sourceAsn",
                      input."networkClass", input."firstSeenAtUtc",
                      input."lastSeenAtUtc", input."handshakeCount"
                 FROM deduped_input input
                 JOIN "EventVpnUserPeers" peer
                   ON peer.id = input."peerId" AND peer.game_id = $1
                  AND peer.user_id = input."userId"
                  AND peer.participation_id = input."participationId"
                  AND (peer.revoked_at_utc IS NULL
                       OR input."firstSeenAtUtc" < peer.revoked_at_utc)
                 JOIN "Games" game ON game.id = peer.game_id
                WHERE game.vpn_source_asn_telemetry_enabled = TRUE
                  AND input."firstSeenAtUtc" >= game.start_time_utc
                  AND input."lastSeenAtUtc" < game.end_time_utc
                  -- Reconciliation uses a BEFORE INSERT stamp. Reject an
                  -- exact replay before the trigger; ON CONFLICT still fences
                  -- concurrent races after the input batch is deduplicated.
                  AND NOT EXISTS (
                      SELECT 1 FROM "VpnPeerNetworkObservations" existing
                       WHERE existing.game_id = $1
                         AND existing.peer_id = input."peerId"
                         AND existing.endpoint_hash = decode(input."endpointHash", 'hex')
                         AND existing.first_seen_at_utc = input."firstSeenAtUtc"
                  )
               ON CONFLICT DO NOTHING RETURNING 1
           ) SELECT COUNT(*)::bigint FROM inserted"#,
    )
    .bind(game_id)
    .bind(sqlx::types::Json(values))
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

pub(super) async fn insert_flags(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    rows: &[FlagTransportInput],
) -> AppResult<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let values = rows
        .iter()
        .map(|row| {
            Ok(serde_json::json!({
                "challengeId": row.challenge_id,
                "receivingUserId": row.receiving_user_id,
                "receivingParticipationId": row.receiving_participation_id,
                "owningParticipationId": row.owning_participation_id,
                "peerId": row.peer_id,
                "flagValueHash": hex::encode(decode_hash(&row.flag_value_hash)?),
                "transport": row.transport,
                "direction": row.direction,
                "observedAtUtc": row.observed_at_utc,
            }))
        })
        .collect::<AppResult<Vec<_>>>()?;
    let count: i64 = sqlx::query_scalar(
        r#"WITH input AS (
               SELECT * FROM jsonb_to_recordset($2::jsonb) AS row(
                   "challengeId" integer, "receivingUserId" uuid,
                   "receivingParticipationId" integer, "owningParticipationId" integer,
                   "peerId" uuid, "flagValueHash" text, "transport" smallint,
                   "direction" smallint, "observedAtUtc" timestamptz
               )
           ), deduped_input AS MATERIALIZED (
               SELECT DISTINCT ON (
                          "challengeId", "receivingParticipationId",
                          "owningParticipationId", "flagValueHash",
                          "transport", "direction"
                      ) *
                 FROM input
                ORDER BY "challengeId", "receivingParticipationId",
                         "owningParticipationId", "flagValueHash",
                         "transport", "direction", "observedAtUtc", "peerId"
           ), inserted AS (
               INSERT INTO "VpnFlagTransportEvents"
                 (game_id, challenge_id, receiving_user_id,
                  receiving_participation_id, owning_participation_id, peer_id,
                  flag_value_hash, transport, direction, observed_at_utc)
               SELECT $1, input."challengeId", input."receivingUserId",
                      input."receivingParticipationId", input."owningParticipationId",
                      input."peerId", decode(input."flagValueHash", 'hex'),
                      input."transport", input."direction", input."observedAtUtc"
                 FROM deduped_input input
                 JOIN "EventVpnUserPeers" peer
                   ON peer.id = input."peerId" AND peer.game_id = $1
                  AND peer.user_id = input."receivingUserId"
                  AND peer.participation_id = input."receivingParticipationId"
                  AND (peer.revoked_at_utc IS NULL
                       OR input."observedAtUtc" < peer.revoked_at_utc)
                 JOIN "Games" game ON game.id = peer.game_id
                 JOIN "GameChallenges" challenge
                   ON challenge.game_id = game.id AND challenge.id = input."challengeId"
                 JOIN "Participations" owner
                   ON owner.game_id = game.id AND owner.id = input."owningParticipationId"
                WHERE game.vpn_flag_scan_enabled = TRUE
                  AND challenge."Type" NOT IN (4, 5)
                  AND (
                      challenge.flag_template IS NOT NULL
                      OR EXISTS (
                          SELECT 1 FROM "ChallengeVariants" variant
                           WHERE variant.game_id = game.id
                             AND variant.challenge_id = challenge.id
                             AND variant.participation_id = input."owningParticipationId"
                             AND variant.frozen_at_utc IS NOT NULL
                      )
                  )
                  AND input."observedAtUtc" >= game.start_time_utc
                  AND input."observedAtUtc" < game.end_time_utc
                  -- Reconciliation uses a BEFORE INSERT stamp. Reject an
                  -- exact replay before the trigger; ON CONFLICT still fences
                  -- concurrent races after the input batch is deduplicated.
                  AND NOT EXISTS (
                      SELECT 1 FROM "VpnFlagTransportEvents" existing
                       WHERE existing.game_id = $1
                         AND existing.challenge_id = input."challengeId"
                         AND existing.receiving_participation_id
                               = input."receivingParticipationId"
                         AND existing.owning_participation_id
                               = input."owningParticipationId"
                         AND existing.flag_value_hash
                               = decode(input."flagValueHash", 'hex')
                         AND existing.transport = input."transport"
                         AND existing.direction = input."direction"
                  )
               ON CONFLICT DO NOTHING RETURNING 1
           ) SELECT COUNT(*)::bigint FROM inserted"#,
    )
    .bind(game_id)
    .bind(sqlx::types::Json(values))
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}
