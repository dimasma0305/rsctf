//! services/anti_cheat.rs — ported from RSCTF
//! `Controllers/AccountController.External.cs :: CheckAntiCheatConflictAsync`.
//!
//! The shared IP / browser-fingerprint login gate. When the admin
//! `AccountPolicy.RequireUnique*` flags are set, a login (password or OAuth) is
//! blocked when its client IP or submitted browser fingerprint collides with
//! another account that was active in the last 24h — either any account
//! (`*Global`) or only a teammate (`*PerTeamUser`). Each block is persisted as an
//! [`anti_cheat_block`] row for the admin audit trail.
//!
//! The algorithm mirrors the C# ground truth exactly:
//!
//! * `ipCheck  = RequireUniqueIpPerTeamUser  || RequireUniqueIpGlobal`
//! * `fpCheck  = RequireUniqueFingerprintPerTeamUser || RequireUniqueFingerprintGlobal`
//! * neither ⇒ `Ok(None)` (fast path).
//! * `since = now − 24h`; `anyGlobal = RequireUniqueIpGlobal || RequireUniqueFingerprintGlobal`.
//! * candidates = users where `id != user.id AND last_visited_utc > since AND
//!   (anyGlobal OR shares a team with the user)`, each tagged `isTeammate`.
//! * IP conflict: candidate whose normalized IP == normalized current IP AND
//!   (`RequireUniqueIpGlobal || isTeammate`) ⇒ record + block (`kind = "Ip"`).
//! * Fingerprint conflict: candidate whose fingerprint == the submitted one AND
//!   (`RequireUniqueFingerprintGlobal || isTeammate`) ⇒ record + block.
//!
//! Note the two-flag split: the candidate **query** widens on `anyGlobal`, but
//! each **conflict** gates on its own per-kind `*Global` flag (never `anyGlobal`).

use std::collections::{BTreeMap, HashSet};

use axum::http::HeaderMap;
use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::models::data::{anti_cheat_block, config, team_member, user};
use crate::utils::error::AppResult;

/// The four `AccountPolicy.RequireUnique*` flags this gate depends on, loaded
/// from the DB-backed `Configs` table (not the static env config).
#[derive(Debug, Default, Clone, Copy)]
pub struct PolicyFlags {
    pub require_unique_ip_per_team_user: bool,
    pub require_unique_fingerprint_per_team_user: bool,
    pub require_unique_ip_global: bool,
    pub require_unique_fingerprint_global: bool,
}

/// Load the `RequireUnique*` flags from the flat `Configs` key/value table, the
/// same source (and lowercase-`bool::to_string()` convention) as
/// `admin.rs::get_config`. Missing keys default to `false`.
pub async fn load_policy_flags(db: &DatabaseConnection) -> AppResult<PolicyFlags> {
    let rows = config::Entity::find().all(db).await?;
    let map: BTreeMap<String, Option<String>> =
        rows.into_iter().map(|c| (c.config_key, c.value)).collect();
    let get_bool = |key: &str| {
        map.get(key)
            .cloned()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(false)
    };

    Ok(PolicyFlags {
        require_unique_ip_per_team_user: get_bool("AccountPolicy:RequireUniqueIpPerTeamUser"),
        require_unique_fingerprint_per_team_user: get_bool(
            "AccountPolicy:RequireUniqueFingerprintPerTeamUser",
        ),
        require_unique_ip_global: get_bool("AccountPolicy:RequireUniqueIpGlobal"),
        require_unique_fingerprint_global: get_bool("AccountPolicy:RequireUniqueFingerprintGlobal"),
    })
}

