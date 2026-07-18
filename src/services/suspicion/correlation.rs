//! services/suspicion/correlation.rs — IP / fingerprint correlation detectors.
//!
//! Ported from RSCTF `Controllers/CheatReportController.cs` (the IP-analysis
//! block, checks 2 / 2c / 2d / F / G / I). RSCTF builds these as transient
//! `report.IpAnalysis` rows returned to the admin; rsctf instead persists them as
//! `suspicion_event` rows keyed by participation (the same adaptation
//! [`super::detectors::correlate_fingerprints`] already makes for
//! `SharedFingerprint`), reusing the exact dedup + insert + score-bump path in
//! [`super::detectors::record_with_dedup`]. Each aggregate rule uses a stable
//! global evidence key, so concurrent and repeated sweeps are idempotent.
//!
//! ## Data source
//! RSCTF reads `Logs` rows where `Logger.Contains("AccountController")`, carrying
//! `RemoteIP` + `BrowserFingerprint` + `UserName`. rsctf's login path writes
//! exactly that row via [`crate::services::audit::info_with_fingerprint`]
//! (`logger = "AccountController"`, `remote_ip = client_ip`, `browser_fingerprint`,
//! `user_name`), so the port is faithful. The separate `logger = "fingerprint"`
//! row is deliberately ignored (it duplicates the fingerprint into `message` and
//! would double-count).
//!
//! ## Coverage vs RSCTF
//! Implemented (login-log-tractable): FingerprintChurn (2c), IpChurn (2d),
//! CrossTeamIP (2 — the login-IP-shared-across-teams variant, matching this
//! subsystem's definition), SubnetOverlap (G, /28), ClusteredRegistration (F),
//! SessionConcurrency (I).
//!
//! **Gap — UnknownIP (not emitted).** RSCTF's UnknownIP compares a *Download
//! `GameEvent` IP* against the team's login-IP baseline. rsctf persists no
//! download/container game-event IP stream, and `submission` rows carry no IP
//! column, so from the login logs alone every observed IP is by definition
//! already "seen" — there is no second stream to diff against. Rather than invent
//! a heuristic that fires on nothing meaningful, this detector is intentionally
//! left unimplemented until a per-request IP capture (download/container events)
//! lands. See the note at [`unknown_ip_gap`].

use super::*;
use uuid::Uuid;

// ── Thresholds (verbatim RSCTF constants) ────────────────────────────────────
/// `FingerprintChurnThreshold` — distinct fingerprints for one user before
/// [`SuspicionType::FingerprintChurn`] fires.
const FINGERPRINT_CHURN_THRESHOLD: usize = 4;
/// `IpChurnThreshold` — distinct (non-`Any`) IPs for one user before
/// [`SuspicionType::IpChurn`] fires.
const IP_CHURN_THRESHOLD: usize = 4;
/// `SessionWindowMinutes` — pairwise login window for SessionConcurrency.
const SESSION_WINDOW_MINUTES: i64 = 10;
/// `SessionConcurrencyMinOccurrences` — qualifying pairs required to fire.
const SESSION_CONCURRENCY_MIN_OCCURRENCES: usize = 3;
/// Registration-clustering window (`TimeSpan.FromHours(48)`).
const CLUSTERED_REGISTRATION_MAX_HOURS: i64 = 48;
/// Shared-network suppression: a shared IP/subnet touching more than this many
/// distinct teams is treated as benign campus/CGNAT (RSCTF `distinctTeams <= 4`).
const SHARED_NETWORK_MAX_TEAMS: usize = 4;

/// A shared address is useful review context only while it identifies a small
/// group. Larger groups are normal for event NAT, campus networks, and load-test
/// gateways; persisting one event per team would create noise without adding
/// attribution value.
fn reviewable_shared_network(team_count: usize) -> bool {
    (2..=SHARED_NETWORK_MAX_TEAMS).contains(&team_count)
}

