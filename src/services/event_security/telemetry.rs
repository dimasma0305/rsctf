use chrono::{DateTime, Timelike, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

pub const EVENT_LOGICAL_QUOTA_BYTES: i64 = 256 * 1024 * 1024;
pub const GLOBAL_LOGICAL_QUOTA_BYTES: i64 = 5 * 1024 * 1024 * 1024;
pub const MAX_PATTERNS: usize = 50_000;
pub const MAX_PATTERN_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRACKED_FLOWS: usize = 65_536;
pub const MAX_INGEST_ROWS: usize = 4_096;
pub const INGEST_INTERVAL_SECONDS: u64 = 30;

const FLOW_LOGICAL_BYTES: i64 = 192;
const DNS_LOGICAL_BYTES: i64 = 144;
const NETWORK_LOGICAL_BYTES: i64 = 176;
const FLAG_LOGICAL_BYTES: i64 = 176;

pub fn flag_value_hash(
    secret: &str,
    game_id: i32,
    challenge_id: i32,
    flag: &str,
) -> AppResult<[u8; 32]> {
    super::validate_credential_key(secret)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| AppError::internal("initialize VPN flag-value hash"))?;
    mac.update(b"rsctf:vpn-flag-value:v1\0");
    mac.update(&game_id.to_be_bytes());
    mac.update(&challenge_id.to_be_bytes());
    mac.update(flag.as_bytes());
    Ok(mac.finalize().into_bytes().into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowBucketInput {
    pub user_id: Uuid,
    pub participation_id: i32,
    pub peer_id: Uuid,
    pub challenge_id: Option<i32>,
    pub container_generation: Option<i32>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub bucket_start_utc: DateTime<Utc>,
    pub packets_up: i64,
    pub packets_down: i64,
    pub bytes_up: i64,
    pub bytes_down: i64,
    pub distinct_destinations: i32,
    pub connection_count: i32,
    pub active_seconds: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsProviderBucketInput {
    pub user_id: Uuid,
    pub participation_id: i32,
    pub peer_id: Uuid,
    pub provider_category: i16,
    #[serde(with = "crate::utils::datetime::millis")]
    pub bucket_start_utc: DateTime<Utc>,
    pub query_count: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub first_seen_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub last_seen_at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerNetworkInput {
    pub user_id: Uuid,
    pub participation_id: i32,
    pub peer_id: Uuid,
    /// Domain-separated SHA-256/HMAC of the endpoint; never its raw address.
    pub endpoint_hash: String,
    pub source_asn: Option<i64>,
    pub network_class: i16,
    #[serde(with = "crate::utils::datetime::millis")]
    pub first_seen_at_utc: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub last_seen_at_utc: DateTime<Utc>,
    pub handshake_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlagTransportInput {
    pub challenge_id: i32,
    pub receiving_user_id: Uuid,
    pub receiving_participation_id: i32,
    pub owning_participation_id: i32,
    pub peer_id: Uuid,
    /// Domain-separated HMAC of an exact platform-issued dynamic flag.
    pub flag_value_hash: String,
    pub transport: i16,
    pub direction: i16,
    #[serde(with = "crate::utils::datetime::millis")]
    pub observed_at_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryBatch {
    pub game_id: i32,
    #[serde(default)]
    pub flows: Vec<FlowBucketInput>,
    #[serde(default)]
    pub dns_providers: Vec<DnsProviderBucketInput>,
    #[serde(default)]
    pub peer_networks: Vec<PeerNetworkInput>,
    #[serde(default)]
    pub flag_transports: Vec<FlagTransportInput>,
    /// Aggregate sensor-side shedding since the previous successful enqueue.
    /// These counters contain no packet or identity data.
    #[serde(default)]
    pub sensor_dropped_rows: i64,
    #[serde(default)]
    pub sensor_dropped_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryIngestResult {
    pub accepted_rows: usize,
    pub duplicate_or_invalid_rows: usize,
    pub dropped_for_quota: bool,
    pub logical_bytes: i64,
}

impl TelemetryBatch {
    fn row_count(&self) -> usize {
        self.flows.len()
            + self.dns_providers.len()
            + self.peer_networks.len()
            + self.flag_transports.len()
    }

    fn estimated_bytes(&self) -> AppResult<i64> {
        if self.row_count() > MAX_INGEST_ROWS {
            return Err(AppError::bad_request(format!(
                "A telemetry batch may contain at most {MAX_INGEST_ROWS} rows"
            )));
        }
        let bytes = i64::try_from(self.flows.len()).unwrap_or(i64::MAX) * FLOW_LOGICAL_BYTES
            + i64::try_from(self.dns_providers.len()).unwrap_or(i64::MAX) * DNS_LOGICAL_BYTES
            + i64::try_from(self.peer_networks.len()).unwrap_or(i64::MAX) * NETWORK_LOGICAL_BYTES
            + i64::try_from(self.flag_transports.len()).unwrap_or(i64::MAX) * FLAG_LOGICAL_BYTES;
        Ok(bytes)
    }

    fn validate(&self) -> AppResult<()> {
        self.estimated_bytes()?;
        if !(0..=100_000_000).contains(&self.sensor_dropped_rows)
            || !(0..=i64::from(u32::MAX)).contains(&self.sensor_dropped_bytes)
        {
            return Err(AppError::bad_request("Invalid sensor drop counters"));
        }
        for row in &self.flows {
            if row.bucket_start_utc.second() != 0
                || row.bucket_start_utc.nanosecond() != 0
                || row.bucket_start_utc.minute() % 5 != 0
                || row.packets_up < 0
                || row.packets_down < 0
                || row.bytes_up < 0
                || row.bytes_down < 0
                || row.distinct_destinations < 0
                || row.connection_count < 0
                || !(0..=300).contains(&row.active_seconds)
            {
                return Err(AppError::bad_request("Invalid five-minute flow bucket"));
            }
        }
        for row in &self.dns_providers {
            if row.bucket_start_utc.second() != 0
                || row.bucket_start_utc.nanosecond() != 0
                || row.bucket_start_utc.minute() % 15 != 0
                || !(0..=31).contains(&row.provider_category)
                || row.query_count < 0
                || row.first_seen_at_utc < row.bucket_start_utc
                || row.last_seen_at_utc < row.first_seen_at_utc
                || row.last_seen_at_utc >= row.bucket_start_utc + chrono::Duration::minutes(15)
            {
                return Err(AppError::bad_request(
                    "Invalid 15-minute DNS provider bucket",
                ));
            }
        }
        for row in &self.peer_networks {
            decode_hash(&row.endpoint_hash)?;
            if row
                .source_asn
                .is_some_and(|value| !(0..=4_294_967_295).contains(&value))
                || !(0..=7).contains(&row.network_class)
                || row.last_seen_at_utc < row.first_seen_at_utc
                || row.handshake_count < 1
            {
                return Err(AppError::bad_request("Invalid peer network observation"));
            }
        }
        for row in &self.flag_transports {
            decode_hash(&row.flag_value_hash)?;
            if row.receiving_participation_id == row.owning_participation_id
                || !(0..=15).contains(&row.transport)
                || !(0..=1).contains(&row.direction)
            {
                return Err(AppError::bad_request("Invalid exact-flag transport event"));
            }
        }
        Ok(())
    }
}

fn decode_hash(value: &str) -> AppResult<Vec<u8>> {
    let bytes =
        hex::decode(value).map_err(|_| AppError::bad_request("Expected 32-byte hex hash"))?;
    if bytes.len() != 32 {
        return Err(AppError::bad_request("Expected 32-byte hex hash"));
    }
    Ok(bytes)
}

async fn lock_usage(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
) -> AppResult<(i64, bool, i64)> {
    sqlx::query(
        r#"INSERT INTO "AntiCheatTelemetryUsage" (game_id)
           VALUES ($1) ON CONFLICT DO NOTHING"#,
    )
    .bind(game_id)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"INSERT INTO "AntiCheatTelemetryGlobalUsage" (id)
           VALUES (1) ON CONFLICT DO NOTHING"#,
    )
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let event: (i64, bool) = sqlx::query_as(
        r#"SELECT logical_bytes, disabled_at_utc IS NOT NULL
             FROM "AntiCheatTelemetryUsage" WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let global: i64 = sqlx::query_scalar(
        r#"SELECT logical_bytes FROM "AntiCheatTelemetryGlobalUsage"
            WHERE id = 1 FOR UPDATE"#,
    )
    .fetch_one(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((event.0, event.1, global))
}

async fn insert_flows(
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
                  AND peer.revoked_at_utc IS NULL
                 JOIN "Games" game ON game.id = peer.game_id
                WHERE game.vpn_behavior_telemetry_enabled = TRUE
                  AND input."bucketStartUtc" >= game.start_time_utc
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
fn flow_database_rows(rows: &[FlowBucketInput]) -> Vec<serde_json::Value> {
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

async fn insert_dns(
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
           ), inserted AS (
               INSERT INTO "VpnDnsProviderBuckets"
                 (game_id, user_id, participation_id, peer_id, provider_category,
                  bucket_start_utc, query_count, first_seen_at_utc, last_seen_at_utc)
               SELECT $1, input."userId", input."participationId", input."peerId",
                      input."providerCategory", input."bucketStartUtc", input."queryCount",
                      input."firstSeenAtUtc", input."lastSeenAtUtc"
                 FROM input
                 JOIN "EventVpnUserPeers" peer
                   ON peer.id = input."peerId" AND peer.game_id = $1
                  AND peer.user_id = input."userId"
                  AND peer.participation_id = input."participationId"
                  AND peer.revoked_at_utc IS NULL
                 JOIN "Games" game ON game.id = peer.game_id
                WHERE game.vpn_provider_dns_telemetry_enabled = TRUE
                  AND input."firstSeenAtUtc" >= game.start_time_utc
                  AND input."lastSeenAtUtc" < game.end_time_utc
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

fn dns_database_rows(rows: &[DnsProviderBucketInput]) -> Vec<serde_json::Value> {
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

async fn insert_networks(
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
           ), inserted AS (
               INSERT INTO "VpnPeerNetworkObservations"
                 (game_id, user_id, participation_id, peer_id, endpoint_hash,
                  source_asn, network_class, first_seen_at_utc, last_seen_at_utc,
                  handshake_count)
               SELECT $1, input."userId", input."participationId", input."peerId",
                      decode(input."endpointHash", 'hex'), input."sourceAsn",
                      input."networkClass", input."firstSeenAtUtc",
                      input."lastSeenAtUtc", input."handshakeCount"
                 FROM input
                 JOIN "EventVpnUserPeers" peer
                   ON peer.id = input."peerId" AND peer.game_id = $1
                  AND peer.user_id = input."userId"
                  AND peer.participation_id = input."participationId"
                  AND peer.revoked_at_utc IS NULL
                 JOIN "Games" game ON game.id = peer.game_id
                WHERE game.vpn_source_asn_telemetry_enabled = TRUE
                  AND input."firstSeenAtUtc" >= game.start_time_utc
                  AND input."lastSeenAtUtc" < game.end_time_utc
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

async fn insert_flags(
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
           ), inserted AS (
               INSERT INTO "VpnFlagTransportEvents"
                 (game_id, challenge_id, receiving_user_id,
                  receiving_participation_id, owning_participation_id, peer_id,
                  flag_value_hash, transport, direction, observed_at_utc)
               SELECT $1, input."challengeId", input."receivingUserId",
                      input."receivingParticipationId", input."owningParticipationId",
                      input."peerId", decode(input."flagValueHash", 'hex'),
                      input."transport", input."direction", input."observedAtUtc"
                 FROM input
                 JOIN "EventVpnUserPeers" peer
                   ON peer.id = input."peerId" AND peer.game_id = $1
                  AND peer.user_id = input."receivingUserId"
                  AND peer.participation_id = input."receivingParticipationId"
                  AND peer.revoked_at_utc IS NULL
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

pub async fn ingest_batch(
    st: &SharedState,
    batch: &TelemetryBatch,
) -> AppResult<TelemetryIngestResult> {
    batch.validate()?;
    let estimated = batch.estimated_bytes()?;
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let policy: Option<(bool, bool, bool, bool, bool)> = sqlx::query_as(
        r#"SELECT vpn_behavior_telemetry_enabled, vpn_flag_scan_enabled,
                  vpn_provider_dns_telemetry_enabled,
                  vpn_source_asn_telemetry_enabled,
                  vpn_device_sharing_telemetry_enabled
             FROM "Games" WHERE id = $1 AND deletion_pending = FALSE FOR SHARE"#,
    )
    .bind(batch.game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(policy) = policy else {
        return Err(AppError::not_found("Game not found"));
    };
    if !(policy.0 || policy.1 || policy.2 || policy.3 || policy.4) {
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(TelemetryIngestResult {
            accepted_rows: 0,
            duplicate_or_invalid_rows: batch.row_count(),
            dropped_for_quota: false,
            logical_bytes: 0,
        });
    }
    if batch.sensor_dropped_rows > 0 {
        sqlx::query(
            r#"INSERT INTO "AntiCheatTelemetryDrops"
                 (game_id, source, reason, dropped_rows, dropped_bytes, bucket_start_utc)
               VALUES ($1, 1, 1, $2, $3, date_trunc('hour', clock_timestamp()))
               ON CONFLICT ((COALESCE(game_id, -1)), source, reason, bucket_start_utc)
               DO UPDATE SET
                   dropped_rows = "AntiCheatTelemetryDrops".dropped_rows + EXCLUDED.dropped_rows,
                   dropped_bytes = "AntiCheatTelemetryDrops".dropped_bytes + EXCLUDED.dropped_bytes,
                   observed_at_utc = clock_timestamp()"#,
        )
        .bind(batch.game_id)
        .bind(batch.sensor_dropped_rows)
        .bind(batch.sensor_dropped_bytes)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let (event_bytes, disabled, global_bytes) = lock_usage(&mut transaction, batch.game_id).await?;
    if disabled
        || event_bytes.saturating_add(estimated) > EVENT_LOGICAL_QUOTA_BYTES
        || global_bytes.saturating_add(estimated) > GLOBAL_LOGICAL_QUOTA_BYTES
    {
        sqlx::query(
            r#"UPDATE "AntiCheatTelemetryUsage"
                  SET disabled_at_utc = COALESCE(disabled_at_utc, clock_timestamp()),
                      updated_at_utc = clock_timestamp()
                WHERE game_id = $1"#,
        )
        .bind(batch.game_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        sqlx::query(
            r#"INSERT INTO "AntiCheatTelemetryDrops"
                 (game_id, source, reason, dropped_rows, dropped_bytes, bucket_start_utc)
               VALUES ($1, 0, 0, $2, $3, date_trunc('hour', clock_timestamp()))
               ON CONFLICT ((COALESCE(game_id, -1)), source, reason, bucket_start_utc)
               DO UPDATE SET
                   dropped_rows = "AntiCheatTelemetryDrops".dropped_rows + EXCLUDED.dropped_rows,
                   dropped_bytes = "AntiCheatTelemetryDrops".dropped_bytes + EXCLUDED.dropped_bytes,
                   observed_at_utc = clock_timestamp()"#,
        )
        .bind(batch.game_id)
        .bind(i64::try_from(batch.row_count()).unwrap_or(i64::MAX))
        .bind(estimated)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(TelemetryIngestResult {
            accepted_rows: 0,
            duplicate_or_invalid_rows: 0,
            dropped_for_quota: true,
            logical_bytes: 0,
        });
    }

    let flow_count = insert_flows(&mut transaction, batch.game_id, &batch.flows).await?;
    let dns_count = insert_dns(&mut transaction, batch.game_id, &batch.dns_providers).await?;
    let network_count =
        insert_networks(&mut transaction, batch.game_id, &batch.peer_networks).await?;
    let flag_count = insert_flags(&mut transaction, batch.game_id, &batch.flag_transports).await?;
    let accepted = flow_count + dns_count + network_count + flag_count;
    let actual = i64::try_from(flow_count).unwrap_or(i64::MAX) * FLOW_LOGICAL_BYTES
        + i64::try_from(dns_count).unwrap_or(i64::MAX) * DNS_LOGICAL_BYTES
        + i64::try_from(network_count).unwrap_or(i64::MAX) * NETWORK_LOGICAL_BYTES
        + i64::try_from(flag_count).unwrap_or(i64::MAX) * FLAG_LOGICAL_BYTES;
    if actual > 0 {
        sqlx::query(
            r#"UPDATE "AntiCheatTelemetryUsage"
                  SET logical_bytes = logical_bytes + $2,
                      row_count = row_count + $3,
                      updated_at_utc = clock_timestamp()
                WHERE game_id = $1"#,
        )
        .bind(batch.game_id)
        .bind(actual)
        .bind(i64::try_from(accepted).unwrap_or(i64::MAX))
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        sqlx::query(
            r#"UPDATE "AntiCheatTelemetryGlobalUsage"
                  SET logical_bytes = logical_bytes + $1,
                      row_count = row_count + $2,
                      updated_at_utc = clock_timestamp()
                WHERE id = 1"#,
        )
        .bind(actual)
        .bind(i64::try_from(accepted).unwrap_or(i64::MAX))
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(TelemetryIngestResult {
        accepted_rows: accepted,
        duplicate_or_invalid_rows: batch.row_count().saturating_sub(accepted),
        dropped_for_quota: false,
        logical_bytes: actual,
    })
}

pub async fn purge_game_telemetry(
    st: &SharedState,
    game_id: i32,
    actor: Uuid,
    reason: &str,
) -> AppResult<(i64, i64)> {
    let reason = reason.trim();
    if !(8..=512).contains(&reason.len()) {
        return Err(AppError::bad_request(
            "Purge reason must contain 8 to 512 characters",
        ));
    }
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let (logical_bytes, row_count): (i64, i64) = sqlx::query_as(
        r#"SELECT logical_bytes, row_count FROM "AntiCheatTelemetryUsage"
            WHERE game_id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or((0, 0));
    for statement in [
        r#"DELETE FROM "VpnFlagTransportEvents" WHERE game_id = $1"#,
        r#"DELETE FROM "VpnPeerNetworkObservations" WHERE game_id = $1"#,
        r#"DELETE FROM "VpnDnsProviderBuckets" WHERE game_id = $1"#,
        r#"DELETE FROM "VpnFlowTelemetryBuckets" WHERE game_id = $1"#,
    ] {
        sqlx::query(statement)
            .bind(game_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let drop_rows = sqlx::query(r#"DELETE FROM "AntiCheatTelemetryDrops" WHERE game_id = $1"#)
        .bind(game_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .rows_affected();
    let rows_removed = row_count.saturating_add(i64::try_from(drop_rows).unwrap_or(i64::MAX));
    sqlx::query(r#"DELETE FROM "AntiCheatTelemetryUsage" WHERE game_id = $1"#)
        .bind(game_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "AntiCheatTelemetryGlobalUsage"
              SET logical_bytes = GREATEST(0, logical_bytes - $1),
                  row_count = GREATEST(0, row_count - $2),
                  updated_at_utc = clock_timestamp()
            WHERE id = 1"#,
    )
    .bind(logical_bytes)
    .bind(row_count)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"INSERT INTO "AntiCheatTelemetryPurges"
             (game_id, requested_by_user_id, reason, rows_removed, logical_bytes_removed)
           VALUES ($1, $2, $3, $4, $5)"#,
    )
    .bind(game_id)
    .bind(actor)
    .bind(reason)
    .bind(rows_removed)
    .bind(logical_bytes)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok((rows_removed, logical_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_deliberately_small_and_gameplay_independent() {
        assert_eq!(EVENT_LOGICAL_QUOTA_BYTES, 256 * 1024 * 1024);
        assert_eq!(GLOBAL_LOGICAL_QUOTA_BYTES, 5 * 1024 * 1024 * 1024);
        assert_eq!(MAX_PATTERNS, 50_000);
        assert_eq!(MAX_PATTERN_BYTES, 4 * 1024 * 1024);
        assert_eq!(MAX_TRACKED_FLOWS, 65_536);
        assert_eq!(MAX_INGEST_ROWS, 4_096);
        assert_eq!(INGEST_INTERVAL_SECONDS, 30);
    }

    #[test]
    fn invalid_bucket_and_raw_values_are_rejected_before_database_work() {
        let batch = TelemetryBatch {
            game_id: 1,
            flows: vec![FlowBucketInput {
                user_id: Uuid::nil(),
                participation_id: 1,
                peer_id: Uuid::nil(),
                challenge_id: None,
                container_generation: None,
                bucket_start_utc: Utc::now(),
                packets_up: 0,
                packets_down: 0,
                bytes_up: 0,
                bytes_down: 0,
                distinct_destinations: 0,
                connection_count: 0,
                active_seconds: 0,
            }],
            dns_providers: Vec::new(),
            peer_networks: Vec::new(),
            flag_transports: Vec::new(),
            sensor_dropped_rows: 0,
            sensor_dropped_bytes: 0,
        };
        assert!(batch.validate().is_err());
        assert!(decode_hash("not-an-address-or-hash").is_err());
    }

    #[test]
    fn internal_bulk_timestamps_are_rfc3339_not_wire_milliseconds() {
        let timestamp = DateTime::parse_from_rfc3339("2026-08-20T13:40:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let flow = FlowBucketInput {
            user_id: Uuid::nil(),
            participation_id: 1,
            peer_id: Uuid::nil(),
            challenge_id: None,
            container_generation: None,
            bucket_start_utc: timestamp,
            packets_up: 1,
            packets_down: 1,
            bytes_up: 1,
            bytes_down: 1,
            distinct_destinations: 1,
            connection_count: 1,
            active_seconds: 1,
        };
        let dns = DnsProviderBucketInput {
            user_id: Uuid::nil(),
            participation_id: 1,
            peer_id: Uuid::nil(),
            provider_category: 1,
            bucket_start_utc: timestamp,
            query_count: 1,
            first_seen_at_utc: timestamp,
            last_seen_at_utc: timestamp,
        };

        assert_eq!(
            flow_database_rows(&[flow])[0]["bucketStartUtc"],
            "2026-08-20T13:40:00Z"
        );
        assert_eq!(
            dns_database_rows(&[dns])[0]["firstSeenAtUtc"],
            "2026-08-20T13:40:00Z"
        );
    }
}
