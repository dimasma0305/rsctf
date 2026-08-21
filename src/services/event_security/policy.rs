use std::net::Ipv4Addr;
use std::sync::LazyLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const POLICY_CACHE_TTL: Duration = Duration::from_secs(2);
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
}