/// Run the login-log IP/fingerprint correlation detectors for one game and
/// persist a `suspicion_event` per fired signal (deduped per participation).
///
/// See the module docs for the RSCTF mapping and the UnknownIP gap.
pub async fn run_correlation_checks(db: &DatabaseConnection, game_id: i32) -> AppResult<()> {
    use crate::models::data::{game, log_entry, team_member, user};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    // 1. Analysis window: [start, end]. Practice games have no fixed end, so use
    //    "now" (RSCTF `game.PracticeMode ? UtcNow : EndTimeUtc`).
    let Some(g) = game::Entity::find_by_id(game_id).one(db).await? else {
        return Ok(());
    };
    let start = g.start_time_utc;
    let end = if g.practice_mode {
        chrono::Utc::now()
    } else {
        g.end_time_utc
    };

    // 2. Participations in this game → team_id ⇒ participation_id. A team plays a
    //    game through exactly one participation; correlation events for a team are
    //    recorded on that participation.
    let parts = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .all(db)
        .await?;
    if parts.is_empty() {
        return Ok(());
    }
    let mut part_of_team: HashMap<i32, i32> = HashMap::new();
    for p in &parts {
        part_of_team.entry(p.team_id).or_insert(p.id);
    }
    let game_team_ids: Vec<i32> = part_of_team.keys().copied().collect();

    // 3. Game-scoped roster: user → team. Only members of a team participating in
    //    this game are considered (RSCTF's `userTeamMap` is game-only); a user
    //    with no in-game participation is dropped. First team wins on the rare
    //    multi-team membership (RSCTF `.First()`).
    let members = team_member::Entity::find()
        .filter(team_member::Column::TeamId.is_in(game_team_ids.clone()))
        .all(db)
        .await?;
    let mut team_of_uid: HashMap<Uuid, i32> = HashMap::new();
    for m in &members {
        team_of_uid.entry(m.user_id).or_insert(m.team_id);
    }
    let roster_uids: Vec<Uuid> = team_of_uid.keys().copied().collect();
    let roster_users = if roster_uids.is_empty() {
        Vec::new()
    } else {
        user::Entity::find()
            .filter(user::Column::Id.is_in(roster_uids))
            .all(db)
            .await?
    };
    // user_name → team_id, plus the per-user last-login IP + registration time.
    let mut team_of_name: HashMap<String, i32> = HashMap::new();
    let mut reg_time_of_name: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut user_ip_of_name: HashMap<String, String> = HashMap::new();
    for u in &roster_users {
        let Some(name) = u.user_name.clone() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let Some(&tid) = team_of_uid.get(&u.id) else {
            continue;
        };
        team_of_name.insert(name.clone(), tid);
        reg_time_of_name.insert(name.clone(), u.register_time_utc);
        // `user.ip` = RSCTF `UserInfo.IP` (last-login IP), unioned into teamIps.
        let ip = norm_ip(&u.ip);
        if !ip.is_empty() && !is_any_ip(&ip) {
            user_ip_of_name.insert(name, ip);
        }
    }

    // 4. AccountController login logs in the window (the only row written on every
    //    login; carries remote_ip + browser_fingerprint). Restricted to rostered
    //    users so off-roster / admin logins never implicate a team.
    let logs = log_entry::Entity::find()
        .filter(log_entry::Column::Logger.contains("AccountController"))
        .filter(log_entry::Column::TimeUtc.gte(start))
        .filter(log_entry::Column::TimeUtc.lte(end))
        .all(db)
        .await?;

    // Per-user aggregates (game-scoped).
    let mut fps_of_user: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut ips_of_user: HashMap<String, BTreeSet<String>> = HashMap::new();
    // Time-ordered (time, ip) sessions per user, for SessionConcurrency.
    let mut sessions_of_user: HashMap<String, Vec<(chrono::DateTime<chrono::Utc>, String)>> =
        HashMap::new();
    // Team → set of IPs (login IPs; user.ip is folded in below).
    let mut ips_of_team: HashMap<i32, BTreeSet<String>> = HashMap::new();

    for l in &logs {
        let Some(name) = l.user_name.clone() else {
            continue;
        };
        let Some(&tid) = team_of_name.get(&name) else {
            continue;
        };

        if let Some(fp) = l.browser_fingerprint.as_deref() {
            if !fp.is_empty() {
                fps_of_user
                    .entry(name.clone())
                    .or_default()
                    .insert(fp.to_string());
            }
        }

        if let Some(raw_ip) = l.remote_ip.as_deref() {
            let ip = norm_ip(raw_ip);
            if !ip.is_empty() && !is_any_ip(&ip) {
                ips_of_user
                    .entry(name.clone())
                    .or_default()
                    .insert(ip.clone());
                ips_of_team.entry(tid).or_default().insert(ip.clone());
                sessions_of_user
                    .entry(name.clone())
                    .or_default()
                    .push((l.time_utc, ip));
            }
        }
    }

    // Registration-clustering IP baseline: each rostered user's *first-ever*
    // AccountController login IP (RSCTF's unwindowed `allTimeUserLogs` →
    // `firstLogIpPerUser`, deliberately NOT limited to the game window — users
    // typically register before the game starts). One earliest (time, ip) per user.
    let all_time_logs = log_entry::Entity::find()
        .filter(log_entry::Column::Logger.contains("AccountController"))
        .filter(log_entry::Column::RemoteIp.is_not_null())
        .filter(log_entry::Column::UserName.is_not_null())
        .all(db)
        .await?;
    let mut first_login: HashMap<String, (chrono::DateTime<chrono::Utc>, String)> = HashMap::new();
    for l in &all_time_logs {
        let Some(name) = l.user_name.clone() else {
            continue;
        };
        if !team_of_name.contains_key(&name) {
            continue;
        }
        let Some(raw_ip) = l.remote_ip.as_deref() else {
            continue;
        };
        let ip = norm_ip(raw_ip);
        if ip.is_empty() {
            continue;
        }
        first_login
            .entry(name)
            .and_modify(|(t, existing)| {
                if l.time_utc < *t {
                    *t = l.time_utc;
                    *existing = ip.clone();
                }
            })
            .or_insert((l.time_utc, ip));
    }

    // Fold each rostered user's last-login IP (`user.ip`) into its team's IP set —
    // RSCTF unions `member.IP` into `teamIps` (checks 2 / F / G).
    for (name, ip) in &user_ip_of_name {
        if let Some(&tid) = team_of_name.get(name) {
            ips_of_team.entry(tid).or_default().insert(ip.clone());
        }
    }

    // Scratch code vec; discarded (correlation events aren't returned as codes).
    let mut codes: Vec<i16> = Vec::new();

    // ── (2c) FingerprintChurn — one user, ≥4 distinct browser fingerprints. ──
    for (name, fps) in &fps_of_user {
        if fps.len() < FINGERPRINT_CHURN_THRESHOLD {
            continue;
        }
        if let Some(&tid) = team_of_name.get(name) {
            if let Some(&pid) = part_of_team.get(&tid) {
                super::detectors::record_with_dedup(
                    db,
                    game_id,
                    pid,
                    None,
                    SuspicionType::FingerprintChurn,
                    GLOBAL_EVIDENCE_KEY,
                    &mut codes,
                )
                .await?;
            }
        }
    }

    // ── (2d) IpChurn — one user, ≥4 distinct (non-Any) IPs. ──
    for (name, ips) in &ips_of_user {
        if ips.len() < IP_CHURN_THRESHOLD {
            continue;
        }
        if let Some(&tid) = team_of_name.get(name) {
            if let Some(&pid) = part_of_team.get(&tid) {
                super::detectors::record_with_dedup(
                    db,
                    game_id,
                    pid,
                    None,
                    SuspicionType::IpChurn,
                    GLOBAL_EVIDENCE_KEY,
                    &mut codes,
                )
                .await?;
            }
        }
    }

    // ── (2) CrossTeamIP — one IP observed for ≥2 distinct teams. ──
    // Reverse the team→IP map into IP→teams; any IP spanning multiple teams
    // implicates every team on it (RSCTF `ipToTeams` with distinctTeams > 1).
    {
        let mut teams_of_ip: BTreeMap<String, BTreeSet<i32>> = BTreeMap::new();
        for (tid, ips) in &ips_of_team {
            for ip in ips {
                teams_of_ip.entry(ip.clone()).or_default().insert(*tid);
            }
        }
        for teams in teams_of_ip.values() {
            if !reviewable_shared_network(teams.len()) {
                continue;
            }
            for tid in teams {
                if let Some(&pid) = part_of_team.get(tid) {
                    super::detectors::record_with_dedup(
                        db,
                        game_id,
                        pid,
                        None,
                        SuspicionType::CrossTeamIp,
                        GLOBAL_EVIDENCE_KEY,
                        &mut codes,
                    )
                    .await?;
                }
            }
        }
    }

    // ── (G) SubnetOverlap — teams sharing a /28, suppressing large shared NAT. ──
    {
        let mut teams_of_subnet: BTreeMap<String, BTreeSet<i32>> = BTreeMap::new();
        for (tid, ips) in &ips_of_team {
            let mut subnets: BTreeSet<String> = BTreeSet::new();
            for ip in ips {
                if let Some(s) = subnet28(ip) {
                    subnets.insert(s);
                }
            }
            for s in subnets {
                teams_of_subnet.entry(s).or_default().insert(*tid);
            }
        }
        for teams in teams_of_subnet.values() {
            if !reviewable_shared_network(teams.len()) {
                continue;
            }
            for tid in teams {
                if let Some(&pid) = part_of_team.get(tid) {
                    super::detectors::record_with_dedup(
                        db,
                        game_id,
                        pid,
                        None,
                        SuspicionType::SubnetOverlap,
                        GLOBAL_EVIDENCE_KEY,
                        &mut codes,
                    )
                    .await?;
                }
            }
        }
    }

    // ── (F) ClusteredRegistration — accounts from 2..=4 teams sharing a first-
    //    login IP, all registered within 48h. ──
    {
        // Group rostered users by their earliest-login IP.
        let mut users_on_ip: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, (_t, ip)) in &first_login {
            users_on_ip
                .entry(ip.clone())
                .or_default()
                .push(name.clone());
        }
        for names in users_on_ip.values() {
            // Distinct teams on this IP.
            let teams: BTreeSet<i32> = names
                .iter()
                .filter_map(|n| team_of_name.get(n).copied())
                .collect();
            if !reviewable_shared_network(teams.len()) {
                continue;
            }
            // Registration span across all members on this IP must be ≤ 48h.
            let reg_times: Vec<chrono::DateTime<chrono::Utc>> = names
                .iter()
                .filter_map(|n| reg_time_of_name.get(n).copied())
                .collect();
            let (Some(min), Some(max)) = (reg_times.iter().min(), reg_times.iter().max()) else {
                continue;
            };
            if (*max - *min) > chrono::Duration::hours(CLUSTERED_REGISTRATION_MAX_HOURS) {
                continue;
            }
            for tid in &teams {
                if let Some(&pid) = part_of_team.get(tid) {
                    super::detectors::record_with_dedup(
                        db,
                        game_id,
                        pid,
                        None,
                        SuspicionType::ClusteredRegistration,
                        GLOBAL_EVIDENCE_KEY,
                        &mut codes,
                    )
                    .await?;
                }
            }
        }
    }

    // ── (I) SessionConcurrency — one user seen from IPs in different /20 subnets
    //    within a 10-minute window, ≥3 qualifying pairs. ──
    {
        let window = chrono::Duration::minutes(SESSION_WINDOW_MINUTES);
        for (name, sessions) in &sessions_of_user {
            if sessions.len() < 2 {
                continue;
            }
            // Sessions are appended in log order; sort by time to honor RSCTF's
            // `OrderBy(Time)` (the `break` below relies on ascending time).
            let mut sess = sessions.clone();
            sess.sort_by_key(|session| session.0);

            let mut occurrences = 0usize;
            for si in 0..sess.len() {
                for sj in (si + 1)..sess.len() {
                    if (sess[sj].0 - sess[si].0) > window {
                        break; // sorted: no later sj is within the window either
                    }
                    let (ip1, ip2) = (&sess[si].1, &sess[sj].1);
                    if ip1 == ip2 {
                        continue;
                    }
                    // Same /20 = likely one ISP pool (mobile/DHCP churn): suppress.
                    if same_subnet20(ip1, ip2) {
                        continue;
                    }
                    occurrences += 1;
                }
            }

            if occurrences < SESSION_CONCURRENCY_MIN_OCCURRENCES {
                continue;
            }
            if let Some(&tid) = team_of_name.get(name) {
                if let Some(&pid) = part_of_team.get(&tid) {
                    super::detectors::record_with_dedup(
                        db,
                        game_id,
                        pid,
                        None,
                        SuspicionType::SessionConcurrency,
                        GLOBAL_EVIDENCE_KEY,
                        &mut codes,
                    )
                    .await?;
                }
            }
        }
    }

    // ── UnknownIP — not emitted; see [`unknown_ip_gap`] and the module docs. ──
    unknown_ip_gap();

    Ok(())
}

