//! Fail-closed WireGuard INPUT/FORWARD/NAT reconciliation.

use std::io::Write;
use std::net::Ipv4Addr;
use std::process::{Command, Stdio};

use ipnet::Ipv4Net;

pub(super) const IFNAME: &str = "wg0";
pub(super) const FORWARD_DISPATCH_CHAIN: &str = "RSCTF_VPN_DISPATCH";
pub(super) const FORWARD_CHAIN: &str = "RSCTF_VPN_FORWARD";
pub(super) const INPUT_CHAIN: &str = "RSCTF_VPN_INPUT";
pub(super) const NAT_CHAIN: &str = "RSCTF_VPN_NAT";
const LOCK_INPUT_CHAIN: &str = "RSCTF_VPN_LOCK_IN";
const LOCK_FORWARD_CHAIN: &str = "RSCTF_VPN_LOCK_FWD";
pub(super) const QUARANTINE_SET: &str = "rsv_quarantine";
pub(super) const TRANSITION_BLOCK_SET: &str = "rsv_transition";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServiceRoute {
    pub(super) network: Ipv4Net,
    pub(super) interface: String,
    pub(super) directly_connected: bool,
    pub(super) local_address: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VpnTarget {
    pub address: Ipv4Addr,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CooldownBlock {
    pub peer: Ipv4Addr,
    pub target: VpnTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GameVpnPolicy {
    pub game_id: i32,
    pub peers: Vec<Ipv4Addr>,
    pub targets: Vec<VpnTarget>,
    pub cooldown_blocks: Vec<CooldownBlock>,
}

fn verify_ip_forwarding() -> Result<(), String> {
    let value = std::fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
        .map_err(|error| format!("read net.ipv4.ip_forward: {error}"))?;
    if value.trim() == "1" {
        Ok(())
    } else {
        Err("net.ipv4.ip_forward must be enabled by the deployment".to_string())
    }
}

fn command_error(program: &str, args: &[&str], output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    format!(
        "{} {} failed with {}{}",
        program,
        args.join(" "),
        output.status,
        if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        }
    )
}

fn iptables_output(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("iptables")
        .args(["-w", "5"])
        .args(args)
        .output()
        .map_err(|error| format!("execute iptables {}: {error}", args.join(" ")))
}

fn iptables(args: &[&str]) -> Result<(), String> {
    let output = iptables_output(args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("iptables", args, &output))
    }
}

pub(super) fn iptables_restore_noflush(script: &str) -> Result<(), String> {
    let mut child = Command::new("iptables-restore")
        .args(["-w", "5", "--noflush"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("execute iptables-restore: {error}"))?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "open iptables-restore stdin".to_string())?
        .write_all(script.as_bytes());
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for iptables-restore: {error}"))?;
    write_result.map_err(|error| format!("write iptables-restore rules: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(
            "iptables-restore",
            &["-w", "5", "--noflush"],
            &output,
        ))
    }
}

pub(super) fn ipset(args: &[&str]) -> Result<(), String> {
    let output = Command::new("ipset")
        .args(args)
        .output()
        .map_err(|error| format!("execute ipset {}: {error}", args.join(" ")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("ipset", args, &output))
    }
}

pub(super) fn ipset_restore(script: &str, ignore_existing: bool) -> Result<(), String> {
    let mut command = Command::new("ipset");
    command.arg("restore");
    if ignore_existing {
        command.arg("-exist");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("execute ipset restore: {error}"))?;
    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "open ipset restore stdin".to_string())?
        .write_all(script.as_bytes());
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for ipset restore: {error}"))?;
    write_result.map_err(|error| format!("write ipset restore commands: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let args = if ignore_existing {
            &["restore", "-exist"][..]
        } else {
            &["restore"][..]
        };
        Err(command_error("ipset", args, &output))
    }
}

