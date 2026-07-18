//! Detached ipset construction and atomic replacement of live VPN policy chains.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ipnet::Ipv4Net;

use super::firewall::{self, CooldownBlock, GameVpnPolicy, PolicySets, ServiceRoute};

static GENERATION_COUNTER: AtomicU64 = AtomicU64::new(0);

fn create_set_command(script: &mut String, name: &str, kind: &str) {
    writeln!(script, "create {name} {kind} family inet maxelem 131072")
        .expect("writing to a String cannot fail");
}

pub(super) fn ensure_transition_sets() -> Result<(), String> {
    let mut script = String::new();
    create_set_command(&mut script, firewall::QUARANTINE_SET, "hash:ip");
    create_set_command(
        &mut script,
        firewall::TRANSITION_BLOCK_SET,
        "hash:ip,port,ip",
    );
    firewall::ipset_restore(&script, true)
}

pub(super) fn quarantine_transition(
    peers: &[Ipv4Addr],
    blocks: &[CooldownBlock],
) -> Result<(), String> {
    ensure_transition_sets()?;
    let mut script = String::new();
    for peer in peers {
        writeln!(script, "add {} {peer}", firewall::QUARANTINE_SET)
            .expect("writing to a String cannot fail");
    }
    for block in blocks {
        writeln!(
            script,
            "add {} {},tcp:{},{}",
            firewall::TRANSITION_BLOCK_SET,
            block.peer,
            block.target.port,
            block.target.address
        )
        .expect("writing to a String cannot fail");
    }
    firewall::ipset_restore(&script, true)
}

pub(super) fn clear_transition_sets() -> Result<(), String> {
    ensure_transition_sets()?;
    firewall::ipset_restore(
        &format!(
            "flush {}\nflush {}\n",
            firewall::QUARANTINE_SET,
            firewall::TRANSITION_BLOCK_SET
        ),
        true,
    )
}

pub(super) fn apply_then_release_guards(
    apply: impl FnOnce() -> Result<(), String>,
    release: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    apply()?;
    release()
}

pub(super) fn dispatch_rule_plan(
    routes: &[ServiceRoute],
    guard_service_interfaces: bool,
) -> Vec<Vec<String>> {
    let mut rules = vec![
        vec![
            "-i".into(),
            firewall::IFNAME.into(),
            "-j".into(),
            firewall::FORWARD_CHAIN.into(),
        ],
        vec![
            "-o".into(),
            firewall::IFNAME.into(),
            "-j".into(),
            firewall::FORWARD_CHAIN.into(),
        ],
    ];
    if guard_service_interfaces {
        let mut guarded_interfaces = Vec::new();
        for interface in routes
            .iter()
            .filter(|route| route.directly_connected)
            .map(|route| route.interface.clone())
        {
            if !guarded_interfaces.contains(&interface) {
                rules.push(vec![
                    "-i".into(),
                    interface.clone(),
                    "-j".into(),
                    firewall::FORWARD_CHAIN.into(),
                ]);
                guarded_interfaces.push(interface);
            }
        }
    }
    rules.push(vec!["-j".into(), "RETURN".into()]);
    rules
}

pub(super) struct PreparedPolicy {
    pub(super) all_peers: String,
    pub(super) sets: Vec<PolicySets>,
    names: HashSet<String>,
    live: bool,
}

impl PreparedPolicy {
    pub(super) fn mark_live(&mut self) {
        self.live = true;
    }
}

impl Drop for PreparedPolicy {
    fn drop(&mut self) {
        if self.live {
            return;
        }
        // A partially installed generation may still be referenced and refuse
        // destruction. Retain only this generation first, so older failed
        // attempts cannot accumulate across retries; the next activation can
        // reclaim this one once it is no longer referenced.
        cleanup_stale(self);
        for name in &self.names {
            let _ = firewall::ipset(&["destroy", name]);
        }
    }
}

fn next_generation() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let sequence = GENERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:08x}", (elapsed ^ sequence.rotate_left(17)) as u32)
}