/// Documentation anchor for the unimplemented **UnknownIP** detector.
///
/// RSCTF fires UnknownIP when a **Download `GameEvent`** originates from an IP
/// outside the team's login-IP history. rsctf persists no download/container
/// game-event IP stream and `submission` rows carry no IP column, so there is no
/// event stream to diff against the login baseline — from the login logs alone
/// every IP is already "seen." Emitting nothing here is deliberate (the task
/// requires flagging genuine data gaps over inventing a signal). Wiring this up
/// requires capturing a per-download/-container request IP first.
#[inline]
fn unknown_ip_gap() {}

// ── IP helpers (ports of CheatReportController.NormIp / GetSubnet28 / SameSubnet20) ──

/// Canonicalize an IP for cross-source comparison: collapse an IPv4-mapped IPv6
/// address (`::ffff:1.2.3.4`) to its IPv4 form so a dual-stack login and a plain
/// IPv4 login compare equal (RSCTF `NormIp`). Trims surrounding whitespace.
fn norm_ip(ip: &str) -> String {
    let t = ip.trim();
    let lower = t.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("::ffff:") {
        // The mapped suffix is a dotted-quad IPv4 (no letters), so the lowercased
        // form is identical to the original.
        return rest.to_string();
    }
    t.to_string()
}