/// `iptables -C` and `-S <chain>` use exit 1 for a clean "not found" result;
/// execution/syntax/permission failures use a different status and must not be
/// mistaken for an absent rule.
fn iptables_exists(args: &[&str]) -> Result<bool, String> {
    let output = iptables_output(args)?;
    if output.status.success() {
        Ok(true)
    } else if output.status.code() == Some(1) {
        Ok(false)
    } else {
        Err(command_error("iptables", args, &output))
    }
}

fn ensure_chain(table: &str, chain: &str) -> Result<(), String> {
    if !iptables_exists(&["-t", table, "-S", chain])? {
        iptables(&["-t", table, "-N", chain])?;
    }
    iptables(&["-t", table, "-F", chain])
}

fn append_rule(table: &str, chain: &str, rule: &[String]) -> Result<(), String> {
    let mut args = vec!["-t", table, "-A", chain];
    args.extend(rule.iter().map(String::as_str));
    iptables(&args)
}

fn remove_all_rules(table: &str, chain: &str, rule: &[&str]) -> Result<(), String> {
    loop {
        let mut check = vec!["-t", table, "-C", chain];
        check.extend_from_slice(rule);
        if !iptables_exists(&check)? {
            return Ok(());
        }
        let mut delete = vec!["-t", table, "-D", chain];
        delete.extend_from_slice(rule);
        iptables(&delete)?;
    }
}

fn insert_rule(table: &str, chain: &str, rule: &[&str]) -> Result<(), String> {
    let mut args = vec!["-t", table, "-I", chain, "1"];
    args.extend_from_slice(rule);
    iptables(&args)
}

fn insert_rule_at(table: &str, chain: &str, position: usize, rule: &[&str]) -> Result<(), String> {
    let position = position.to_string();
    let mut args = vec!["-t", table, "-I", chain, position.as_str()];
    args.extend_from_slice(rule);
    iptables(&args)
}

fn ensure_chain_exists(table: &str, chain: &str) -> Result<(), String> {
    if !iptables_exists(&["-t", table, "-S", chain])? {
        iptables(&["-t", table, "-N", chain])?;
    }
    Ok(())
}

fn ensure_lock_rule(chain: &str, rule: &[&str]) -> Result<(), String> {
    let mut check = vec!["-t", "filter", "-C", chain];
    check.extend_from_slice(rule);
    if !iptables_exists(&check)? {
        let mut insert = vec!["-t", "filter", "-I", chain, "1"];
        insert.extend_from_slice(rule);
        iptables(&insert)?;
    }
    Ok(())
}

fn ensure_lock_chains(service_interfaces: &[String]) -> Result<(), String> {
    ensure_chain_exists("filter", LOCK_INPUT_CHAIN)?;
    ensure_chain_exists("filter", LOCK_FORWARD_CHAIN)?;
    if !iptables_exists(&["-t", "filter", "-C", LOCK_INPUT_CHAIN, "-j", "RETURN"])? {
        iptables(&["-t", "filter", "-A", LOCK_INPUT_CHAIN, "-j", "RETURN"])?;
    }
    if !iptables_exists(&["-t", "filter", "-C", LOCK_FORWARD_CHAIN, "-j", "RETURN"])? {
        iptables(&["-t", "filter", "-A", LOCK_FORWARD_CHAIN, "-j", "RETURN"])?;
    }
    ensure_lock_rule(LOCK_INPUT_CHAIN, &["-i", IFNAME, "-j", "DROP"])?;
    ensure_lock_rule(LOCK_FORWARD_CHAIN, &["-i", IFNAME, "-j", "DROP"])?;
    ensure_lock_rule(LOCK_FORWARD_CHAIN, &["-o", IFNAME, "-j", "DROP"])?;
    for interface in service_interfaces {
        ensure_lock_rule(LOCK_INPUT_CHAIN, &["-i", interface.as_str(), "-j", "DROP"])?;
        ensure_lock_rule(
            LOCK_FORWARD_CHAIN,
            &["-i", interface.as_str(), "-j", "DROP"],
        )?;
    }
    Ok(())
}

pub(super) struct FirewallLock {
    armed: bool,
}

