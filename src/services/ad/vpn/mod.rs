//! In-process A&D WireGuard hub and fail-closed network policy reconciliation.
//! Requires `NET_ADMIN` and `/dev/net/tun`.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::str::FromStr;

use chrono::Utc;
use defguard_wireguard_rs::key::Key;
use ipnet::{IpNet, Ipv4Net};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use crate::models::data::{ad_vpn_peer, config, game, participation};
use crate::services::container::ContainerBackendKind;
use crate::utils::enums::ParticipationStatus;
use crate::utils::error::{AppError, AppResult};
use allocation::assign_available_ip;
#[cfg(test)]
use allocation::assign_deterministic_ip;
use firewall::VpnTarget;
pub use lease::{
    acquire_instance_lease, owns_instance_lease, release_instance_lease,
    start_instance_lease_monitor,
};

const IFNAME: &str = "wg0";
const SERVER_KEY_CFG: &str = "Ad:Vpn:ServerPrivateKey";
static SYNC_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ALLOCATION_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
static VPN_BACKEND: std::sync::OnceLock<VpnBackendConfig> = std::sync::OnceLock::new();

mod allocation;
mod capture_policy;
mod cooldown;
pub(crate) mod coordination;
mod endpoints;
mod firewall;
mod firewall_atomic;
mod firewall_rules;
mod lease;
mod reconcile;

#[cfg(test)]
use reconcile::retry_operation;
pub use reconcile::{
    audit_owner_state, enforce_cycle_cooldown, ensure_hub_and_sync, reconcile_pending_for_owner,
};

pub use endpoints::{
    clear_stale_local_relays, deactivate_backend_endpoint, deactivate_backend_endpoints,
    deactivate_participation_services, deactivate_team_service,
};

/// Revoke capture-gated routes without consulting PostgreSQL. Used only while
/// a capture owner is failing or draining; normal policy uses exact DB ACKs.
pub async fn fence_capture_routes() -> AppResult<()> {
    capture_policy::fence_live().await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VpnBackendConfig {
    kind: ContainerBackendKind,
    service_cidrs: Vec<String>,
    guard_service_interfaces: bool,
}

pub fn sync_is_dirty() -> bool {
    SYNC_DIRTY.load(std::sync::atomic::Ordering::Acquire)
}

/// VPN client subnet — each team gets a /32 here; the hub owns `.1`.
pub fn client_cidr() -> String {
    canonical_env_ipv4_cidr("RSCTF_AD_VPN_CLIENT_CIDR", "10.13.37.0/24")
}

/// Docker subnet the platform A&D service containers live on (reached over the VPN).
pub fn services_cidr() -> String {
    canonical_env_ipv4_cidr("RSCTF_AD_VPN_SERVICES_CIDR", "10.13.40.0/24")
}

/// Kubernetes allocates ClusterIPs from a cluster-specific service CIDR rather
/// than the Docker A&D bridge. Deployments using the Kubernetes backend must set
/// this CIDR so generated team routes and the checker firewall can reach it.
pub fn kubernetes_services_cidr() -> Option<String> {
    std::env::var("RSCTF_K8S_AD_SERVICE_CIDR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<Ipv4Net>()
                .map(|network| network.trunc().to_string())
                .unwrap_or(value)
        })
}

fn truthy_env(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(false)
}

fn backend_config_for(kind: ContainerBackendKind) -> Result<VpnBackendConfig, String> {
    let service_cidrs = match kind {
        ContainerBackendKind::Docker => vec![services_cidr()],
        ContainerBackendKind::Kubernetes => kubernetes_services_cidr().into_iter().collect(),
        ContainerBackendKind::None | ContainerBackendKind::Worker => Vec::new(),
    };
    let config = VpnBackendConfig {
        kind,
        service_cidrs,
        guard_service_interfaces: kind == ContainerBackendKind::Docker,
    };
    validate_kubernetes_service_routes(&config)?;

    if enabled() {
        match kind {
            ContainerBackendKind::None | ContainerBackendKind::Worker => {
                return Err(
                    "the A&D VPN requires a local Docker or Kubernetes backend; remote-worker A&D routing is not enabled yet".to_string(),
                );
            }
            ContainerBackendKind::Kubernetes => {
                if truthy_env("RSCTF_K8S_HOST_NETWORK")
                    || !truthy_env("RSCTF_K8S_ISOLATED_POD_NETNS")
                {
                    return Err(
                        "the Kubernetes A&D VPN requires an isolated pod network namespace; set RSCTF_K8S_ISOLATED_POD_NETNS=true and do not use hostNetwork"
                            .to_string(),
                    );
                }
            }
            ContainerBackendKind::Docker => {}
        }
    }

    Ok(config)
}

