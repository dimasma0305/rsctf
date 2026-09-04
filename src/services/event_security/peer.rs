use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::str::FromStr;

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use chrono::{DateTime, Utc};
use defguard_wireguard_rs::key::Key;
use ipnet::Ipv4Net;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::models::data::participation;
use crate::utils::enums::{ParticipationStatus, Role};
use crate::utils::error::{AppError, AppResult};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EventVpnUserPeer {
    pub id: Uuid,
    pub game_id: i32,
    pub user_id: Uuid,
    pub participation_id: i32,
    pub public_key: String,
    pub private_key_ciphertext: Vec<u8>,
    pub private_key_nonce: Vec<u8>,
    pub address: String,
    pub generation: i32,
    pub issued_at_utc: DateTime<Utc>,
    pub last_config_download_at_utc: Option<DateTime<Utc>>,
    pub revoked_at_utc: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ProvisionedEventVpnPeer {
    pub row: EventVpnUserPeer,
    pub private_key: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VerifiedPeerSource {
    pub peer_id: Uuid,
    pub user_id: Uuid,
    pub participation_id: i32,
    pub public_key: String,
    pub generation: i32,
    pub policy_revision: i64,
    pub security_stamp: String,
}

fn encryption_key(secret: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"rsctf:event-vpn:credential:v1\0");
    digest.update(secret.as_bytes());
    digest.finalize().into()
}

fn aad(game_id: i32, user_id: Uuid, participation_id: i32, generation: i32) -> Vec<u8> {
    format!("v1:{game_id}:{user_id}:{participation_id}:{generation}").into_bytes()
}

fn encrypt_private_key(
    secret: &str,
    game_id: i32,
    user_id: Uuid,
    participation_id: i32,
    generation: i32,
    private_key: &str,
) -> AppResult<(Vec<u8>, [u8; 12])> {
    super::validate_credential_key(secret)?;
    let cipher = Aes256Gcm::new_from_slice(&encryption_key(secret))
        .map_err(|_| AppError::internal("initialize event VPN credential encryption"))?;
    let nonce: [u8; 12] = rand::random();
    let nonce_value: Nonce<U12> = nonce.into();
    let ciphertext = cipher
        .encrypt(
            &nonce_value,
            Payload {
                msg: private_key.as_bytes(),
                aad: &aad(game_id, user_id, participation_id, generation),
            },
        )
        .map_err(|_| AppError::internal("encrypt event VPN credential"))?;
    Ok((ciphertext, nonce))
}

fn decrypt_private_key(secret: &str, peer: &EventVpnUserPeer) -> AppResult<String> {
    super::validate_credential_key(secret)?;
    if peer.private_key_nonce.len() != 12 {
        return Err(AppError::internal("invalid event VPN credential nonce"));
    }
    let cipher = Aes256Gcm::new_from_slice(&encryption_key(secret))
        .map_err(|_| AppError::internal("initialize event VPN credential encryption"))?;
    let nonce_value = Nonce::<U12>::try_from(peer.private_key_nonce.as_slice())
        .map_err(|_| AppError::internal("invalid event VPN credential nonce"))?;
    let plaintext = cipher
        .decrypt(
            &nonce_value,
            Payload {
                msg: &peer.private_key_ciphertext,
                aad: &aad(
                    peer.game_id,
                    peer.user_id,
                    peer.participation_id,
                    peer.generation,
                ),
            },
        )
        .map_err(|_| AppError::unavailable("event VPN credential cannot be decrypted"))?;
    String::from_utf8(plaintext)
        .map_err(|_| AppError::internal("event VPN credential is not valid UTF-8"))
}

fn allocate_address(
    cidr: &str,
    game_id: i32,
    user_id: Uuid,
    used: &HashSet<Ipv4Addr>,
) -> AppResult<String> {
    let network = cidr
        .parse::<Ipv4Net>()
        .map_err(|_| AppError::internal("RSCTF_AD_VPN_CLIENT_CIDR must be an IPv4 CIDR"))?
        .trunc();
    if network.prefix_len() > 30 {
        return Err(AppError::internal(
            "RSCTF_AD_VPN_CLIENT_CIDR does not have usable peer addresses",
        ));
    }
    let host_count = 1u64 << (32 - network.prefix_len());
    let usable = host_count.saturating_sub(3);
    let mut digest = Sha256::new();
    digest.update(game_id.to_be_bytes());
    digest.update(user_id.as_bytes());
    let bytes: [u8; 32] = digest.finalize().into();
    let start = u64::from_be_bytes(bytes[..8].try_into().expect("eight-byte prefix")) % usable;
    for offset in 0..usable {
        let host = 2 + ((start + offset) % usable);
        let address = Ipv4Addr::from(
            u32::from(network.network())
                .checked_add(host as u32)
                .ok_or_else(|| AppError::internal("event VPN address overflow"))?,
        );
        if address != network.broadcast() && !used.contains(&address) {
            return Ok(address.to_string());
        }
    }
    Err(AppError::unavailable(
        "The event VPN address pool is exhausted; enlarge RSCTF_AD_VPN_CLIENT_CIDR",
    ))
}

async fn load_live_peer<'e, E>(
    executor: E,
    game_id: i32,
    user_id: Uuid,
) -> AppResult<Option<EventVpnUserPeer>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, EventVpnUserPeer>(
        r#"SELECT id, game_id, user_id, participation_id, public_key,
                  private_key_ciphertext, private_key_nonce, address, generation,
                  issued_at_utc, last_config_download_at_utc, revoked_at_utc
             FROM "EventVpnUserPeers"
            WHERE game_id = $1 AND user_id = $2 AND revoked_at_utc IS NULL"#,
    )
    .bind(game_id)
    .bind(user_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

async fn load_reserved_addresses<'e, E>(executor: E) -> AppResult<HashSet<Ipv4Addr>>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    // Event VPN peer rows are immutable audit history and their address column
    // has a global unique index. A revoked row therefore still reserves its
    // address even though it is no longer installed in WireGuard. Keep those
    // historical addresses in the allocator set or a later event can select
    // one and fail its config download with a unique-constraint violation.
    sqlx::query_scalar::<_, String>(
        r#"SELECT address FROM "AdVpnPeers"
           UNION ALL
           SELECT address FROM "EventVpnUserPeers""#,
    )
    .fetch_all(executor)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
    .map(|addresses| {
        addresses
            .into_iter()
            .filter_map(|address| Ipv4Addr::from_str(&address).ok())
            .collect()
    })
}