impl FirewallLock {
    fn inactive() -> Self {
        Self { armed: false }
    }

    fn reassert(&self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        // Install both fail-closed hooks in one restore transaction.
        iptables_restore_noflush(&format!(
            "*filter\n-I INPUT 1 -j {LOCK_INPUT_CHAIN}\n-I FORWARD 1 -j {LOCK_FORWARD_CHAIN}\nCOMMIT\n"
        ))
    }

    pub(super) fn unlock(self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        clear_fail_closed_lock()
    }
}

/// Idempotently remove fail-closed hooks left by an earlier failed owner.
pub(super) fn clear_fail_closed_lock() -> Result<(), String> {
    remove_all_rules("filter", "INPUT", &["-j", LOCK_INPUT_CHAIN])?;
    remove_all_rules("filter", "FORWARD", &["-j", LOCK_FORWARD_CHAIN])?;
    Ok(())
}

fn lock_vpn_traffic(
    routes: &[ServiceRoute],
    guard_service_interfaces: bool,
) -> Result<FirewallLock, String> {
    let mut service_interfaces = Vec::new();
    if guard_service_interfaces {
        for interface in routes
            .iter()
            .filter(|route| route.directly_connected)
            .map(|route| route.interface.clone())
        {
            if service_interfaces.contains(&interface) {
                continue;
            }
            service_interfaces.push(interface);
        }
    }
    ensure_lock_chains(&service_interfaces)?;
    let firewall_lock = FirewallLock { armed: true };
    firewall_lock.reassert()?;
    Ok(firewall_lock)
}

pub(super) fn lock_existing_vpn() -> Result<FirewallLock, String> {
    ensure_lock_chains(&[])?;
    let firewall_lock = FirewallLock { armed: true };
    firewall_lock.reassert()?;
    Ok(firewall_lock)
}

fn route_probe(network: &Ipv4Net) -> Ipv4Addr {
    if network.prefix_len() == 32 {
        network.network()
    } else {
        Ipv4Addr::from(u32::from(network.network()).saturating_add(1))
    }
}

fn parse_route_interface(output: &str) -> Option<String> {
    let words: Vec<&str> = output.split_whitespace().collect();
    let interface = words
        .windows(2)
        .find_map(|window| (window[0] == "dev").then_some(window[1]))?
        .split('@')
        .next()?;
    if interface.is_empty()
        || interface.len() > 15
        || !interface
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    {
        return None;
    }
    Some(interface.to_string())
}

fn parse_route_source(output: &str) -> Option<Ipv4Addr> {
    let words: Vec<&str> = output.split_whitespace().collect();
    words.windows(2).find_map(|window| {
        (window[0] == "src")
            .then(|| window[1].parse().ok())
            .flatten()
    })
}

fn resolve_service_route(network: Ipv4Net) -> Result<ServiceRoute, String> {
    let probe = route_probe(&network).to_string();
    let get_args = ["-o", "-4", "route", "get", probe.as_str()];
    let get = Command::new("ip")
        .args(get_args)
        .output()
        .map_err(|error| format!("resolve route for {network}: {error}"))?;
    if !get.status.success() {
        return Err(command_error("ip", &get_args, &get));
    }
    let get_text = String::from_utf8_lossy(&get.stdout);
    let interface = parse_route_interface(&get_text)
        .ok_or_else(|| format!("route for {network} did not contain a safe interface"))?;
    if interface == IFNAME {
        return Err(format!(
            "A&D service CIDR {network} resolves back through {IFNAME}"
        ));
    }

    let network_text = network.to_string();
    let exact_args = ["-o", "-4", "route", "show", "exact", network_text.as_str()];
    let exact = Command::new("ip")
        .args(exact_args)
        .output()
        .map_err(|error| format!("inspect route for {network}: {error}"))?;
    if !exact.status.success() {
        return Err(command_error("ip", &exact_args, &exact));
    }
    let exact_text = String::from_utf8_lossy(&exact.stdout);
    let directly_connected = exact_text.lines().any(|line| {
        !line.split_whitespace().any(|word| word == "via")
            && parse_route_interface(line).as_deref() == Some(interface.as_str())
    });
    Ok(ServiceRoute {
        network,
        interface,
        directly_connected,
        local_address: parse_route_source(&get_text),
    })
}