/// The all-zeros wildcard addresses RSCTF excludes (`IPAddress.Any` / `IPv6Any`).
fn is_any_ip(ip: &str) -> bool {
    matches!(ip, "0.0.0.0" | "::" | "::0")
}

/// `/28` subnet key for an IPv4 address (`GetSubnet28`): zero the low 4 bits of
/// the final octet. IPv4-only — returns `None` for anything that isn't a dotted
/// quad.
fn subnet28(ip: &str) -> Option<String> {
    let addr: std::net::Ipv4Addr = ip.parse().ok()?;
    let o = addr.octets();
    Some(format!("{}.{}.{}.{}/28", o[0], o[1], o[2], o[3] & 0xF0))
}

/// True when two IPv4 addresses share a `/20` (`SameSubnet20`): first 20 bits
/// equal (`b0`, `b1`, and the high nibble of `b2`). Non-IPv4 inputs are never
/// "same subnet".
fn same_subnet20(a: &str, b: &str) -> bool {
    match (
        a.parse::<std::net::Ipv4Addr>(),
        b.parse::<std::net::Ipv4Addr>(),
    ) {
        (Ok(x), Ok(y)) => {
            let (p, q) = (x.octets(), y.octets());
            p[0] == q[0] && p[1] == q[1] && (p[2] & 0xF0) == (q[2] & 0xF0)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::reviewable_shared_network;

    #[test]
    fn shared_network_context_suppresses_singletons_and_large_nat_groups() {
        assert!(!reviewable_shared_network(0));
        assert!(!reviewable_shared_network(1));
        assert!(reviewable_shared_network(2));
        assert!(reviewable_shared_network(4));
        assert!(!reviewable_shared_network(5));
        assert!(!reviewable_shared_network(100));
    }
}