/// Run the anti-cheat login gate. Returns `Ok(Some(block))` (already persisted)
/// when the login must be denied, or `Ok(None)` when it may proceed. For OAuth,
/// pass `fingerprint = None` — only the IP checks then apply.
pub async fn check_login_conflict(
    db: &DatabaseConnection,
    policy: &PolicyFlags,
    user: &user::Model,
    current_ip: Option<&str>,
    fingerprint: Option<&str>,
) -> AppResult<Option<anti_cheat_block::Model>> {
    // IP and fingerprint uniqueness each have a per-team flag (conflict only with
    // a teammate) and a global flag (conflict with ANY other user in the last 24h).
    let ip_check = policy.require_unique_ip_per_team_user || policy.require_unique_ip_global;
    let fp_check =
        policy.require_unique_fingerprint_per_team_user || policy.require_unique_fingerprint_global;
    if !ip_check && !fp_check {
        return Ok(None);
    }

    let since = Utc::now() - Duration::hours(24);
    let any_global = policy.require_unique_ip_global || policy.require_unique_fingerprint_global;

    // Teammates: every other user who shares at least one team with `user`.
    let my_team_ids: Vec<i32> = team_member::Entity::find()
        .filter(team_member::Column::UserId.eq(user.id))
        .all(db)
        .await?
        .into_iter()
        .map(|m| m.team_id)
        .collect();

    let teammate_ids: HashSet<Uuid> = if my_team_ids.is_empty() {
        HashSet::new()
    } else {
        team_member::Entity::find()
            .filter(team_member::Column::TeamId.is_in(my_team_ids))
            .filter(team_member::Column::UserId.ne(user.id))
            .all(db)
            .await?
            .into_iter()
            .map(|m| m.user_id)
            .collect()
    };

    // Under a per-team-only policy the candidate filter reduces to "shares a team
    // with the user", so with no teammates there is nothing to conflict against.
    // Short-circuit (rather than emit `Id IN ()`, whose SQL is dialect-dependent).
    if !any_global && teammate_ids.is_empty() {
        return Ok(None);
    }

    // Candidate query: other users active in the window. Global checks scan every
    // such user; a per-team-only policy restricts the scan to teammates.
    let mut query = user::Entity::find()
        .filter(user::Column::Id.ne(user.id))
        .filter(user::Column::LastVisitedUtc.gt(since));
    if !any_global {
        let ids: Vec<Uuid> = teammate_ids.iter().copied().collect();
        query = query.filter(user::Column::Id.is_in(ids));
    }
    let candidates = query.all(db).await?;

    // Normalize the current IP once (fold ::ffff: IPv4-mapped, lowercase).
    let norm_ip = current_ip
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(parse_ip)
        .map(normalize_ip);

    // IP conflict first (matches the ground-truth ordering).
    if ip_check {
        if let Some(cur) = norm_ip.as_deref() {
            for c in &candidates {
                if c.ip.trim().is_empty() {
                    continue;
                }
                let is_teammate = teammate_ids.contains(&c.id);
                if parse_ip(c.ip.trim()).map(normalize_ip).as_deref() == Some(cur)
                    && (policy.require_unique_ip_global || is_teammate)
                {
                    return Ok(Some(
                        record(db, user, c, "Ip", Some(cur.to_string())).await?,
                    ));
                }
            }
        }
    }

    // Fingerprint conflict.
    if fp_check {
        if let Some(fp) = fingerprint.map(str::trim).filter(|s| !s.is_empty()) {
            for c in &candidates {
                let is_teammate = teammate_ids.contains(&c.id);
                if c.browser_fingerprint.as_deref() == Some(fp)
                    && (policy.require_unique_fingerprint_global || is_teammate)
                {
                    return Ok(Some(
                        record(db, user, c, "Fingerprint", Some(fp.to_string())).await?,
                    ));
                }
            }
        }
    }

    Ok(None)
}

/// A human-readable 403 message for a recorded block, mirroring RSCTF's
/// `Account_TeammateIpInUse` / `Account_TeammateFingerprintInUse`.
pub fn block_message(block: &anti_cheat_block::Model) -> String {
    let who = block
        .conflict_user_name
        .as_deref()
        .unwrap_or("another user");
    match block.kind.as_str() {
        "Ip" => format!("This IP is already in use by teammate {who}"),
        _ => format!("This fingerprint is already in use by teammate {who}"),
    }
}

/// Persist one [`anti_cheat_block`] row describing the denied login and return it.
async fn record(
    db: &DatabaseConnection,
    user: &user::Model,
    conflict: &user::Model,
    kind: &str,
    conflicting_value: Option<String>,
) -> AppResult<anti_cheat_block::Model> {
    let am = anti_cheat_block::ActiveModel {
        user_id: Set(user.id),
        user_name: Set(user.user_name.clone()),
        conflict_user_id: Set(Some(conflict.id)),
        conflict_user_name: Set(conflict.user_name.clone()),
        kind: Set(kind.to_string()),
        conflicting_value: Set(conflicting_value),
        occurred_at_utc: Set(Utc::now()),
        ..Default::default()
    };
    Ok(am.insert(db).await?)
}