pub(super) fn resolve_service_routes(
    services: &[Ipv4Net],
    require_direct: bool,
) -> Result<Vec<ServiceRoute>, String> {
    let routes = services
        .iter()
        .copied()
        .map(resolve_service_route)
        .collect::<Result<Vec<_>, _>>()?;
    if require_direct && routes.iter().any(|route| !route.directly_connected) {
        return Err(
            "the Docker A&D service CIDR must be directly connected to the rsctf container"
                .to_string(),
        );
    }
    if require_direct
        && routes
            .iter()
            .any(|route| route.local_address == Some(route_probe(&route.network)))
    {
        return Err(
            "the Docker A&D service route resolves to the bridge gateway; host networking is unsupported"
                .to_string(),
        );
    }
    Ok(routes)
}

pub(super) fn service_route_fingerprint(
    services: &[Ipv4Net],
    require_direct: bool,
) -> Result<String, String> {
    verify_ip_forwarding()?;
    let routes = resolve_service_routes(services, require_direct)?;
    Ok(routes
        .iter()
        .map(|route| {
            format!(
                "{}|{}|{}|{}",
                route.network,
                route.interface,
                route.directly_connected,
                route
                    .local_address
                    .map(|address| address.to_string())
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(";"))
}

fn owned_rules_are_prefix(
    table: &str,
    chain: &str,
    lock_rule: &str,
    expected: &[String],
) -> Result<bool, String> {
    let args = ["-t", table, "-S", chain];
    let output = iptables_output(&args)?;
    if !output.status.success() {
        return if output.status.code() == Some(1) {
            Ok(false)
        } else {
            Err(command_error("iptables", &args, &output))
        };
    }
    let prefix = format!("-A {chain} ");
    let mut remaining = expected.to_vec();
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with(&prefix))
    {
        if line == lock_rule {
            continue;
        }
        if let Some(index) = remaining.iter().position(|rule| rule == line) {
            remaining.remove(index);
            continue;
        }
        break;
    }
    Ok(remaining.is_empty())
}

pub(super) fn vpn_firewall_installed(
    client: &Ipv4Net,
    services: &[Ipv4Net],
    guard_service_interfaces: bool,
) -> Result<bool, String> {
    let required_chains = [
        ("filter", INPUT_CHAIN),
        ("filter", FORWARD_CHAIN),
        ("filter", FORWARD_DISPATCH_CHAIN),
        ("nat", NAT_CHAIN),
    ];
    for (table, chain) in required_chains {
        if !iptables_exists(&["-t", table, "-S", chain])? {
            return Ok(false);
        }
    }
    let routes = resolve_service_routes(services, guard_service_interfaces)?;
    let mut input_hooks = vec![format!("-A INPUT -i {IFNAME} -j {INPUT_CHAIN}")];
    if guard_service_interfaces {
        for interface in routes
            .iter()
            .filter(|route| route.directly_connected)
            .map(|route| route.interface.as_str())
        {
            let hook = format!("-A INPUT -i {interface} -j {INPUT_CHAIN}");
            if !input_hooks.contains(&hook) {
                input_hooks.push(hook);
            }
        }
    }
    if !owned_rules_are_prefix(
        "filter",
        "INPUT",
        &format!("-A INPUT -j {LOCK_INPUT_CHAIN}"),
        &input_hooks,
    )? || !owned_rules_are_prefix(
        "filter",
        "FORWARD",
        &format!("-A FORWARD -j {LOCK_FORWARD_CHAIN}"),
        &[format!("-A FORWARD -j {FORWARD_DISPATCH_CHAIN}")],
    )? {
        return Ok(false);
    }
    if !iptables_exists(&[
        "-t",
        "filter",
        "-C",
        "INPUT",
        "-i",
        IFNAME,
        "-j",
        INPUT_CHAIN,
    ])? || !iptables_exists(&[
        "-t",
        "filter",
        "-C",
        "FORWARD",
        "-j",
        FORWARD_DISPATCH_CHAIN,
    ])? || !iptables_exists(&[
        "-t",
        "filter",
        "-C",
        INPUT_CHAIN,
        "-i",
        IFNAME,
        "-m",
        "set",
        "--match-set",
        QUARANTINE_SET,
        "src",
        "-j",
        "DROP",
    ])? || !iptables_exists(&[
        "-t",
        "filter",
        "-C",
        FORWARD_CHAIN,
        "-i",
        IFNAME,
        "-m",
        "set",
        "--match-set",
        QUARANTINE_SET,
        "src",
        "-j",
        "DROP",
    ])? || !iptables_exists(&[
        "-t",
        "filter",
        "-C",
        INPUT_CHAIN,
        "-i",
        IFNAME,
        "-p",
        "tcp",
        "-m",
        "set",
        "--match-set",
        TRANSITION_BLOCK_SET,
        "src,dst,dst",
        "-j",
        "DROP",
    ])? || !iptables_exists(&[
        "-t",
        "filter",
        "-C",
        FORWARD_CHAIN,
        "-i",
        IFNAME,
        "-p",
        "tcp",
        "-m",
        "set",
        "--match-set",
        TRANSITION_BLOCK_SET,
        "src,dst,dst",
        "-j",
        "DROP",
    ])? || !iptables_exists(&["-t", "filter", "-C", INPUT_CHAIN, "-j", "DROP"])?
        || !iptables_exists(&["-t", "filter", "-C", FORWARD_CHAIN, "-j", "DROP"])?
        || !iptables_exists(&["-t", "nat", "-C", NAT_CHAIN, "-j", "RETURN"])?
    {
        return Ok(false);
    }
    let client = client.to_string();
    if !iptables_exists(&[
        "-t",
        "nat",
        "-C",
        "POSTROUTING",
        "-s",
        client.as_str(),
        "-j",
        NAT_CHAIN,
    ])? {
        return Ok(false);
    }
    for name in [
        QUARANTINE_SET,
        TRANSITION_BLOCK_SET,
        super::capture_policy::REQUIRED_SET,
        super::capture_policy::LIVE_SET,
    ] {
        let output = Command::new("ipset")
            .args(["list", name])
            .output()
            .map_err(|error| format!("inspect ipset {name}: {error}"))?;
        if output.status.code() == Some(1) {
            return Ok(false);
        }
        if !output.status.success() {
            return Err(command_error("ipset", &["list", name], &output));
        }
    }
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PolicySets {
    pub(super) peers: String,
    pub(super) forward_targets: String,
    pub(super) local_targets: String,
    pub(super) nat_targets: String,
    pub(super) cooldown_blocks: String,
}

pub(super) fn policy_set_name(kind: &str, game_id: i32, generation: &str) -> String {
    format!("rsv_{kind}_{}_{generation}", game_id.unsigned_abs())
}

pub(super) use super::firewall_rules::{forwarding_rule_plan, input_rule_plan, nat_rule_plan};

fn setup_input_guard(
    all_peers: &str,
    sets: &[PolicySets],
    routes: &[ServiceRoute],
    guard_service_interfaces: bool,
) -> Result<(), String> {
    ensure_chain("filter", INPUT_CHAIN)?;
    for rule in input_rule_plan(all_peers, sets, routes, guard_service_interfaces) {
        append_rule("filter", INPUT_CHAIN, &rule)?;
    }
    remove_all_rules("filter", "INPUT", &["-i", IFNAME, "-j", INPUT_CHAIN])?;
    insert_rule_at("filter", "INPUT", 2, &["-i", IFNAME, "-j", INPUT_CHAIN])?;
    if guard_service_interfaces {
        let mut interfaces = Vec::new();
        for interface in routes
            .iter()
            .filter(|route| route.directly_connected)
            .map(|route| route.interface.clone())
        {
            if interfaces.contains(&interface) {
                continue;
            }
            remove_all_rules(
                "filter",
                "INPUT",
                &["-i", interface.as_str(), "-j", INPUT_CHAIN],
            )?;
            insert_rule_at(
                "filter",
                "INPUT",
                2,
                &["-i", interface.as_str(), "-j", INPUT_CHAIN],
            )?;
            interfaces.push(interface);
        }
    }
    Ok(())
}

/// Install an owned, fail-closed VPN boundary before exposing any WireGuard
/// peers. Only client-to-client and client-to-configured-service forwarding is
/// permitted. The app, database, Redis, proxy peers, and default route remain
/// unreachable from the tunnel.
pub(super) fn setup_vpn_firewall(
    client: &Ipv4Net,
    services: &[Ipv4Net],
    policies: &[GameVpnPolicy],
    guard_service_interfaces: bool,
) -> Result<FirewallLock, String> {
    verify_ip_forwarding()?;
    let routes = resolve_service_routes(services, guard_service_interfaces)?;
    super::firewall_atomic::ensure_transition_sets()?;
    let mut policy = super::firewall_atomic::prepare(client, &routes, policies)?;
    if vpn_firewall_installed(client, services, guard_service_interfaces)? {
        super::firewall_atomic::replace_live(&mut policy, &routes, guard_service_interfaces)?;
        super::firewall_atomic::cleanup_stale(&policy);
        return Ok(FirewallLock::inactive());
    }
    let firewall_lock = lock_vpn_traffic(&routes, guard_service_interfaces)?;
    setup_input_guard(
        &policy.all_peers,
        &policy.sets,
        &routes,
        guard_service_interfaces,
    )?;

    ensure_chain("filter", FORWARD_CHAIN)?;
    for rule in forwarding_rule_plan(&policy.sets) {
        append_rule("filter", FORWARD_CHAIN, &rule)?;
    }

    ensure_chain("filter", FORWARD_DISPATCH_CHAIN)?;
    for rule in super::firewall_atomic::dispatch_rule_plan(&routes, guard_service_interfaces) {
        append_rule("filter", FORWARD_DISPATCH_CHAIN, &rule)?;
    }
    remove_all_rules("filter", "FORWARD", &["-j", FORWARD_DISPATCH_CHAIN])?;
    insert_rule_at("filter", "FORWARD", 2, &["-j", FORWARD_DISPATCH_CHAIN])?;

    ensure_chain("nat", NAT_CHAIN)?;
    for rule in nat_rule_plan(&policy.sets) {
        append_rule("nat", NAT_CHAIN, &rule)?;
    }

    let client_text = client.to_string();
    let mut legacy_sources = vec![
        client_text.clone(),
        "10.13.0.0/16".to_string(),
        "10.13.37.0/24".to_string(),
    ];
    legacy_sources.sort();
    legacy_sources.dedup();
    for source in legacy_sources {
        remove_all_rules(
            "nat",
            "POSTROUTING",
            &["-s", source.as_str(), "!", "-o", IFNAME, "-j", "MASQUERADE"],
        )?;
        remove_all_rules(
            "nat",
            "POSTROUTING",
            &["-s", source.as_str(), "-j", NAT_CHAIN],
        )?;
    }
    insert_rule(
        "nat",
        "POSTROUTING",
        &["-s", client_text.as_str(), "-j", NAT_CHAIN],
    )?;
    // Permanent hooks are inserted directly below the selective lock dispatcher.
    // Reassert the dispatcher at the top until WireGuard's peer replacement succeeds.
    firewall_lock.reassert()?;
    policy.mark_live();
    super::firewall_atomic::cleanup_stale(&policy);
    Ok(firewall_lock)
}

#[cfg(test)]
#[path = "firewall_tests.rs"]
mod tests;
