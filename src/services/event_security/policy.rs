use std::net::Ipv4Addr;
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const POLICY_CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_EXPIRATIONS_PER_GAME: i64 = 128;
const MAX_EXPIRATION_GAMES_PER_PASS: i64 = 64;
static POLICY_FLIGHT: LazyLock<crate::utils::single_flight::SingleFlight<Option<bytes::Bytes>>> =
    LazyLock::new(crate::utils::single_flight::SingleFlight::new);

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct EventVpnPolicy {
    pub game_id: i32,
    pub access_required: bool,
    pub behavior_telemetry_enabled: bool,
    pub flag_scan_enabled: bool,
    pub provider_dns_telemetry_enabled: bool,
    pub source_asn_telemetry_enabled: bool,
    pub device_sharing_telemetry_enabled: bool,
    pub revision: i64,
    pub start_time_utc: DateTime<Utc>,
    pub end_time_utc: DateTime<Utc>,
    pub override_active: bool,
}

impl EventVpnPolicy {
    pub fn gate_active_at(&self, now: DateTime<Utc>) -> bool {
        self.access_required
            && !self.override_active
            && now >= self.start_time_utc
            && now < self.end_time_utc
    }

    pub fn any_telemetry_enabled(&self) -> bool {
        self.behavior_telemetry_enabled
            || self.flag_scan_enabled
            || self.provider_dns_telemetry_enabled
            || self.source_asn_telemetry_enabled
            || self.device_sharing_telemetry_enabled
    }
}

fn cache_key(game_id: i32) -> String {
    format!("event-vpn-policy:{game_id}")
}

fn decode(bytes: &[u8]) -> Option<EventVpnPolicy> {
    serde_json::from_slice(bytes).ok()
}

pub async fn load_policy(st: &SharedState, game_id: i32) -> AppResult<EventVpnPolicy> {
    let key = cache_key(game_id);
    if let Some(bytes) = st.cache.get(&key).await {
        if let Some(policy) = decode(&bytes) {
            return Ok(policy);
        }
    }
    let app = st.clone();
    let fill_key = key.clone();
    let bytes = POLICY_FLIGHT
        .run(&key, move || async move {
            if let Some(bytes) = app.cache.get(&fill_key).await {
                if decode(&bytes).is_some() {
                    return Some(bytes);
                }
            }
            let policy = sqlx::query_as::<_, EventVpnPolicy>(
                r#"SELECT game.id AS game_id,
                          game.vpn_access_required AS access_required,
                          game.vpn_behavior_telemetry_enabled AS behavior_telemetry_enabled,
                          game.vpn_flag_scan_enabled AS flag_scan_enabled,
                          game.vpn_provider_dns_telemetry_enabled AS provider_dns_telemetry_enabled,
                          game.vpn_source_asn_telemetry_enabled AS source_asn_telemetry_enabled,
                          game.vpn_device_sharing_telemetry_enabled AS device_sharing_telemetry_enabled,
                          game.vpn_policy_revision AS revision,
                          game.start_time_utc, game.end_time_utc,
                          EXISTS (
                              SELECT 1 FROM "EventVpnGateOverrides" override
                               WHERE override.game_id = game.id
                                 AND override.revoked_at_utc IS NULL
                                 AND override.created_at_utc <= clock_timestamp()
                                 AND override.expires_at_utc > clock_timestamp()
                          ) AS override_active
                     FROM "Games" game
                    WHERE game.id = $1 AND game.deletion_pending = FALSE"#,
            )
            .bind(game_id)
            .fetch_optional(app.pg())
            .await
            .ok()??;
            let encoded = serde_json::to_vec(&policy).ok()?;
            app.cache
                .set(&fill_key, &encoded, Some(POLICY_CACHE_TTL))
                .await;
            Some(bytes::Bytes::from(encoded))
        })
        .await
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    decode(&bytes).ok_or_else(|| AppError::internal("invalid event VPN policy cache entry"))
}

pub async fn invalidate_policy(st: &SharedState, game_id: i32) {
    st.cache.remove(&cache_key(game_id)).await;
}