fn validate_kubernetes_service_routes(config: &VpnBackendConfig) -> Result<(), String> {
    if config.kind == ContainerBackendKind::Kubernetes && config.service_cidrs.is_empty() {
        return Err(
            "RSCTF_K8S_AD_SERVICE_CIDR is required with RSCTF_CONTAINER_BACKEND=kubernetes, even when the A&D VPN is disabled; web provisioning and process checker isolation both need the authoritative cluster Service CIDR"
                .to_string(),
        );
    }
    Ok(())
}

/// Freeze the network-policy inputs from the backend that actually won startup
/// selection. This prevents route generation and firewall setup from disagreeing
/// with the runtime after an auto-detection fallback.
pub fn initialize_backend(kind: ContainerBackendKind) -> Result<(), String> {
    let config = backend_config_for(kind)?;
    if let Some(existing) = VPN_BACKEND.get() {
        return (existing == &config)
            .then_some(())
            .ok_or_else(|| "A&D VPN backend was initialized more than once".to_string());
    }
    VPN_BACKEND
        .set(config)
        .map_err(|_| "A&D VPN backend was initialized more than once".to_string())
}

fn backend_config() -> Result<&'static VpnBackendConfig, String> {
    VPN_BACKEND
        .get()
        .ok_or_else(|| "A&D VPN backend was not initialized".to_string())
}

pub fn service_route_cidrs() -> Result<Vec<String>, String> {
    Ok(backend_config()?.service_cidrs.clone())
}

/// A custom checker gets only exact target permissions inside these routes.
/// Kubernetes has no portable default Service CIDR, so checker-owning roles
/// must refuse startup when the operator has not supplied the cluster value.
pub fn validate_checker_service_routes() -> Result<(), String> {
    let config = backend_config()?;
    validate_kubernetes_service_routes(config)
}

fn canonical_env_ipv4_cidr(name: &str, default: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_string());
    value
        .parse::<Ipv4Net>()
        .map(|network| network.trunc().to_string())
        .unwrap_or(value)
}

fn parse_ipv4_network(value: &str, label: &str) -> Result<Ipv4Net, String> {
    match value.parse::<IpNet>() {
        Ok(IpNet::V4(network)) => Ok(network.trunc()),
        Ok(IpNet::V6(_)) => Err(format!("{label} must be an IPv4 CIDR")),
        Err(error) => Err(format!("invalid {label} {value:?}: {error}")),
    }
}

fn validate_vpn_networks(
    client_cidr: &str,
    service_cidrs: &[String],
) -> Result<(Ipv4Net, Vec<Ipv4Net>), String> {
    let client = parse_ipv4_network(client_cidr, "VPN client CIDR")?;
    if client.prefix_len() > 30 {
        return Err("VPN client CIDR must provide at least two usable addresses".to_string());
    }

    let mut services = Vec::new();
    for value in service_cidrs {
        let service = parse_ipv4_network(value, "A&D service CIDR")?;
        let overlaps = client.contains(&service.network()) || service.contains(&client.network());
        if overlaps {
            return Err(format!(
                "VPN client CIDR {client} overlaps A&D service CIDR {service}"
            ));
        }
        if !services.contains(&service) {
            services.push(service);
        }
    }
    if services.is_empty() {
        return Err("at least one A&D service CIDR is required".to_string());
    }
    Ok((client, services))
}

fn peer_address_allowed(address: &str, client: &Ipv4Net, services: &[Ipv4Net]) -> bool {
    let Ok(address) = address.parse::<Ipv4Addr>() else {
        return false;
    };
    client.contains(&address)
        && address != client.network()
        && address != client.broadcast()
        && address != Ipv4Addr::from(u32::from(client.network()).saturating_add(1))
        && services.iter().all(|service| !service.contains(&address))
}

