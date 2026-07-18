//! Pure iptables rule planning for the managed VPN chains.

use super::capture_policy::{LIVE_SET, REQUIRED_SET};
use super::firewall::{PolicySets, ServiceRoute, IFNAME, QUARANTINE_SET, TRANSITION_BLOCK_SET};

fn transition_block_rule() -> Vec<String> {
    vec![
        "-i".into(),
        IFNAME.into(),
        "-p".into(),
        "tcp".into(),
        "-m".into(),
        "set".into(),
        "--match-set".into(),
        TRANSITION_BLOCK_SET.into(),
        "src,dst,dst".into(),
        "-j".into(),
        "DROP".into(),
    ]
}

fn capture_gate(interface: &str, endpoint_direction: &str) -> Vec<String> {
    vec![
        interface.into(),
        IFNAME.into(),
        "-p".into(),
        "tcp".into(),
        "-m".into(),
        "set".into(),
        "--match-set".into(),
        REQUIRED_SET.into(),
        endpoint_direction.into(),
        "-m".into(),
        "set".into(),
        "!".into(),
        "--match-set".into(),
        LIVE_SET.into(),
        endpoint_direction.into(),
        "-j".into(),
        "DROP".into(),
    ]
}

pub(super) fn forwarding_rule_plan(sets: &[PolicySets]) -> Vec<Vec<String>> {
    let mut rules = vec![vec![
        "-m".into(),
        "conntrack".into(),
        "--ctstate".into(),
        "INVALID".into(),
        "-j".into(),
        "DROP".into(),
    ]];
    rules.push(vec![
        "-i".into(),
        IFNAME.into(),
        "-m".into(),
        "set".into(),
        "--match-set".into(),
        QUARANTINE_SET.into(),
        "src".into(),
        "-j".into(),
        "DROP".into(),
    ]);
    rules.push(transition_block_rule());
    rules.push(capture_gate("-i", "dst,dst"));
    rules.push(capture_gate("-o", "src,src"));
    for game in sets {
        rules.push(vec![
            "-i".into(),
            IFNAME.into(),
            "-p".into(),
            "tcp".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.cooldown_blocks.clone(),
            "src,dst,dst".into(),
            "-j".into(),
            "DROP".into(),
        ]);
        rules.push(vec![
            "-i".into(),
            IFNAME.into(),
            "-p".into(),
            "tcp".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.peers.clone(),
            "src".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.forward_targets.clone(),
            "dst,dst".into(),
            "-m".into(),
            "conntrack".into(),
            "--ctstate".into(),
            "NEW,ESTABLISHED".into(),
            "-j".into(),
            "ACCEPT".into(),
        ]);
        rules.push(vec![
            "-o".into(),
            IFNAME.into(),
            "-p".into(),
            "tcp".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.forward_targets.clone(),
            "src,src".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.peers.clone(),
            "dst".into(),
            "-m".into(),
            "conntrack".into(),
            "--ctstate".into(),
            "ESTABLISHED,RELATED".into(),
            "-j".into(),
            "ACCEPT".into(),
        ]);
    }
    rules.push(vec!["-j".into(), "DROP".into()]);
    rules
}

pub(super) fn input_rule_plan(
    all_peers: &str,
    sets: &[PolicySets],
    routes: &[ServiceRoute],
    guard_service_interfaces: bool,
) -> Vec<Vec<String>> {
    let mut rules = vec![
        vec![
            "-i".into(),
            IFNAME.into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            QUARANTINE_SET.into(),
            "src".into(),
            "-j".into(),
            "DROP".into(),
        ],
        transition_block_rule(),
        capture_gate("-i", "dst,dst"),
        vec![
            "-i".into(),
            IFNAME.into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            all_peers.into(),
            "src".into(),
            "-m".into(),
            "conntrack".into(),
            "--ctstate".into(),
            "ESTABLISHED,RELATED".into(),
            "-j".into(),
            "ACCEPT".into(),
        ],
    ];
    for game in sets {
        rules.push(vec![
            "-i".into(),
            IFNAME.into(),
            "-p".into(),
            "tcp".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.cooldown_blocks.clone(),
            "src,dst,dst".into(),
            "-j".into(),
            "DROP".into(),
        ]);
        rules.push(vec![
            "-i".into(),
            IFNAME.into(),
            "-p".into(),
            "tcp".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.peers.clone(),
            "src".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.local_targets.clone(),
            "dst,dst".into(),
            "-m".into(),
            "conntrack".into(),
            "--ctstate".into(),
            "NEW,ESTABLISHED".into(),
            "-j".into(),
            "ACCEPT".into(),
        ]);
    }
    if guard_service_interfaces {
        let mut interfaces = Vec::new();
        for interface in routes
            .iter()
            .filter(|route| route.directly_connected)
            .map(|route| route.interface.clone())
        {
            if !interfaces.contains(&interface) {
                rules.push(vec![
                    "-i".into(),
                    interface.clone(),
                    "-m".into(),
                    "conntrack".into(),
                    "--ctstate".into(),
                    "ESTABLISHED,RELATED".into(),
                    "-j".into(),
                    "ACCEPT".into(),
                ]);
                interfaces.push(interface);
            }
        }
    }
    rules.push(vec!["-j".into(), "DROP".into()]);
    rules
}

pub(super) fn nat_rule_plan(sets: &[PolicySets]) -> Vec<Vec<String>> {
    let mut rules = Vec::new();
    for game in sets {
        rules.push(vec![
            "-p".into(),
            "tcp".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.peers.clone(),
            "src".into(),
            "-m".into(),
            "set".into(),
            "--match-set".into(),
            game.nat_targets.clone(),
            "dst,dst".into(),
            "!".into(),
            "-o".into(),
            IFNAME.into(),
            "-j".into(),
            "MASQUERADE".into(),
        ]);
    }
    rules.push(vec!["-j".into(), "RETURN".into()]);
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_gate_covers_new_and_established_both_directions() {
        let rules = forwarding_rule_plan(&[])
            .into_iter()
            .map(|rule| rule.join(" "))
            .collect::<Vec<_>>();
        assert!(rules.iter().any(|rule| {
            rule.contains("-i wg0")
                && rule.contains("rsv_capture_required dst,dst")
                && rule.contains("! --match-set rsv_capture_live dst,dst")
        }));
        assert!(rules.iter().any(|rule| {
            rule.contains("-o wg0")
                && rule.contains("rsv_capture_required src,src")
                && rule.contains("! --match-set rsv_capture_live src,src")
        }));
    }
}