/// Advance one event's policy revision after bypasses expire naturally.
///
/// The timestamp predicate is already authoritative at the access boundary;
/// this receipt makes that transition observable and invalidates cached policy
/// without relying on an administrator opening the event page. The game row is
/// the cross-replica serialization fence, so every expired override is recorded
/// once and one bounded batch advances the revision once.
async fn reconcile_expired_game(
    pool: &sqlx::PgPool,
    game_id: i32,
) -> AppResult<Option<(usize, i64)>> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let current_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT vpn_policy_revision
             FROM "Games"
            WHERE id = $1 AND deletion_pending = FALSE
            FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some(current_revision) = current_revision else {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    };
    let expired_ids = sqlx::query_scalar::<_, uuid::Uuid>(
        r#"SELECT override.id
             FROM "EventVpnGateOverrides" override
             LEFT JOIN "EventVpnOverrideExpirations" receipt
               ON receipt.override_id = override.id
            WHERE override.game_id = $1
              AND override.revoked_at_utc IS NULL
              AND override.expires_at_utc <= clock_timestamp()
              AND receipt.override_id IS NULL
            ORDER BY override.expires_at_utc, override.id
            LIMIT $2
            FOR UPDATE OF override SKIP LOCKED"#,
    )
    .bind(game_id)
    .bind(MAX_EXPIRATIONS_PER_GAME)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if expired_ids.is_empty() {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    let next_revision = current_revision
        .checked_add(1)
        .ok_or_else(|| AppError::internal("Event VPN policy revision overflow"))?;
    let inserted = sqlx::query(
        r#"INSERT INTO "EventVpnOverrideExpirations"
                  (override_id, game_id, policy_revision)
           SELECT override.id, override.game_id, $3
             FROM "EventVpnGateOverrides" override
            WHERE override.game_id = $1
              AND override.id = ANY($2)
              AND override.revoked_at_utc IS NULL
              AND override.expires_at_utc <= clock_timestamp()
           ON CONFLICT (override_id) DO NOTHING"#,
    )
    .bind(game_id)
    .bind(&expired_ids)
    .bind(next_revision)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if inserted == 0 {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(None);
    }
    let revision_update = sqlx::query(
        r#"UPDATE "Games"
              SET vpn_policy_revision = $2
            WHERE id = $1 AND vpn_policy_revision = $3"#,
    )
    .bind(game_id)
    .bind(next_revision)
    .bind(current_revision)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if revision_update != 1 {
        return Err(AppError::internal(
            "Event VPN expiry reconciliation lost its policy fence",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(Some((inserted as usize, next_revision)))
}

/// Reconcile one administrator-observed event before returning its grant list.
pub async fn reconcile_expired_override_game(st: &SharedState, game_id: i32) -> AppResult<usize> {
    let Some((count, _revision)) = reconcile_expired_game(st.pg(), game_id).await? else {
        return Ok(0);
    };
    invalidate_policy(st, game_id).await;
    Ok(count)
}

/// Reconcile a bounded number of games whose temporary bypasses expired.
pub async fn reconcile_expired_overrides(st: &SharedState, game_limit: i64) -> AppResult<usize> {
    let game_limit = game_limit.clamp(1, MAX_EXPIRATION_GAMES_PER_PASS);
    let candidate_row_limit = game_limit.saturating_mul(MAX_EXPIRATIONS_PER_GAME);
    let game_ids = sqlx::query_scalar::<_, i32>(
        r#"SELECT DISTINCT candidate.game_id
             FROM (
                 SELECT override.game_id, override.expires_at_utc, override.id
                   FROM "EventVpnGateOverrides" override
                   LEFT JOIN "EventVpnOverrideExpirations" receipt
                     ON receipt.override_id = override.id
                  WHERE override.revoked_at_utc IS NULL
                    AND override.expires_at_utc <= clock_timestamp()
                    AND receipt.override_id IS NULL
                  ORDER BY override.expires_at_utc, override.game_id, override.id
                  LIMIT $1
             ) candidate
            ORDER BY candidate.game_id
            LIMIT $2"#,
    )
    .bind(candidate_row_limit)
    .bind(game_limit)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut reconciled = 0_usize;
    for game_id in game_ids {
        reconciled = reconciled.saturating_add(reconcile_expired_override_game(st, game_id).await?);
    }
    Ok(reconciled)
}

/// Recheck the transport boundary on a transaction which already owns the
/// caller's final roster/scope fence. The game row is locked before peer state,
/// so an organizer policy change linearizes wholly before or after this check.
///
/// `Ok(false)` means the event gate is not active. `Ok(true)` means it is active
/// and the exact source address belongs to this user/participation's live peer.
pub async fn require_event_vpn_source_on(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    user_id: uuid::Uuid,
    participation_id: i32,
    source: Option<Ipv4Addr>,
) -> AppResult<bool> {
    let gate_active = sqlx::query_scalar::<_, bool>(
        r#"SELECT vpn_access_required
                  AND start_time_utc <= clock_timestamp()
                  AND clock_timestamp() < end_time_utc
             FROM "Games"
            WHERE id = $1 AND deletion_pending = FALSE
            FOR SHARE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    if !gate_active {
        return Ok(false);
    }

    let override_active = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1 FROM "EventVpnGateOverrides" override
                WHERE override.game_id = $1
                  AND override.revoked_at_utc IS NULL
                  AND override.created_at_utc <= clock_timestamp()
                  AND override.expires_at_utc > clock_timestamp()
           )"#,
    )
    .bind(game_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if override_active {
        return Ok(false);
    }

    let source = source.ok_or(AppError::Unauthorized)?;
    let peer = sqlx::query_scalar::<_, uuid::Uuid>(
        r#"SELECT peer.id
             FROM "EventVpnUserPeers" peer
            WHERE peer.game_id = $1
              AND peer.user_id = $2
              AND peer.participation_id = $3
              AND peer.address = $4
              AND peer.revoked_at_utc IS NULL
            LIMIT 1
            FOR SHARE OF peer"#,
    )
    .bind(game_id)
    .bind(user_id)
    .bind(participation_id)
    .bind(source.to_string())
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if peer.is_none() {
        return Err(AppError::Unauthorized);
    }
    Ok(true)
}

pub fn validate_credential_key(secret: &str) -> AppResult<()> {
    if secret.len() < 32 || secret.chars().any(char::is_whitespace) {
        return Err(AppError::unavailable(
            "Event VPN requires RSCTF_EVENT_VPN_CREDENTIAL_KEY with at least 32 non-whitespace characters",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    #[test]
    fn gate_uses_a_strict_active_window_and_override() {
        let start = Utc::now();
        let mut policy = EventVpnPolicy {
            game_id: 1,
            access_required: true,
            behavior_telemetry_enabled: false,
            flag_scan_enabled: false,
            provider_dns_telemetry_enabled: false,
            source_asn_telemetry_enabled: false,
            device_sharing_telemetry_enabled: false,
            revision: 1,
            start_time_utc: start,
            end_time_utc: start + chrono::Duration::hours(1),
            override_active: false,
        };
        assert!(!policy.gate_active_at(start - chrono::Duration::milliseconds(1)));
        assert!(policy.gate_active_at(start));
        assert!(!policy.gate_active_at(policy.end_time_utc));
        policy.override_active = true;
        assert!(!policy.gate_active_at(start));
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn natural_expiry_advances_policy_once_across_replicas() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("vpn_expiry_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse database URL")
            .options([("search_path", schema.as_str())]);
        let first = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect first replica");
        let second = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect second replica");
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (
                   id INTEGER PRIMARY KEY,
                   deletion_pending BOOLEAN NOT NULL DEFAULT FALSE,
                   vpn_policy_revision BIGINT NOT NULL DEFAULT 1
               );
               CREATE TABLE "EventVpnGateOverrides" (
                   id UUID PRIMARY KEY,
                   game_id INTEGER NOT NULL REFERENCES "Games"(id),
                   expires_at_utc TIMESTAMPTZ NOT NULL,
                   revoked_at_utc TIMESTAMPTZ
               );
               CREATE TABLE "EventVpnOverrideExpirations" (
                   override_id UUID PRIMARY KEY REFERENCES "EventVpnGateOverrides"(id),
                   game_id INTEGER NOT NULL REFERENCES "Games"(id),
                   policy_revision BIGINT NOT NULL,
                   reconciled_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
               );"#,
        )
        .execute(&first)
        .await
        .expect("create expiry fixture");
        let override_id = uuid::Uuid::new_v4();
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES (7)"#)
            .execute(&first)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "EventVpnGateOverrides" (id, game_id, expires_at_utc)
               VALUES ($1, 7, clock_timestamp() - INTERVAL '1 second')"#,
        )
        .bind(override_id)
        .execute(&first)
        .await
        .unwrap();

        let (left, right) = tokio::join!(
            reconcile_expired_game(&first, 7),
            reconcile_expired_game(&second, 7)
        );
        assert_eq!(
            left.unwrap().is_some() as u8 + right.unwrap().is_some() as u8,
            1
        );
        let revision: i64 =
            sqlx::query_scalar(r#"SELECT vpn_policy_revision FROM "Games" WHERE id = 7"#)
                .fetch_one(&first)
                .await
                .unwrap();
        let receipts: i64 =
            sqlx::query_scalar(r#"SELECT COUNT(*)::bigint FROM "EventVpnOverrideExpirations""#)
                .fetch_one(&first)
                .await
                .unwrap();
        assert_eq!(revision, 2);
        assert_eq!(receipts, 1);
        assert!(reconcile_expired_game(&first, 7).await.unwrap().is_none());

        first.close().await;
        second.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
    }
}