/// Best-effort client IP from sources a client cannot forge past a trusted
/// reverse proxy: `X-Real-IP`, else the **rightmost** `X-Forwarded-For` hop.
/// Untrusted or direct connections always resolve to their socket peer.
pub fn client_ip(headers: &HeaderMap, peer: Option<std::net::IpAddr>) -> Option<String> {
    client_ip_with_trust(headers, peer, peer.is_some_and(is_trusted_proxy))
}

fn client_ip_with_trust(
    headers: &HeaderMap,
    peer: Option<std::net::IpAddr>,
    trust_forwarded: bool,
) -> Option<String> {
    if trust_forwarded {
        if let Some(real) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            if let Some(ip) = parse_ip(real) {
                return Some(normalize_ip(ip));
            }
        }
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(ip) = xff.split(',').map(str::trim).rev().find_map(parse_ip) {
                return Some(normalize_ip(ip));
            }
        }
    }
    // Direct connection (no trusted proxy header): fall back to the socket peer,
    // matching RSCTF's `HttpContext.Connection.RemoteIpAddress`.
    peer.map(normalize_ip)
}

fn parse_ip(value: &str) -> Option<std::net::IpAddr> {
    value.trim().parse().ok()
}

pub fn normalize_ip(ip: std::net::IpAddr) -> String {
    match ip {
        std::net::IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(std::net::IpAddr::V4)
            .unwrap_or(std::net::IpAddr::V6(v6))
            .to_string(),
        std::net::IpAddr::V4(v4) => v4.to_string(),
    }
}

fn parse_trusted_proxy_cidrs(value: &str) -> anyhow::Result<Vec<ipnet::IpNet>> {
    value
        .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<ipnet::IpNet>()
                .map_err(|_| anyhow::anyhow!("invalid trusted proxy CIDR: {value}"))
        })
        .collect()
}

fn trusted_proxy_networks() -> &'static [ipnet::IpNet] {
    static NETWORKS: std::sync::LazyLock<Vec<ipnet::IpNet>> = std::sync::LazyLock::new(|| {
        let configured = std::env::var("RSCTF_TRUSTED_PROXY_CIDRS").unwrap_or_default();
        parse_trusted_proxy_cidrs(&configured).unwrap_or_else(|error| {
            tracing::error!(%error, "ignoring invalid trusted-proxy configuration");
            Vec::new()
        })
    });
    &NETWORKS
}

/// Validate the environment configuration before the HTTP listener starts.
pub fn validate_trusted_proxy_config() -> anyhow::Result<()> {
    let configured = std::env::var("RSCTF_TRUSTED_PROXY_CIDRS").unwrap_or_default();
    parse_trusted_proxy_cidrs(&configured).map(|_| ())
}

/// Canonical CIDRs used by request resolution and the admin diagnostic.
pub fn configured_trusted_proxy_cidrs() -> Vec<String> {
    trusted_proxy_networks()
        .iter()
        .map(ToString::to_string)
        .collect()
}

pub fn is_trusted_proxy(peer: std::net::IpAddr) -> bool {
    trusted_proxy_networks()
        .iter()
        .any(|network| network.contains(&peer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn untrusted_peer_cannot_spoof_forwarded_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.9"));
        let peer = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));

        assert_eq!(
            client_ip_with_trust(&headers, Some(peer), false).as_deref(),
            Some("198.51.100.7")
        );
        assert_eq!(
            client_ip_with_trust(&headers, Some(peer), true).as_deref(),
            Some("203.0.113.9")
        );
    }

    #[test]
    fn malformed_forwarded_values_fall_back_to_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("not-an-ip"));
        let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert_eq!(
            client_ip_with_trust(&headers, Some(peer), true).as_deref(),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn trusted_proxy_parser_is_explicit_and_strict() {
        assert!(parse_trusted_proxy_cidrs("").unwrap().is_empty());
        let networks = parse_trusted_proxy_cidrs("192.0.2.10/32, 2001:db8::1/128").unwrap();
        assert_eq!(networks.len(), 2);
        assert!(networks[0].contains(&"192.0.2.10".parse::<IpAddr>().unwrap()));
        assert!(!networks[0].contains(&"192.0.2.11".parse::<IpAddr>().unwrap()));
        assert!(parse_trusted_proxy_cidrs("192.0.2.10").is_err());
    }
}