pub async fn ensure_user_peer(
    st: &SharedState,
    user: &CurrentUser,
    part: &participation::Model,
) -> AppResult<ProvisionedEventVpnPeer> {
    if !crate::services::ad_vpn::enabled() {
        return Err(AppError::unavailable(
            "Event VPN access requires RSCTF_AD_VPN_ENABLED=true",
        ));
    }
    super::validate_credential_key(&st.config.event_vpn_credential_key)?;
    let mut roster = crate::services::live_roster::try_acquire_participation_fence(
        st.pg(),
        user.id,
        &user.security_stamp,
        part.game_id,
        part.team_id,
        part.id,
        true,
    )
    .await?
    .ok_or(AppError::Forbidden)?;
    let tx = roster.transaction_mut();
    crate::utils::single_flight::acquire_transaction_advisory_lock(tx, "ad-vpn-peer-allocation")
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let allowed: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "Games"
                WHERE id = $1
                  AND deletion_pending = FALSE
                  AND vpn_access_required = TRUE
                  AND clock_timestamp() < end_time_utc
           )"#,
    )
    .bind(part.game_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !allowed {
        return Err(AppError::bad_request(
            "Event VPN configuration is unavailable after the event has ended",
        ));
    }

    if let Some(peer) = load_live_peer(&mut **tx, part.game_id, user.id).await? {
        if peer.participation_id != part.id {
            return Err(AppError::conflict(
                "The active event VPN credential belongs to a different participation",
            ));
        }
        let private_key = decrypt_private_key(&st.config.event_vpn_credential_key, &peer)?;
        sqlx::query(
            r#"UPDATE "EventVpnUserPeers"
                  SET last_config_download_at_utc = clock_timestamp()
                WHERE id = $1 AND revoked_at_utc IS NULL"#,
        )
        .bind(peer.id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        roster.release().await?;
        return Ok(ProvisionedEventVpnPeer {
            row: peer,
            private_key,
        });
    }

    let used = load_reserved_addresses(&mut **tx).await?;
    let address = allocate_address(
        &crate::services::ad_vpn::client_cidr(),
        part.game_id,
        user.id,
        &used,
    )?;
    let generation: i32 = sqlx::query_scalar(
        r#"SELECT COALESCE(MAX(generation), 0) + 1
             FROM "EventVpnUserPeers" WHERE game_id = $1 AND user_id = $2"#,
    )
    .bind(part.game_id)
    .bind(user.id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let key = Key::generate();
    let private_key = key.to_string();
    let (ciphertext, nonce) = encrypt_private_key(
        &st.config.event_vpn_credential_key,
        part.game_id,
        user.id,
        part.id,
        generation,
        &private_key,
    )?;
    let peer_id = Uuid::now_v7();
    let peer = sqlx::query_as::<_, EventVpnUserPeer>(
        r#"INSERT INTO "EventVpnUserPeers"
             (id, game_id, user_id, participation_id, public_key,
              private_key_ciphertext, private_key_nonce, address, generation,
              last_config_download_at_utc)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, clock_timestamp())
           RETURNING id, game_id, user_id, participation_id, public_key,
                     private_key_ciphertext, private_key_nonce, address, generation,
                     issued_at_utc, last_config_download_at_utc, revoked_at_utc"#,
    )
    .bind(peer_id)
    .bind(part.game_id)
    .bind(user.id)
    .bind(part.id)
    .bind(key.public_key().to_string())
    .bind(ciphertext)
    .bind(nonce.as_slice())
    .bind(address)
    .bind(generation)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    roster.release().await?;
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    Ok(ProvisionedEventVpnPeer {
        row: peer,
        private_key,
    })
}

pub async fn verified_peer_source(
    pool: &sqlx::PgPool,
    game_id: i32,
    source: Ipv4Addr,
) -> AppResult<Option<VerifiedPeerSource>> {
    sqlx::query_as::<_, VerifiedPeerSource>(
        r#"SELECT peer.id AS peer_id, peer.user_id, peer.participation_id,
                  peer.public_key,
                  peer.generation, game.vpn_policy_revision AS policy_revision,
                  account.security_stamp
             FROM "EventVpnUserPeers" peer
             JOIN "Games" game ON game.id = peer.game_id
             JOIN "Participations" participation
               ON participation.game_id = peer.game_id
              AND participation.id = peer.participation_id
             JOIN "UserParticipations" historical
               ON historical.user_id = peer.user_id
              AND historical.game_id = peer.game_id
              AND historical.participation_id = peer.participation_id
              AND historical.team_id = participation.team_id
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "AspNetUsers" account ON account.id = peer.user_id
            WHERE peer.game_id = $1
              AND peer.address = $2
              AND peer.revoked_at_utc IS NULL
              AND game.deletion_pending = FALSE
              AND game.vpn_access_required = TRUE
              AND game.start_time_utc <= clock_timestamp()
              AND clock_timestamp() < game.end_time_utc
              AND participation.status = $3
              AND team.deletion_pending = FALSE
              AND account.email_confirmed = TRUE
              AND account.role <> $4
              AND account.security_stamp IS NOT NULL
              AND (
                    team.captain_id = peer.user_id
                    OR EXISTS (
                        SELECT 1 FROM "TeamMembers" member
                         WHERE member.team_id = team.id AND member.user_id = peer.user_id
                    )
              )"#,
    )
    .bind(game_id)
    .bind(source.to_string())
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub async fn verified_user_peer(
    pool: &sqlx::PgPool,
    game_id: i32,
    user_id: Uuid,
    participation_id: i32,
) -> AppResult<Option<VerifiedPeerSource>> {
    sqlx::query_as::<_, VerifiedPeerSource>(
        r#"SELECT peer.id AS peer_id, peer.user_id, peer.participation_id,
                  peer.public_key,
                  peer.generation, game.vpn_policy_revision AS policy_revision,
                  account.security_stamp
             FROM "EventVpnUserPeers" peer
             JOIN "Games" game ON game.id = peer.game_id
             JOIN "Participations" participation
               ON participation.game_id = peer.game_id
              AND participation.id = peer.participation_id
             JOIN "UserParticipations" historical
               ON historical.user_id = peer.user_id
              AND historical.game_id = peer.game_id
              AND historical.participation_id = peer.participation_id
              AND historical.team_id = participation.team_id
             JOIN "Teams" team ON team.id = participation.team_id
             JOIN "AspNetUsers" account ON account.id = peer.user_id
            WHERE peer.game_id = $1
              AND peer.user_id = $2
              AND peer.participation_id = $3
              AND peer.revoked_at_utc IS NULL
              AND game.deletion_pending = FALSE
              AND game.vpn_access_required = TRUE
              AND game.start_time_utc <= clock_timestamp()
              AND clock_timestamp() < game.end_time_utc
              AND participation.status = $4
              AND team.deletion_pending = FALSE
              AND account.email_confirmed = TRUE
              AND account.role <> $5
              AND account.security_stamp IS NOT NULL
              AND (
                    team.captain_id = peer.user_id
                    OR EXISTS (
                        SELECT 1 FROM "TeamMembers" member
                         WHERE member.team_id = team.id AND member.user_id = peer.user_id
                    )
              )"#,
    )
    .bind(game_id)
    .bind(user_id)
    .bind(participation_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(Role::Banned as i16)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

pub fn proof_url(game_id: i32) -> AppResult<String> {
    let base = std::env::var("RSCTF_EVENT_VPN_PROOF_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("https://") && value.len() > 8)
        .ok_or_else(|| {
            AppError::unavailable(
                "Event VPN requires an HTTPS RSCTF_EVENT_VPN_PROOF_URL on the browser origin",
            )
        })?;
    Ok(format!("{base}/api/game/{game_id}/vpn/proof"))
}

fn event_profile_routes(
    configured: Option<&str>,
    service_routes: Vec<String>,
    client_route: String,
) -> AppResult<Vec<String>> {
    let mut allowed = configured
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if allowed
        .iter()
        .any(|network| network == "0.0.0.0/0" || network == "::/0")
    {
        return Err(AppError::unavailable(
            "Event VPN must use split-tunnel routes; default routes are forbidden",
        ));
    }
    for route in service_routes
        .into_iter()
        .chain(std::iter::once(client_route))
    {
        if !allowed.contains(&route) {
            allowed.push(route);
        }
    }
    Ok(allowed)
}

pub async fn render_user_config(
    st: &SharedState,
    user: &CurrentUser,
    part: &participation::Model,
) -> AppResult<String> {
    let peer = ensure_user_peer(st, user, part).await?;
    let proof_url = proof_url(part.game_id)?;
    let endpoint = std::env::var("RSCTF_AD_VPN_SERVER_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::unavailable("RSCTF_AD_VPN_SERVER_ENDPOINT is required"))?;
    // The Toolkit profile also reaches managed BYOC peers in the client
    // network. Kernel policy still limits each event's peers to its exact live
    // targets, so adding the route does not grant cross-event access.
    let configured = std::env::var("RSCTF_EVENT_VPN_ALLOWED_IPS").ok();
    let allowed = event_profile_routes(
        configured.as_deref(),
        crate::services::ad_vpn::service_route_cidrs().map_err(AppError::internal)?,
        crate::services::ad_vpn::client_cidr(),
    )?;
    let dns = crate::services::ad_vpn::same_origin_access()
        .map_err(AppError::internal)?
        .map(|access| format!("DNS = {}\n", access.dns))
        .unwrap_or_default();
    let server_public_key = crate::services::ad_vpn::server_public_key(&st.db).await?;
    Ok(format!(
        "# RSCTF event {game_id}; VPN proof endpoint: {proof_url}\n\
         [Interface]\nPrivateKey = {private_key}\nAddress = {address}/32\n{dns}\n\
         [Peer]\nPublicKey = {server_public_key}\nEndpoint = {endpoint}\n\
         AllowedIPs = {allowed}\nPersistentKeepalive = 25\n",
        game_id = part.game_id,
        private_key = peer.private_key,
        address = peer.row.address,
        allowed = allowed.join(", "),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "event-vpn-test-key-0123456789abcdef";

    #[test]
    fn credential_ciphertext_is_bound_to_exact_identity_and_generation() {
        let user = Uuid::new_v4();
        let (ciphertext, nonce) = encrypt_private_key(KEY, 1, user, 2, 3, "private").unwrap();
        let peer = EventVpnUserPeer {
            id: Uuid::new_v4(),
            game_id: 1,
            user_id: user,
            participation_id: 2,
            public_key: "x".repeat(44),
            private_key_ciphertext: ciphertext,
            private_key_nonce: nonce.to_vec(),
            address: "10.13.0.2".to_string(),
            generation: 3,
            issued_at_utc: Utc::now(),
            last_config_download_at_utc: None,
            revoked_at_utc: None,
        };
        assert_eq!(decrypt_private_key(KEY, &peer).unwrap(), "private");
        let mut wrong = peer.clone();
        wrong.generation += 1;
        assert!(decrypt_private_key(KEY, &wrong).is_err());
    }

    #[test]
    fn allocation_skips_hub_network_broadcast_and_used_addresses() {
        let user = Uuid::nil();
        let first = allocate_address("10.14.0.0/29", 1, user, &HashSet::new()).unwrap();
        let mut used = HashSet::new();
        used.insert(first.parse().unwrap());
        let second = allocate_address("10.14.0.0/29", 1, user, &used).unwrap();
        assert_ne!(first, second);
        assert_ne!(first, "10.14.0.1");
    }

    #[test]
    fn personal_event_profile_includes_toolkit_routes_once() {
        let allowed = event_profile_routes(
            Some("192.0.2.0/28, 10.13.41.0/24"),
            vec!["10.13.41.0/24".to_string()],
            "10.13.42.0/24".to_string(),
        )
        .unwrap();
        assert_eq!(
            allowed,
            vec!["192.0.2.0/28", "10.13.41.0/24", "10.13.42.0/24"]
        );
        assert_eq!(
            allowed
                .iter()
                .filter(|route| route.as_str() == "10.13.41.0/24")
                .count(),
            1
        );
        assert!(
            event_profile_routes(Some("0.0.0.0/0"), Vec::new(), "10.13.42.0/24".to_string())
                .is_err()
        );
    }
}

#[cfg(test)]
#[path = "peer_pg_tests.rs"]
mod pg_tests;