pub(super) fn prepare(
    client: &Ipv4Net,
    routes: &[ServiceRoute],
    policies: &[GameVpnPolicy],
) -> Result<PreparedPolicy, String> {
    let generation = next_generation();
    let all_peers = format!("rsv_a_{generation}");
    let sets = policies
        .iter()
        .map(|policy| PolicySets {
            peers: firewall::policy_set_name("p", policy.game_id, &generation),
            forward_targets: firewall::policy_set_name("f", policy.game_id, &generation),
            local_targets: firewall::policy_set_name("l", policy.game_id, &generation),
            nat_targets: firewall::policy_set_name("n", policy.game_id, &generation),
            cooldown_blocks: firewall::policy_set_name("c", policy.game_id, &generation),
        })
        .collect::<Vec<_>>();
    let mut names = HashSet::from([all_peers.clone()]);
    for sets in &sets {
        names.extend([
            sets.peers.clone(),
            sets.forward_targets.clone(),
            sets.local_targets.clone(),
            sets.nat_targets.clone(),
            sets.cooldown_blocks.clone(),
        ]);
    }
    let mut script = String::new();
    create_set_command(&mut script, &all_peers, "hash:ip");
    for (policy, sets) in policies.iter().zip(&sets) {
        create_set_command(&mut script, &sets.peers, "hash:ip");
        create_set_command(&mut script, &sets.forward_targets, "hash:ip,port");
        create_set_command(&mut script, &sets.local_targets, "hash:ip,port");
        create_set_command(&mut script, &sets.nat_targets, "hash:ip,port");
        create_set_command(&mut script, &sets.cooldown_blocks, "hash:ip,port,ip");
        for peer in &policy.peers {
            writeln!(script, "add {} {peer}", sets.peers).expect("writing to a String cannot fail");
            writeln!(script, "add {all_peers} {peer}").expect("writing to a String cannot fail");
        }
        for target in &policy.targets {
            if client.contains(&target.address) {
                writeln!(
                    script,
                    "add {} {},tcp:{}",
                    sets.forward_targets, target.address, target.port
                )
                .expect("writing to a String cannot fail");
                continue;
            }
            let Some(route) = routes
                .iter()
                .find(|route| route.network.contains(&target.address))
            else {
                continue;
            };
            let destination = if route.local_address == Some(target.address) {
                &sets.local_targets
            } else {
                writeln!(
                    script,
                    "add {} {},tcp:{}",
                    sets.nat_targets, target.address, target.port
                )
                .expect("writing to a String cannot fail");
                &sets.forward_targets
            };
            writeln!(
                script,
                "add {destination} {},tcp:{}",
                target.address, target.port
            )
            .expect("writing to a String cannot fail");
        }
        for block in &policy.cooldown_blocks {
            writeln!(
                script,
                "add {} {},tcp:{},{}",
                sets.cooldown_blocks, block.peer, block.target.port, block.target.address
            )
            .expect("writing to a String cannot fail");
        }
    }
    // Generation names are exclusive. Ignoring an unlikely collision could
    // merge stale members into a new allow policy, so detached restores fail.
    if let Err(error) = firewall::ipset_restore(&script, false) {
        for name in &names {
            let _ = firewall::ipset(&["destroy", name]);
        }
        return Err(error);
    }
    Ok(PreparedPolicy {
        all_peers,
        sets,
        names,
        live: false,
    })
}

fn append_chain(script: &mut String, chain: &str, rules: Vec<Vec<String>>) {
    script.push_str("-F ");
    script.push_str(chain);
    script.push('\n');
    for rule in rules {
        script.push_str("-A ");
        script.push_str(chain);
        script.push(' ');
        script.push_str(&rule.join(" "));
        script.push('\n');
    }
}

/// Replace all authorization chains at one filter-table commit. The old rules
/// remain active while detached sets are populated and until this commit lands.
pub(super) fn replace_live(
    policy: &mut PreparedPolicy,
    routes: &[ServiceRoute],
    guard_service_interfaces: bool,
) -> Result<(), String> {
    let mut filter = String::from("*filter\n");
    append_chain(
        &mut filter,
        firewall::INPUT_CHAIN,
        firewall::input_rule_plan(
            &policy.all_peers,
            &policy.sets,
            routes,
            guard_service_interfaces,
        ),
    );
    append_chain(
        &mut filter,
        firewall::FORWARD_CHAIN,
        firewall::forwarding_rule_plan(&policy.sets),
    );
    append_chain(
        &mut filter,
        firewall::FORWARD_DISPATCH_CHAIN,
        dispatch_rule_plan(routes, guard_service_interfaces),
    );
    filter.push_str("COMMIT\n");
    firewall::iptables_restore_noflush(&filter)?;

    let mut nat = String::from("*nat\n");
    append_chain(
        &mut nat,
        firewall::NAT_CHAIN,
        firewall::nat_rule_plan(&policy.sets),
    );
    nat.push_str("COMMIT\n");
    firewall::iptables_restore_noflush(&nat)?;
    policy.mark_live();
    Ok(())
}

fn owned_policy_set(name: &str) -> bool {
    ["rsv_a_", "rsv_p_", "rsv_f_", "rsv_l_", "rsv_n_", "rsv_c_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Old generations are no longer referenced after both table commits. Cleanup
/// is best-effort: a concurrently observed/busy set is safer to retain than to
/// turn a successful policy activation into an outage.
pub(super) fn cleanup_stale(policy: &PreparedPolicy) {
    let Ok(output) = Command::new("ipset").args(["list", "-name"]).output() else {
        return;
    };
    if !output.status.success() {
        return;
    }
    for name in String::from_utf8_lossy(&output.stdout).lines() {
        if owned_policy_set(name) && !policy.names.contains(name) {
            let _ = firewall::ipset(&["destroy", name]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_owned_generation_sets() {
        assert!(owned_policy_set("rsv_p_10_deadbeef"));
        assert!(owned_policy_set("rsv_a_deadbeef"));
        assert!(!owned_policy_set("rsv_quarantine"));
        assert!(!owned_policy_set("rsv_transition"));
        assert!(!owned_policy_set("application_set"));
    }

    #[test]
    fn generations_fit_kernel_set_name_limit() {
        let generation = next_generation();
        let longest = format!("rsv_c_{}_{}", i32::MIN.unsigned_abs(), generation);
        assert!(longest.len() <= 31);
    }

    #[test]
    fn failed_activation_does_not_release_transition_guards() {
        let released = std::cell::Cell::new(false);
        let result = apply_then_release_guards(
            || Err("activation failed".to_string()),
            || {
                released.set(true);
                Ok(())
            },
        );
        assert_eq!(result, Err("activation failed".to_string()));
        assert!(!released.get());
    }

    #[test]
    fn successful_activation_releases_transition_guards_once() {
        let releases = std::cell::Cell::new(0);
        apply_then_release_guards(
            || Ok(()),
            || {
                releases.set(releases.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(releases.get(), 1);
    }
}
