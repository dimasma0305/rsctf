use chrono::{DateTime, Timelike, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Acquire;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

#[path = "telemetry_rows.rs"]
mod rows;
use rows::*;

pub const EVENT_LOGICAL_QUOTA_BYTES: i64 = 256 * 1024 * 1024;
pub const GLOBAL_LOGICAL_QUOTA_BYTES: i64 = 5 * 1024 * 1024 * 1024;
pub const MAX_PATTERNS: usize = 50_000;
pub const MAX_PATTERN_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRACKED_FLOWS: usize = 65_536;
pub const MAX_INGEST_ROWS: usize = 4_096;
pub const INGEST_INTERVAL_SECONDS: u64 = 30;

/// The last slice of each quota is kept for exact foreign-flag transport
/// evidence, so bulk flow, DNS and network rows filling an event (or the
/// global budget) never crowd out the strongest signal.
const FLAG_EVENT_RESERVE_BYTES: i64 = 16 * 1024 * 1024;
const FLAG_GLOBAL_RESERVE_BYTES: i64 = 128 * 1024 * 1024;
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
    pub batch_id: Uuid,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

type TelemetryPolicy = (bool, bool, bool, bool, bool, bool);

/// Take the same outer game-row fence as finalization, then observe the
/// reconciliation state in a second READ COMMITTED statement. If this reader
/// waited behind the finalizer, that second snapshot must see the committed
/// closure; if it acquired the shared lock first, finalization waits until the
/// entire telemetry transaction commits.
async fn load_ingest_policy(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
) -> AppResult<Option<TelemetryPolicy>> {
    let flags: Option<(bool, bool, bool, bool, bool)> = sqlx::query_as(
        r#"SELECT vpn_behavior_telemetry_enabled, vpn_flag_scan_enabled,
                  vpn_provider_dns_telemetry_enabled,
                  vpn_source_asn_telemetry_enabled,
                  vpn_device_sharing_telemetry_enabled
             FROM "Games" WHERE id = $1 AND deletion_pending = FALSE
             FOR SHARE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(flags) = flags else {
        return Ok(None);
    };
    let evidence_open = sqlx::query_scalar::<_, bool>(
        r#"SELECT evidence_closed_at_utc IS NULL
             FROM "SuspicionReconciliationState" WHERE game_id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .unwrap_or(false);
    Ok(Some((
        flags.0,
        flags.1,
        flags.2,
        flags.3,
        flags.4,
        evidence_open,
    )))
}

pub async fn ingest_batch(
    st: &SharedState,
    batch: &TelemetryBatch,
) -> AppResult<TelemetryIngestResult> {
    ingest_batch_with_pool(st.pg(), batch).await
}

async fn ingest_batch_with_pool(
    pool: &sqlx::PgPool,
    batch: &TelemetryBatch,
) -> AppResult<TelemetryIngestResult> {
    batch.validate()?;
    if batch.batch_id.is_nil() {
        return Err(AppError::bad_request("Invalid telemetry batch ID"));
    }
    let fingerprint: [u8; 32] = sha2::Sha256::digest(
        serde_json::to_vec(batch)
            .map_err(|error| AppError::internal(format!("encode telemetry batch: {error}")))?,
    )
    .into();
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let claimed = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO "EventTelemetryBatches"
                (batch_id, game_id, request_fingerprint)
           VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING batch_id"#,
    )
    .bind(batch.batch_id)
    .bind(batch.game_id)
    .bind(fingerprint.as_slice())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if claimed.is_none() {
        let replay = sqlx::query_as::<_, (i32, Vec<u8>, Option<serde_json::Value>)>(
            r#"SELECT game_id, request_fingerprint, result
                 FROM "EventTelemetryBatches" WHERE batch_id = $1 FOR UPDATE"#,
        )
        .bind(batch.batch_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let Some((game_id, stored_fingerprint, result)) = replay else {
            return Err(AppError::conflict(
                "Telemetry batch is already being processed",
            ));
        };
        if game_id != batch.game_id || stored_fingerprint.as_slice() != fingerprint.as_slice() {
            return Err(AppError::conflict(
                "Telemetry batch ID was reused with different content",
            ));
        }
        let result = result
            .ok_or_else(|| AppError::unavailable("Telemetry batch result is not yet available"))?;
        let result = serde_json::from_value(result)
            .map_err(|error| AppError::internal(format!("decode telemetry replay: {error}")))?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(result);
    }
    let policy = load_ingest_policy(&mut transaction, batch.game_id).await?;
    let Some(policy) = policy else {
        return Err(AppError::not_found("Game not found"));
    };
    if !policy.5 {
        let result = TelemetryIngestResult {
            accepted_rows: 0,
            duplicate_or_invalid_rows: batch.row_count(),
            dropped_for_quota: false,
            logical_bytes: 0,
        };
        complete_batch(&mut transaction, batch.batch_id, &result).await?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(result);
    }
    if !(policy.0 || policy.1 || policy.2 || policy.3 || policy.4) {
        let result = TelemetryIngestResult {
            accepted_rows: 0,
            duplicate_or_invalid_rows: batch.row_count(),
            dropped_for_quota: false,
            logical_bytes: 0,
        };
        complete_batch(&mut transaction, batch.batch_id, &result).await?;
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(result);
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

    // Discover novelty while both usage rows are locked, but keep the inserted
    // rows in savepoints until the quota decision is made. This charges only
    // rows that actually won their immutable deduplication key. In particular,
    // an exact row replay remains successful even when an event is already at
    // its quota; rejecting it would make sensor retry behavior depend on disk
    // pressure rather than on the durable row identity. Bulk rows and flag
    // transports are decided separately (see [`quota_decision`]).
    let mut bulk_rows = transaction
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let flow_count = insert_flows(&mut bulk_rows, batch.game_id, &batch.flows).await?;
    let dns_count = insert_dns(&mut bulk_rows, batch.game_id, &batch.dns_providers).await?;
    let network_count =
        insert_networks(&mut bulk_rows, batch.game_id, &batch.peer_networks).await?;
    let bulk_count = flow_count + dns_count + network_count;
    let bulk_bytes = i64::try_from(flow_count).unwrap_or(i64::MAX) * FLOW_LOGICAL_BYTES
        + i64::try_from(dns_count).unwrap_or(i64::MAX) * DNS_LOGICAL_BYTES
        + i64::try_from(network_count).unwrap_or(i64::MAX) * NETWORK_LOGICAL_BYTES;
    let keep_bulk = quota_decision(event_bytes, global_bytes, disabled, bulk_bytes, 0).keep_bulk;
    if keep_bulk {
        bulk_rows.commit().await
    } else {
        bulk_rows.rollback().await
    }
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut flag_rows = transaction
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let flag_count = insert_flags(&mut flag_rows, batch.game_id, &batch.flag_transports).await?;
    let flag_bytes = i64::try_from(flag_count).unwrap_or(i64::MAX) * FLAG_LOGICAL_BYTES;
    let keep_flags =
        quota_decision(event_bytes, global_bytes, disabled, bulk_bytes, flag_bytes).keep_flags;
    if keep_flags {
        flag_rows.commit().await
    } else {
        flag_rows.rollback().await
    }
    .map_err(|error| AppError::internal(error.to_string()))?;

    let novel = bulk_count + flag_count;
    let (kept_rows, kept_bytes) = (
        if keep_bulk { bulk_count } else { 0 } + if keep_flags { flag_count } else { 0 },
        if keep_bulk { bulk_bytes } else { 0 } + if keep_flags { flag_bytes } else { 0 },
    );
    let (dropped_rows, dropped_bytes) = (novel - kept_rows, bulk_bytes + flag_bytes - kept_bytes);
    if !keep_bulk {
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
    }
    if dropped_rows > 0 {
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
        .bind(i64::try_from(dropped_rows).unwrap_or(i64::MAX))
        .bind(dropped_bytes)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if kept_bytes > 0 {
        sqlx::query(
            r#"UPDATE "AntiCheatTelemetryUsage"
                  SET logical_bytes = logical_bytes + $2,
                      row_count = row_count + $3,
                      updated_at_utc = clock_timestamp()
                WHERE game_id = $1"#,
        )
        .bind(batch.game_id)
        .bind(kept_bytes)
        .bind(i64::try_from(kept_rows).unwrap_or(i64::MAX))
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
        .bind(kept_bytes)
        .bind(i64::try_from(kept_rows).unwrap_or(i64::MAX))
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let result = TelemetryIngestResult {
        accepted_rows: kept_rows,
        duplicate_or_invalid_rows: batch.row_count().saturating_sub(novel),
        dropped_for_quota: dropped_rows > 0,
        logical_bytes: kept_bytes,
    };
    complete_batch(&mut transaction, batch.batch_id, &result).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuotaDecision {
    keep_bulk: bool,
    keep_flags: bool,
}

/// Bulk rows may use each quota up to its flag reserve and stop for good once
/// they overflow (the event's usage is marked disabled). Flag transports may
/// use the whole quota, reserve included, even after bulk stopped.
fn quota_decision(
    event_bytes: i64,
    global_bytes: i64,
    bulk_disabled: bool,
    bulk_bytes: i64,
    flag_bytes: i64,
) -> QuotaDecision {
    let keep_bulk = bulk_bytes == 0
        || (!bulk_disabled
            && event_bytes.saturating_add(bulk_bytes)
                <= EVENT_LOGICAL_QUOTA_BYTES - FLAG_EVENT_RESERVE_BYTES
            && global_bytes.saturating_add(bulk_bytes)
                <= GLOBAL_LOGICAL_QUOTA_BYTES - FLAG_GLOBAL_RESERVE_BYTES);
    let kept_bulk = if keep_bulk { bulk_bytes } else { 0 };
    let keep_flags = flag_bytes == 0
        || (event_bytes
            .saturating_add(kept_bulk)
            .saturating_add(flag_bytes)
            <= EVENT_LOGICAL_QUOTA_BYTES
            && global_bytes
                .saturating_add(kept_bulk)
                .saturating_add(flag_bytes)
                <= GLOBAL_LOGICAL_QUOTA_BYTES);
    QuotaDecision {
        keep_bulk,
        keep_flags,
    }
}

async fn complete_batch(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    batch_id: Uuid,
    result: &TelemetryIngestResult,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "EventTelemetryBatches"
              SET result = $2, completed_at_utc = clock_timestamp()
            WHERE batch_id = $1"#,
    )
    .bind(batch_id)
    .bind(
        serde_json::to_value(result).map_err(|error| {
            AppError::internal(format!("encode telemetry ingest result: {error}"))
        })?,
    )
    .execute(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Delete a game's telemetry usage row and return its share of the global
/// budget in the same statement. Every path that removes the row must use
/// this, or the global total keeps counting deleted events until telemetry
/// switches off for every future event. Lock order matches ingest: the game's
/// usage row, then the global row.
pub(crate) async fn release_game_usage(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<()> {
    sqlx::query(
        r#"WITH released AS (
               DELETE FROM "AntiCheatTelemetryUsage" WHERE game_id = $1
               RETURNING logical_bytes, row_count
           )
           UPDATE "AntiCheatTelemetryGlobalUsage" global
              SET logical_bytes = GREATEST(0, global.logical_bytes - released.logical_bytes),
                  row_count = GREATEST(0, global.row_count - released.row_count),
                  updated_at_utc = clock_timestamp()
             FROM released
            WHERE global.id = 1"#,
    )
    .bind(game_id)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
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
    release_game_usage(&mut transaction, game_id).await?;
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
#[path = "telemetry_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "telemetry_pg_tests.rs"]
mod pg_tests;