fn vpn_target(
    address: &str,
    port: i32,
    client: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> Option<VpnTarget> {
    let address = address.parse::<Ipv4Addr>().ok()?;
    let port = u16::try_from(port).ok().filter(|port| *port > 0)?;
    let in_client = client.contains(&address)
        && address != client.network()
        && address != client.broadcast()
        && address != Ipv4Addr::from(u32::from(client.network()).saturating_add(1));
    let in_services = service_networks.iter().any(|network| {
        let first_address = if network.prefix_len() == 32 {
            network.network()
        } else {
            Ipv4Addr::from(u32::from(network.network()).saturating_add(1))
        };
        network.contains(&address)
                && address != network.network()
                && address != network.broadcast()
                // Docker bridge gateways and the conventional first Kubernetes
                // ClusterIP are control-plane addresses, never challenge targets.
                && address != first_address
    });
    (in_client || in_services).then_some(VpnTarget { address, port })
}

/// Docker network name the platform A&D service containers join.
pub fn services_network() -> String {
    std::env::var("RSCTF_AD_VPN_SERVICES_NETWORK").unwrap_or_else(|_| "rsctf-ad".to_string())
}

/// Separate bridge used only by A&D services explicitly allowed outbound
/// access. It is never joined to rsctf, Postgres, or Redis.
pub fn egress_network() -> String {
    std::env::var("RSCTF_AD_VPN_EGRESS_NETWORK").unwrap_or_else(|_| "rsctf-ad-egress".to_string())
}

pub fn listen_port() -> u16 {
    std::env::var("RSCTF_AD_VPN_LISTEN_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(51820)
}

pub fn required() -> bool {
    std::env::var("RSCTF_AD_VPN_REQUIRED")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub fn enabled() -> bool {
    std::env::var("RSCTF_AD_VPN_ENABLED")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

pub async fn reconcile_for_deployment(db: &DatabaseConnection) -> AppResult<()> {
    if !enabled() {
        if required() {
            return Err(AppError::internal(
                "RSCTF_AD_VPN_REQUIRED requires RSCTF_AD_VPN_ENABLED",
            ));
        }
        return Ok(());
    }
    let owns_network = owns_instance_lease();
    match ensure_hub_and_sync(db).await {
        Ok(()) => Ok(()),
        Err(error @ AppError::ServiceUnavailable(_)) => Err(error),
        // A non-owner has no safe local fallback: its mutation is not complete
        // until the network owner acknowledges the corresponding generation.
        Err(error) if required() || !owns_network => Err(error),
        Err(error) => {
            tracing::warn!(%error, "A&D VPN unavailable; continuing with the tunnel disabled");
            Ok(())
        }
    }
}

/// The hub owns the first usable address in the canonical client network.
pub fn hub_address() -> String {
    parse_ipv4_network(&client_cidr(), "VPN client CIDR")
        .map(|network| Ipv4Addr::from(u32::from(network.network()).saturating_add(1)).to_string())
        .unwrap_or_else(|_| "10.13.37.1".to_string())
}

async fn sync_byoc_service_hosts(
    db: &DatabaseConnection,
    participation_id: i32,
    address: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"
        UPDATE "AdTeamServices" service
           SET host = $1,
               port = COALESCE(challenge.expose_port, NULLIF(service.port, 0), 80)
          FROM "GameChallenges" challenge
         WHERE service.challenge_id = challenge.id
           AND service.participation_id = $2
           AND service.container_id IS NULL
           AND challenge.ad_self_hosted = TRUE
           AND (service.host = '' OR service.host = $1)
        "#,
    )
    .bind(address)
    .bind(participation_id)
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // Repair legacy direct-peer rows from the old /16 or /24 allocation without
    // touching a live app-local relay in the services CIDR. The observed-host
    // predicate makes this safe if an agent publishes a relay concurrently.
    let configured_services: Vec<Ipv4Net> = service_route_cidrs()
        .map_err(AppError::internal)?
        .iter()
        .filter_map(|cidr| cidr.parse().ok())
        .collect();
    let legacy_clients: Vec<Ipv4Net> = [
        client_cidr(),
        "10.13.0.0/16".to_string(),
        "10.13.37.0/24".to_string(),
    ]
    .iter()
    .filter_map(|cidr| cidr.parse().ok())
    .collect();
    let legacy_rows = sqlx::query_as::<_, (i32, String)>(
        r#"
        SELECT service.id, service.host
          FROM "AdTeamServices" service
          JOIN "GameChallenges" challenge ON challenge.id = service.challenge_id
         WHERE service.participation_id = $1
           AND service.container_id IS NULL
           AND challenge.ad_self_hosted = TRUE
           AND service.host <> ''
           AND service.host <> $2
        "#,
    )
    .bind(participation_id)
    .bind(address)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    for (service_id, observed_host) in legacy_rows {
        let Ok(candidate) = observed_host.parse::<Ipv4Addr>() else {
            continue;
        };
        if configured_services
            .iter()
            .any(|network| network.contains(&candidate))
            || !legacy_clients
                .iter()
                .any(|network| network.contains(&candidate))
        {
            continue;
        }
        sqlx::query(
            r#"
            UPDATE "AdTeamServices" service
               SET host = $1,
                   port = COALESCE(challenge.expose_port, NULLIF(service.port, 0), 80)
              FROM "GameChallenges" challenge
             WHERE service.id = $2
               AND service.challenge_id = challenge.id
               AND service.host = $3
               AND service.container_id IS NULL
               AND challenge.ad_self_hosted = TRUE
            "#,
        )
        .bind(address)
        .bind(service_id)
        .bind(observed_host)
        .execute(db.get_postgres_connection_pool())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

async fn invalidate_byoc_service_hosts(
    db: &DatabaseConnection,
    participation_id: i32,
    address: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"
        UPDATE "AdTeamServices"
           SET host = '', port = 0, status = 2
         WHERE participation_id = $1
           AND container_id IS NULL
           AND host = $2
        "#,
    )
    .bind(participation_id)
    .bind(address)
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Revoke peer rows and their direct-BYOC address references under the same
/// allocator lock used for grants. The address cannot be reassigned between the
/// dependent-row cleanup and the peer deletion.
pub async fn revoke_peers_for_participations(
    db: &DatabaseConnection,
    participation_ids: &[i32],
) -> AppResult<u64> {
    if participation_ids.is_empty() {
        return Ok(0);
    }
    let _local_allocation = ALLOCATION_LOCK.lock().await;
    let allocation_lock = crate::utils::single_flight::PgAdvisoryLock::acquire(
        db.get_postgres_connection_pool(),
        "ad-vpn-peer-allocation",
    )
    .await
    .map_err(|error| AppError::internal(format!("lock VPN address allocation: {error}")))?;
    let peers = ad_vpn_peer::Entity::find()
        .filter(ad_vpn_peer::Column::ParticipationId.is_in(participation_ids.to_vec()))
        .all(db)
        .await?;
    if !peers.is_empty() {
        SYNC_DIRTY.store(true, std::sync::atomic::Ordering::Release);
    }
    for peer in &peers {
        invalidate_byoc_service_hosts(db, peer.participation_id, &peer.address).await?;
    }
    let peer_ids: Vec<i32> = peers.iter().map(|peer| peer.id).collect();
    let removed = if peer_ids.is_empty() {
        0
    } else {
        ad_vpn_peer::Entity::delete_many()
            .filter(ad_vpn_peer::Column::Id.is_in(peer_ids))
            .exec(db)
            .await?
            .rows_affected
    };
    allocation_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    drop(_local_allocation);

    if removed > 0 || sync_is_dirty() {
        let sync = ensure_hub_and_sync(db).await;
        if removed > 0 {
            sync?;
        } else if let Err(error) = sync {
            tracing::warn!(%error, "VPN cleanup retry remains pending");
        }
    }
    Ok(removed)
}

/// Load the hub's server private key from `Configs`, generating + persisting one
/// on first use so the interface key is stable across restarts.
async fn server_key(db: &DatabaseConnection) -> AppResult<Key> {
    if let Some(row) = config::Entity::find_by_id(SERVER_KEY_CFG.to_string())
        .one(db)
        .await?
    {
        if let Some(v) = row.value {
            if let Ok(k) = Key::from_str(v.trim()) {
                return Ok(k);
            }
        }
    }
    let key = Key::generate();
    let value = key.to_string();
    match config::Entity::find_by_id(SERVER_KEY_CFG.to_string())
        .one(db)
        .await?
    {
        Some(existing) => {
            let mut am: config::ActiveModel = existing.into();
            am.value = Set(Some(value));
            am.update(db).await?;
        }
        None => {
            config::ActiveModel {
                config_key: Set(SERVER_KEY_CFG.to_string()),
                value: Set(Some(value)),
                cache_keys: Set(None),
            }
            .insert(db)
            .await?;
        }
    }
    Ok(key)
}

/// The hub's WireGuard public key (base64) — rendered as the `Peer` PublicKey in
/// every team's downloaded `.conf`.
pub async fn server_public_key(db: &DatabaseConnection) -> AppResult<String> {
    Ok(server_key(db).await?.public_key().to_string())
}

/// Get-or-create the persisted WireGuard peer for a team, assigning a stable /32.
pub async fn ensure_peer(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
) -> AppResult<ad_vpn_peer::Model> {
    ensure_peer_inner(db, game_id, participation_id, true).await
}

/// Allocate durable peer credentials as part of a bulk provisioning pass. The
/// caller must reconcile once after all rows and service endpoints are ready.
pub(crate) async fn ensure_peer_deferred(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
) -> AppResult<ad_vpn_peer::Model> {
    ensure_peer_inner(db, game_id, participation_id, false).await
}

async fn ensure_peer_inner(
    db: &DatabaseConnection,
    game_id: i32,
    participation_id: i32,
    reconcile: bool,
) -> AppResult<ad_vpn_peer::Model> {
    if !enabled() {
        return Err(AppError::bad_request("The A&D VPN is disabled"));
    }
    let configured_client_cidr = client_cidr();
    let configured_service_cidrs = service_route_cidrs().map_err(AppError::internal)?;
    let (client_network, service_networks) =
        validate_vpn_networks(&configured_client_cidr, &configured_service_cidrs)
            .map_err(AppError::internal)?;

    participation::Entity::find_by_id(participation_id)
        .one(db)
        .await?
        .filter(|part| part.game_id == game_id && part.status == ParticipationStatus::Accepted)
        .ok_or_else(|| AppError::Forbidden)?;
    game::Entity::find_by_id(game_id)
        .one(db)
        .await?
        .filter(|game| game.is_active(Utc::now()))
        .ok_or_else(|| AppError::bad_request("VPN access is only available during the game"))?;

    // Queue locally before holding a pooled advisory-lock transaction. Without
    // this gate, same-replica waiters could each retain one pool connection and
    // starve the lock holder's ORM query/insert connection.
    let _local_allocation = ALLOCATION_LOCK.lock().await;
    let allocation_lock = crate::utils::single_flight::PgAdvisoryLock::acquire(
        db.get_postgres_connection_pool(),
        "ad-vpn-peer-allocation",
    )
    .await
    .map_err(|error| AppError::internal(format!("lock VPN address allocation: {error}")))?;

    // Status/game windows can change while this request waits for allocation.
    // Revalidate under the allocator lock so teardown cannot be followed by a
    // stale peer recreation from a pre-lock participation snapshot.
    let still_authorized = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1
                 FROM "Participations" participation
                 JOIN "Games" game ON game.id = participation.game_id
                WHERE participation.id = $1
                  AND participation.game_id = $2
                  AND participation.status = $3
                  AND game.start_time_utc <= now()
                  AND now() <= game.end_time_utc
           )"#,
    )
    .bind(participation_id)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .fetch_one(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !still_authorized {
        allocation_lock
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        drop(_local_allocation);
        return Err(AppError::Forbidden);
    }

    let existing = ad_vpn_peer::Entity::find()
        .filter(ad_vpn_peer::Column::GameId.eq(game_id))
        .filter(ad_vpn_peer::Column::ParticipationId.eq(participation_id))
        .one(db)
        .await?;
    if let Some(existing) = existing.as_ref() {
        if peer_address_allowed(&existing.address, &client_network, &service_networks) {
            sync_byoc_service_hosts(db, participation_id, &existing.address).await?;
            allocation_lock
                .release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            drop(_local_allocation);
            if reconcile {
                ensure_hub_and_sync(db).await?;
            }
            return Ok(existing.clone());
        }
    }
    let key = Key::generate();
    let used_addresses: HashSet<Ipv4Addr> = ad_vpn_peer::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .filter(|peer| {
            existing
                .as_ref()
                .is_none_or(|current| peer.id != current.id)
        })
        .filter_map(|peer| peer.address.parse().ok())
        .collect();
    let address = assign_available_ip(&configured_client_cidr, participation_id, &used_addresses)
        .filter(|address| peer_address_allowed(address, &client_network, &service_networks))
        .ok_or_else(|| AppError::internal("Could not assign a safe VPN address"))?;
    let peer = if let Some(existing) = existing {
        // From this point onward any failure must be retried by kernel policy
        // reconciliation; the old key/address may still be installed in wg0.
        SYNC_DIRTY.store(true, std::sync::atomic::Ordering::Release);
        invalidate_byoc_service_hosts(db, participation_id, &existing.address).await?;
        let mut active: ad_vpn_peer::ActiveModel = existing.into();
        active.private_key = Set(key.to_string());
        active.public_key = Set(key.public_key().to_string());
        active.address = Set(address);
        active.created_utc = Set(Utc::now());
        active.update(db).await?
    } else {
        ad_vpn_peer::ActiveModel {
            game_id: Set(game_id),
            participation_id: Set(participation_id),
            private_key: Set(key.to_string()),
            public_key: Set(key.public_key().to_string()),
            address: Set(address),
            created_utc: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await?
    };
    sync_byoc_service_hosts(db, participation_id, &peer.address).await?;
    allocation_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    drop(_local_allocation);
    if reconcile {
        ensure_hub_and_sync(db).await?;
    }
    Ok(peer)
}

/// Validate that a string parses as an IPv4 address (used before handing an
/// address to Docker / the checker).
pub fn is_ipv4(s: &str) -> bool {
    Ipv4Addr::from_str(s).is_ok()
}

#[cfg(test)]
mod tests;
