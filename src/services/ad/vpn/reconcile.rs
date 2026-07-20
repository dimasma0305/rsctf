//! Serialized, crash-retryable reconciliation of database VPN intent to kernel state.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::Ipv4Addr;
use std::str::FromStr;

use defguard_wireguard_rs::{
    key::Key, net::IpAddrMask, peer::Peer, InterfaceConfiguration, WGApi, WireguardInterfaceApi,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use sha2::{Digest, Sha256};

use super::firewall::{CooldownBlock, GameVpnPolicy, VpnTarget};
use super::*;

static SYNC_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
static APPLIED_STATE: std::sync::LazyLock<std::sync::Mutex<Option<AppliedState>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppliedPeer {
    game_id: i32,
    public_key: String,
    address: Ipv4Addr,
}

#[derive(Debug, Clone)]
struct AppliedState {
    fingerprint: [u8; 32],
    hub_identity: [u8; 32],
    peers: Vec<AppliedPeer>,
    policies: Vec<GameVpnPolicy>,
}

fn contains_required_cooldowns(
    state: &AppliedState,
    required: &[cooldown::RequiredCooldownBlock],
) -> bool {
    required.iter().all(|required| {
        state
            .peers
            .iter()
            .any(|peer| peer.game_id == required.game_id && peer.address == required.block.peer)
            && state.policies.iter().any(|policy| {
                policy.game_id == required.game_id
                    && policy.peers.contains(&required.block.peer)
                    && policy.targets.contains(&required.block.target)
                    && policy.cooldown_blocks.contains(&required.block)
            })
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
struct TransitionQuarantine {
    peers: Vec<Ipv4Addr>,
    blocks: Vec<CooldownBlock>,
}

impl TransitionQuarantine {
    fn is_empty(&self) -> bool {
        self.peers.is_empty() && self.blocks.is_empty()
    }
}

fn transition_quarantine(previous: &AppliedState, desired: &AppliedState) -> TransitionQuarantine {
    let mut peers = BTreeSet::new();
    let mut blocks = BTreeSet::new();
    for peer in &previous.peers {
        if !desired.peers.contains(peer) {
            peers.insert(peer.address);
        }
    }
    for old in &previous.policies {
        let Some(new) = desired
            .policies
            .iter()
            .find(|policy| policy.game_id == old.game_id)
        else {
            peers.extend(old.peers.iter().copied());
            continue;
        };
        peers.extend(
            old.peers
                .iter()
                .filter(|peer| !new.peers.contains(peer))
                .copied(),
        );
        for target in old
            .targets
            .iter()
            .filter(|target| !new.targets.contains(target))
        {
            blocks.extend(
                old.peers
                    .iter()
                    .map(|peer| (*peer, target.address, target.port)),
            );
        }
    }
    for new in &desired.policies {
        let old = previous
            .policies
            .iter()
            .find(|policy| policy.game_id == new.game_id);
        for block in &new.cooldown_blocks {
            if old.is_none_or(|policy| !policy.cooldown_blocks.contains(block)) {
                blocks.insert((block.peer, block.target.address, block.target.port));
            }
        }
    }
    TransitionQuarantine {
        peers: peers.into_iter().collect(),
        blocks: blocks
            .into_iter()
            .map(|(peer, address, port)| CooldownBlock {
                peer,
                target: VpnTarget { address, port },
            })
            .collect(),
    }
}

fn build_peer(row: &ad_vpn_peer::Model) -> Option<(AppliedPeer, Peer)> {
    let address = row.address.parse::<Ipv4Addr>().ok()?;
    let public_key = Key::from_str(&row.public_key).ok()?;
    let mut peer = Peer::new(public_key);
    peer.allowed_ips
        .push(IpAddrMask::from_str(&format!("{address}/32")).ok()?);
    Some((
        AppliedPeer {
            game_id: row.game_id,
            public_key: row.public_key.clone(),
            address,
        },
        peer,
    ))
}

fn hub_identity(prvkey: &str, port: u16, address: &str) -> [u8; 32] {
    let mut identity = Sha256::new();
    identity.update(prvkey.as_bytes());
    identity.update(port.to_be_bytes());
    identity.update(address.as_bytes());
    identity.finalize().into()
}

fn policy_fingerprint(
    prvkey: &str,
    port: u16,
    configured_client_cidr: &str,
    configured_service_cidrs: &[String],
    route_fingerprint: &str,
    peers: &[ad_vpn_peer::Model],
    policies: &[GameVpnPolicy],
) -> [u8; 32] {
    let mut fingerprint = Sha256::new();
    fingerprint.update(prvkey.as_bytes());
    fingerprint.update(port.to_be_bytes());
    fingerprint.update(configured_client_cidr.as_bytes());
    for cidr in configured_service_cidrs {
        fingerprint.update(cidr.as_bytes());
    }
    fingerprint.update(route_fingerprint.as_bytes());
    for peer in peers {
        fingerprint.update(peer.id.to_be_bytes());
        fingerprint.update(peer.game_id.to_be_bytes());
        fingerprint.update(peer.participation_id.to_be_bytes());
        fingerprint.update(peer.public_key.as_bytes());
        fingerprint.update(peer.address.as_bytes());
    }
    for policy in policies {
        fingerprint.update(policy.game_id.to_be_bytes());
        for peer in &policy.peers {
            fingerprint.update(peer.octets());
        }
        for target in &policy.targets {
            fingerprint.update(target.address.octets());
            fingerprint.update(target.port.to_be_bytes());
        }
        for block in &policy.cooldown_blocks {
            fingerprint.update(block.peer.octets());
            fingerprint.update(block.target.address.octets());
            fingerprint.update(block.target.port.to_be_bytes());
        }
    }
    fingerprint.finalize().into()
}

fn same_allowed_ips(current: &Peer, desired: &Peer) -> bool {
    let mut current = current.allowed_ips.clone();
    let mut desired = desired.allowed_ips.clone();
    current.sort_by_key(ToString::to_string);
    desired.sort_by_key(ToString::to_string);
    current == desired
}

fn same_hub_key(current: Option<&Key>, desired: &Key) -> bool {
    // Linux normalizes ("clamps") the Curve25519 private scalar before storing
    // it in WireGuard. A subsequent netlink read therefore need not be
    // byte-identical to the persisted input even though both keys describe the
    // same interface identity. Compare the derived public keys so routine
    // policy changes stay on the non-disruptive peer-update path.
    current.is_some_and(|key| key.public_key() == desired.public_key())
}

enum IncrementalError {
    NeedsFull,
    Failed(String),
}

/// Update only changed peers. Unlike `configure_interface`, this never flushes
/// the hub address, so handshakes and traffic for unrelated teams continue.
fn configure_hub_incremental(cfg: &InterfaceConfiguration) -> Result<(), IncrementalError> {
    let api = WGApi::<defguard_wireguard_rs::Kernel>::new(IFNAME.to_string())
        .map_err(|error| IncrementalError::Failed(format!("WGApi::new: {error}")))?;
    let current = api
        .read_interface_data()
        .map_err(|_| IncrementalError::NeedsFull)?;
    let desired_key = Key::from_str(&cfg.prvkey).map_err(|_| IncrementalError::NeedsFull)?;
    if !same_hub_key(current.private_key.as_ref(), &desired_key) || current.listen_port != cfg.port
    {
        return Err(IncrementalError::NeedsFull);
    }
    let desired: HashMap<_, _> = cfg
        .peers
        .iter()
        .map(|peer| (peer.public_key.clone(), peer))
        .collect();
    for key in current
        .peers
        .keys()
        .filter(|key| !desired.contains_key(*key))
    {
        api.remove_peer(key)
            .map_err(|error| IncrementalError::Failed(format!("remove peer: {error}")))?;
    }
    for peer in &cfg.peers {
        if current
            .peers
            .get(&peer.public_key)
            .is_some_and(|installed| same_allowed_ips(installed, peer))
        {
            continue;
        }
        api.configure_peer(peer)
            .map_err(|error| IncrementalError::Failed(format!("configure peer: {error}")))?;
    }
    Ok(())
}

/// Blocking WireGuard interface bring-up used only at bootstrap or after
/// externally observed interface identity loss.
fn configure_hub(cfg: &InterfaceConfiguration) -> Result<(), String> {
    let mut api = WGApi::<defguard_wireguard_rs::Kernel>::new(IFNAME.to_string())
        .map_err(|error| format!("WGApi::new: {error}"))?;
    let _ = api.create_interface();
    api.configure_interface(cfg)
        .map_err(|error| format!("configure_interface: {error}"))
}

fn configure_hub_with_retry(cfg: &InterfaceConfiguration, attempts: usize) -> Result<(), String> {
    retry_operation(
        attempts,
        || configure_hub(cfg),
        |attempt| std::thread::sleep(std::time::Duration::from_millis(150 * attempt as u64)),
    )
}

fn apply_kernel_state(
    cfg: &InterfaceConfiguration,
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
    policies: &[GameVpnPolicy],
    guard_service_interfaces: bool,
    incremental: bool,
) -> Result<(), String> {
    if incremental {
        match configure_hub_incremental(cfg) {
            Ok(()) => {
                let lock = firewall::setup_vpn_firewall(
                    client_network,
                    service_networks,
                    policies,
                    guard_service_interfaces,
                )?;
                return lock.unlock();
            }
            Err(IncrementalError::Failed(error)) => return Err(error),
            Err(IncrementalError::NeedsFull) => {}
        }
    }

    let early = firewall::lock_existing_vpn()?;
    let policy_lock = firewall::setup_vpn_firewall(
        client_network,
        service_networks,
        policies,
        guard_service_interfaces,
    )?;
    configure_hub_with_retry(cfg, 3)?;
    policy_lock.unlock()?;
    early.unlock()
}

async fn load_peers(
    db: &DatabaseConnection,
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> AppResult<Vec<ad_vpn_peer::Model>> {
    let local_allocation = ALLOCATION_LOCK.lock().await;
    let allocation_lock = crate::utils::single_flight::PgAdvisoryLock::acquire(
        db.get_postgres_connection_pool(),
        "ad-vpn-peer-allocation",
    )
    .await
    .map_err(|error| AppError::internal(format!("lock VPN address reconciliation: {error}")))?;
    let all_peers = ad_vpn_peer::Entity::find().all(db).await?;
    let part_ids: Vec<i32> = all_peers.iter().map(|peer| peer.participation_id).collect();
    let parts: HashMap<i32, participation::Model> = participation::Entity::find()
        .filter(participation::Column::Id.is_in(part_ids))
        .all(db)
        .await?
        .into_iter()
        .map(|part| (part.id, part))
        .collect();
    let game_ids: Vec<i32> = all_peers.iter().map(|peer| peer.game_id).collect();
    let eligible_games: std::collections::HashSet<i32> = sqlx::query_scalar(
        r#"SELECT id
             FROM "Games"
            WHERE id = ANY($1)
              AND deletion_pending = FALSE
              AND start_time_utc <= clock_timestamp()
              AND clock_timestamp() <= end_time_utc"#,
    )
    .bind(&game_ids)
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .collect();
    let team_ids: Vec<i32> = parts.values().map(|part| part.team_id).collect();
    let eligible_teams = crate::services::ad::roster::eligible_shared_credential_teams(
        db.get_postgres_connection_pool(),
        &team_ids,
    )
    .await?;
    let (mut peers, invalid): (Vec<_>, Vec<_>) = all_peers.into_iter().partition(|peer| {
        peer_address_allowed(&peer.address, client_network, service_networks)
            && parts.get(&peer.participation_id).is_some_and(|part| {
                part.game_id == peer.game_id
                    && part.status == ParticipationStatus::Accepted
                    && eligible_teams.contains(&part.team_id)
                    && eligible_games.contains(&peer.game_id)
            })
    });
    peers.sort_by_key(|peer| peer.id);
    for peer in &invalid {
        invalidate_byoc_service_hosts(db, peer.participation_id, &peer.address).await?;
    }
    let invalid_ids: Vec<i32> = invalid.into_iter().map(|peer| peer.id).collect();
    if !invalid_ids.is_empty() {
        ad_vpn_peer::Entity::delete_many()
            .filter(ad_vpn_peer::Column::Id.is_in(invalid_ids))
            .exec(db)
            .await?;
    }
    allocation_lock
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    drop(local_allocation);
    Ok(peers)
}

async fn load_policies(
    db: &DatabaseConnection,
    peers: &[ad_vpn_peer::Model],
    client_network: &Ipv4Net,
    service_networks: &[Ipv4Net],
) -> AppResult<Vec<GameVpnPolicy>> {
    let endpoints = sqlx::query_as::<_, (i32, String, i32)>(
        r#"
        SELECT service.game_id, service.host, service.port
          FROM "AdTeamServices" service
          JOIN "Games" game ON game.id = service.game_id
          JOIN "Participations" participation ON participation.id = service.participation_id
             AND participation.game_id = service.game_id
          JOIN "GameChallenges" challenge ON challenge.id = service.challenge_id
             AND challenge.game_id = service.game_id
         WHERE game.start_time_utc <= now() AND now() <= game.end_time_utc
           AND game.deletion_pending = FALSE
           AND participation.status = 1
           AND EXISTS (
             SELECT 1 FROM "Teams" team
              WHERE team.id = participation.team_id
                AND team.deletion_pending = FALSE
           )
           AND challenge.is_enabled = TRUE
           AND challenge.deletion_pending = FALSE
           AND challenge.review_status = 0 AND challenge."Type" = 4
           AND ((challenge.ad_self_hosted = TRUE AND service.container_id IS NULL)
             OR (challenge.ad_self_hosted = FALSE AND service.container_id IS NOT NULL))
           AND service.port BETWEEN 1 AND 65535
           AND (
             challenge.enable_traffic_capture = FALSE
             OR EXISTS (
               SELECT 1
                 FROM "TrafficCaptureLiveEndpoints" live
                 JOIN "TrafficCaptureOwnerState" owner ON owner.id = 1
                WHERE live.service_id = service.id
                  AND live.container_id = BTRIM(service.container_id)
                  AND live.host = BTRIM(service.host)
                  AND live.port = service.port
                  AND live.owner_id = owner.owner_id
                  AND live.owner_epoch = owner.owner_epoch
                  AND owner.draining = FALSE
                  AND owner.lease_expires_at > clock_timestamp()
             )
           )
        UNION ALL
        SELECT target.game_id, target.host, target.port
          FROM "KothTargets" target
          JOIN "Games" game ON game.id = target.game_id
          JOIN "GameChallenges" challenge ON challenge.id = target.challenge_id
             AND challenge.game_id = target.game_id
         WHERE game.start_time_utc <= now() AND now() <= game.end_time_utc
           AND game.deletion_pending = FALSE
           AND challenge.is_enabled = TRUE
           AND challenge.deletion_pending = FALSE
           AND challenge.review_status = 0
           AND challenge."Type" = 5 AND target.container_id IS NOT NULL
           AND target.port BETWEEN 1 AND 65535
        "#,
    )
    .fetch_all(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let blocks = cooldown::load_active_blocks(db, client_network, service_networks).await?;
    let mut policies = BTreeMap::<i32, GameVpnPolicy>::new();
    for peer in peers {
        let Ok(address) = peer.address.parse::<Ipv4Addr>() else {
            continue;
        };
        policies
            .entry(peer.game_id)
            .or_insert_with(|| GameVpnPolicy {
                game_id: peer.game_id,
                peers: Vec::new(),
                targets: Vec::new(),
                cooldown_blocks: Vec::new(),
            })
            .peers
            .push(address);
    }
    for (game_id, host, port) in endpoints {
        let Some(policy) = policies.get_mut(&game_id) else {
            continue;
        };
        if let Some(target) = vpn_target(&host, port, client_network, service_networks) {
            if client_network.contains(&target.address) && !policy.peers.contains(&target.address) {
                continue;
            }
            if !policy.targets.contains(&target) {
                policy.targets.push(target);
            }
        }
    }
    for (game_id, block) in blocks {
        if let Some(policy) = policies.get_mut(&game_id) {
            if !policy.cooldown_blocks.contains(&block) {
                policy.cooldown_blocks.push(block);
            }
        }
    }
    let mut policies: Vec<_> = policies.into_values().collect();
    for policy in &mut policies {
        policy.peers.sort_unstable();
        policy.peers.dedup();
        policy
            .targets
            .sort_by_key(|target| (target.address, target.port));
        policy
            .cooldown_blocks
            .sort_by_key(|block| (block.peer, block.target.address, block.target.port));
    }
    Ok(policies)
}

/// Reconcile peers incrementally and activate firewall intent atomically. Only
/// bootstrap/repair uses the global fail-closed guard; routine restrictive
/// changes quarantine the affected teams until all kernel state is durable.
pub async fn ensure_hub_and_sync(db: &DatabaseConnection) -> AppResult<()> {
    if !enabled() {
        return Ok(());
    }
    let generation = super::coordination::request(db).await?;
    if !super::owns_instance_lease() {
        return super::coordination::wait_until_applied(db, generation).await;
    }

    reconcile_pending_for_owner(db).await?;
    super::coordination::wait_until_applied(db, generation).await
}

/// Apply and acknowledge one exact durable generation snapshot on the owner.
pub async fn reconcile_pending_for_owner(db: &DatabaseConnection) -> AppResult<bool> {
    reconcile_owner(db, false).await
}

/// Audit owner state without creating or advancing a generation.
pub async fn audit_owner_state(db: &DatabaseConnection) -> AppResult<bool> {
    reconcile_owner(db, true).await
}

async fn reconcile_owner(db: &DatabaseConnection, force: bool) -> AppResult<bool> {
    if !enabled() {
        return Ok(false);
    }
    if !super::owns_instance_lease() {
        return Err(AppError::unavailable(
            "A&D network reconciliation requires the singleton network lease",
        ));
    }
    let _sync_guard = SYNC_LOCK.lock().await;
    let ownership = crate::utils::single_flight::PgAdvisoryLock::acquire(
        db.get_postgres_connection_pool(),
        "ad-vpn-kernel-reconcile",
    )
    .await
    .map_err(|error| AppError::internal(format!("lock VPN kernel reconciliation: {error}")))?;
    let generation = super::coordination::pending_snapshot(db).await?;
    if !owner_reconcile_required(generation, force) {
        ownership
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(false);
    }
    let result = ensure_hub_and_sync_owned(db).await;
    let result = match result {
        Ok(()) => match generation {
            Some(generation) => super::coordination::acknowledge(db, generation).await,
            None => Ok(()),
        },
        Err(error) => Err(error),
    };
    if result.is_ok() {
        ownership
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    result.map(|()| true)
}

fn owner_reconcile_required(
    pending_generation: Option<super::coordination::Generation>,
    force: bool,
) -> bool {
    force || pending_generation.is_some()
}

async fn ensure_hub_and_sync_owned(db: &DatabaseConnection) -> AppResult<()> {
    let was_dirty = SYNC_DIRTY.swap(true, std::sync::atomic::Ordering::AcqRel);
    let configured_client_cidr = client_cidr();
    let configured_service_cidrs = service_route_cidrs().map_err(AppError::internal)?;
    let (client_network, service_networks) =
        validate_vpn_networks(&configured_client_cidr, &configured_service_cidrs)
            .map_err(AppError::internal)?;
    let prvkey = server_key(db).await?.to_string();
    let peers_rows = load_peers(db, &client_network, &service_networks).await?;
    super::capture_policy::refresh(db).await?;
    let policies = load_policies(db, &peers_rows, &client_network, &service_networks).await?;
    let guard_service_interfaces = backend_config()
        .map_err(AppError::internal)?
        .guard_service_interfaces;
    let route_networks = service_networks.clone();
    let route_fingerprint = tokio::task::spawn_blocking(move || {
        firewall::service_route_fingerprint(&route_networks, guard_service_interfaces)
    })
    .await
    .map_err(|error| AppError::internal(format!("VPN route check task failed: {error}")))?
    .map_err(AppError::internal)?;
    let port = listen_port();
    let address = format!("{}/{}", hub_address(), cidr_bits(&configured_client_cidr));
    let fingerprint = policy_fingerprint(
        &prvkey,
        port,
        &configured_client_cidr,
        &configured_service_cidrs,
        &route_fingerprint,
        &peers_rows,
        &policies,
    );
    let (applied_peers, peers): (Vec<_>, Vec<_>) = peers_rows.iter().filter_map(build_peer).unzip();
    let desired = AppliedState {
        fingerprint,
        hub_identity: hub_identity(&prvkey, port, &address),
        peers: applied_peers,
        policies: policies.clone(),
    };
    let previous = APPLIED_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let firewall_client = client_network;
    let firewall_services = service_networks.clone();
    let firewall_installed = tokio::task::spawn_blocking(move || {
        firewall::vpn_firewall_installed(
            &firewall_client,
            &firewall_services,
            guard_service_interfaces,
        )
    })
    .await
    .map_err(|error| AppError::internal(format!("VPN firewall check task failed: {error}")))?
    .map_err(AppError::internal)?;
    if firewall_installed
        && !was_dirty
        && previous
            .as_ref()
            .is_some_and(|state| state.fingerprint == fingerprint)
    {
        SYNC_DIRTY.store(false, std::sync::atomic::Ordering::Release);
        return Ok(());
    }

    let quarantine = previous
        .as_ref()
        .map(|state| transition_quarantine(state, &desired))
        .unwrap_or_default();
    if firewall_installed && !quarantine.is_empty() {
        let quarantine_peers = quarantine.peers.clone();
        let quarantine_blocks = quarantine.blocks.clone();
        let quarantine_result = tokio::task::spawn_blocking(move || {
            firewall_atomic::quarantine_transition(&quarantine_peers, &quarantine_blocks)
        })
        .await
        .map_err(|error| AppError::internal(format!("VPN quarantine task failed: {error}")))?;
        if let Err(error) = quarantine_result {
            // A selective deny failure falls back to the original global
            // fail-closed boundary. FirewallLock deliberately has no Drop
            // unlock, so this remains armed until a successful recovery.
            let lock_result = tokio::task::spawn_blocking(firewall::lock_existing_vpn)
                .await
                .map_err(|join| AppError::internal(format!("VPN lock task failed: {join}")))?;
            return match lock_result {
                Ok(_) => Err(AppError::internal(format!(
                    "failed to quarantine changed VPN peers: {error}"
                ))),
                Err(lock_error) => Err(AppError::internal(format!(
                    "failed to quarantine changed VPN peers: {error}; fail-closed lock also failed: {lock_error}"
                ))),
            };
        }
    }
    let incremental = firewall_installed
        && previous
            .as_ref()
            .is_some_and(|state| state.hub_identity == desired.hub_identity);
    let cfg = InterfaceConfiguration {
        name: IFNAME.to_string(),
        prvkey,
        addresses: IpAddrMask::from_str(&address).into_iter().collect(),
        port,
        peers,
        mtu: None,
        fwmark: None,
    };
    let kernel_client = client_network;
    let kernel_services = service_networks;
    let kernel_policies = policies;
    let count = peers_rows.len();
    let applied = tokio::task::spawn_blocking(move || {
        firewall_atomic::apply_then_release_guards(
            || {
                apply_kernel_state(
                    &cfg,
                    &kernel_client,
                    &kernel_services,
                    &kernel_policies,
                    guard_service_interfaces,
                    incremental,
                )
            },
            || {
                // A prior failed/crashed attempt may have left guards armed.
                // Release them only after complete kernel activation succeeds.
                firewall::clear_fail_closed_lock()?;
                firewall_atomic::clear_transition_sets()
            },
        )
    })
    .await
    .map_err(|error| AppError::internal(format!("WireGuard sync task failed: {error}")))?;
    if let Err(error) = applied {
        tracing::warn!(
            %error,
            quarantined_peers = quarantine.peers.len(),
            quarantined_endpoints = quarantine.blocks.len(),
            "ad_vpn: kernel reconciliation failed"
        );
        return Err(AppError::internal(format!(
            "failed to synchronize WireGuard peers: {error}"
        )));
    }
    *APPLIED_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(desired);
    SYNC_DIRTY.store(false, std::sync::atomic::Ordering::Release);
    tracing::info!(
        peers = count,
        incremental,
        quarantined_peers = quarantine.peers.len(),
        quarantined_endpoints = quarantine.blocks.len(),
        "ad_vpn: wg0 hub reconciled"
    );
    Ok(())
}

async fn publish_cycle_route_before_activation<F, Fut>(
    selected: i64,
    vpn_enabled: bool,
    reconcile: F,
) -> AppResult<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = AppResult<()>>,
{
    if selected > 0 && !vpn_enabled {
        return Err(AppError::conflict(
            "KotH champion cooldown requires the managed A&D VPN",
        ));
    }
    if vpn_enabled {
        reconcile().await?;
    }
    Ok(())
}

/// Publish the replacement endpoint into the VPN policy, then install and
/// verify every exact `(champion peer, replacement endpoint)` cooldown tuple.
///
/// Destroying the prior hill removes its endpoint from the kernel policy. The
/// replacement therefore needs a reconciliation even when this cycle has no
/// champion cooldown; otherwise the server-side readiness checker can pass
/// while every player remains blocked from the new container.
pub async fn enforce_cycle_cooldown(db: &DatabaseConnection, cycle_id: i64) -> AppResult<usize> {
    let selected: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "KothCycleCooldowns"
            WHERE cycle_id = $1 AND network_released_at IS NULL"#,
    )
    .bind(cycle_id)
    .fetch_one(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let vpn_enabled = enabled();
    publish_cycle_route_before_activation(selected, vpn_enabled, || ensure_hub_and_sync(db))
        .await?;
    if selected == 0 {
        return Ok(0);
    }
    let configured_client_cidr = client_cidr();
    let configured_service_cidrs = service_route_cidrs().map_err(AppError::internal)?;
    let (client_network, service_networks) =
        validate_vpn_networks(&configured_client_cidr, &configured_service_cidrs)
            .map_err(AppError::internal)?;
    let required =
        cooldown::load_cycle_blocks(db, cycle_id, &client_network, &service_networks).await?;
    if i64::try_from(required.len()).unwrap_or(i64::MAX) != selected {
        return Err(AppError::conflict(format!(
            "KotH cooldown cycle {cycle_id} does not have an enforceable tuple for every selected champion"
        )));
    }
    if !super::owns_instance_lease() {
        return Ok(required.len());
    }
    let installed = APPLIED_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .is_some_and(|state| contains_required_cooldowns(state, &required));
    let kernel_required = required
        .iter()
        .map(|required| (required.game_id, required.block.clone()))
        .collect::<Vec<_>>();
    let kernel_installed = if installed {
        tokio::task::spawn_blocking(move || cooldown::blocks_installed(&kernel_required))
            .await
            .map_err(|error| {
                AppError::internal(format!("VPN cooldown audit task failed: {error}"))
            })?
            .map_err(AppError::internal)?
    } else {
        false
    };
    if !installed || !kernel_installed {
        SYNC_DIRTY.store(true, std::sync::atomic::Ordering::Release);
        return Err(AppError::conflict(format!(
            "KotH cooldown cycle {cycle_id} was not present in the activated kernel VPN policy"
        )));
    }
    Ok(required.len())
}

pub(super) fn retry_operation(
    attempts: usize,
    mut operation: impl FnMut() -> Result<(), String>,
    mut backoff: impl FnMut(usize),
) -> Result<(), String> {
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 1..=attempts {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::warn!(attempt, attempts, %error, "ad_vpn: WireGuard sync attempt failed");
                last_error = Some(error);
                if attempt < attempts {
                    backoff(attempt);
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "unknown WireGuard error".to_string()))
}

fn cidr_bits(cidr: &str) -> u8 {
    cidr.split('/')
        .nth(1)
        .and_then(|bits| bits.parse().ok())
        .unwrap_or(24)
}

#[cfg(test)]
#[path = "reconcile_tests.rs"]
mod tests;
