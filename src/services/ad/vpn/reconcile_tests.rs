use super::super::firewall::{CooldownBlock, VpnTarget};
use super::*;

fn policy(game_id: i32, peer: &str, target_port: u16) -> GameVpnPolicy {
    GameVpnPolicy {
        game_id,
        peers: vec![peer.parse().unwrap()],
        targets: vec![VpnTarget {
            address: "10.13.40.8".parse().unwrap(),
            port: target_port,
        }],
        cooldown_blocks: Vec::new(),
    }
}

fn state(policies: Vec<GameVpnPolicy>) -> AppliedState {
    AppliedState {
        fingerprint: [0; 32],
        hub_identity: [0; 32],
        peers: Vec::new(),
        policies,
    }
}

#[test]
fn target_removal_blocks_only_the_exact_peer_endpoint_pairs() {
    let previous = state(vec![policy(1, "10.13.0.2", 80), policy(2, "10.13.0.3", 80)]);
    let desired = state(vec![policy(1, "10.13.0.2", 81), policy(2, "10.13.0.3", 80)]);
    assert_eq!(
        transition_quarantine(&previous, &desired),
        TransitionQuarantine {
            peers: Vec::new(),
            blocks: vec![CooldownBlock {
                peer: "10.13.0.2".parse().unwrap(),
                target: VpnTarget {
                    address: "10.13.40.8".parse().unwrap(),
                    port: 80,
                },
            }],
        }
    );
}

#[test]
fn new_cooldown_blocks_only_the_champions_hill_endpoint() {
    let previous = state(vec![policy(1, "10.13.0.2", 80)]);
    let mut next = policy(1, "10.13.0.2", 80);
    let block = CooldownBlock {
        peer: "10.13.0.2".parse().unwrap(),
        target: next.targets[0].clone(),
    };
    next.cooldown_blocks.push(block.clone());
    assert_eq!(
        transition_quarantine(&previous, &state(vec![next])),
        TransitionQuarantine {
            peers: Vec::new(),
            blocks: vec![block],
        }
    );
}

#[tokio::test]
async fn replacement_route_is_published_without_a_cooldown_participant() {
    let syncs = std::cell::Cell::new(0);
    publish_cycle_route_before_activation(0, true, || async {
        syncs.set(syncs.get() + 1);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(syncs.get(), 1);

    publish_cycle_route_before_activation(0, false, || async {
        syncs.set(syncs.get() + 1);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(syncs.get(), 1);
}

#[tokio::test]
async fn replacement_route_failure_blocks_activation() {
    let result = publish_cycle_route_before_activation(0, true, || async {
        Err(AppError::internal("route publication failed"))
    })
    .await;
    assert!(result.is_err());

    let syncs = std::cell::Cell::new(0);
    let result = publish_cycle_route_before_activation(1, false, || async {
        syncs.set(syncs.get() + 1);
        Ok(())
    })
    .await;
    assert!(result.is_err());
    assert_eq!(syncs.get(), 0);
}

#[test]
fn cooldown_receipt_requires_peer_target_and_exact_block() {
    let peer = "10.13.37.7".parse().unwrap();
    let target = VpnTarget {
        address: "10.13.40.8".parse().unwrap(),
        port: 80,
    };
    let block = CooldownBlock {
        peer,
        target: target.clone(),
    };
    let required = vec![cooldown::RequiredCooldownBlock {
        cycle_id: 11,
        participation_id: 7,
        game_id: 1,
        block: block.clone(),
    }];
    let mut applied = state(vec![GameVpnPolicy {
        game_id: 1,
        peers: vec![peer],
        targets: vec![target],
        cooldown_blocks: vec![block],
    }]);
    assert!(!contains_required_cooldowns(&applied, &required));

    applied.peers.push(AppliedPeer {
        game_id: 1,
        public_key: "installed-key".to_string(),
        address: peer,
    });
    assert!(contains_required_cooldowns(&applied, &required));

    applied.policies[0].cooldown_blocks.clear();
    assert!(!contains_required_cooldowns(&applied, &required));
}

#[test]
fn peer_key_rotation_quarantines_only_rotated_address() {
    let mut previous = state(vec![policy(1, "10.13.0.2", 80)]);
    previous.peers.push(AppliedPeer {
        game_id: 1,
        public_key: "old".into(),
        address: "10.13.0.2".parse().unwrap(),
    });
    let mut desired = state(vec![policy(1, "10.13.0.2", 80)]);
    desired.peers.push(AppliedPeer {
        game_id: 1,
        public_key: "new".into(),
        address: "10.13.0.2".parse().unwrap(),
    });
    assert_eq!(
        transition_quarantine(&previous, &desired),
        TransitionQuarantine {
            peers: vec!["10.13.0.2".parse().unwrap()],
            blocks: Vec::new(),
        }
    );
}

#[test]
fn policy_revocation_source_quarantines_every_revoked_peer() {
    let previous = state(vec![GameVpnPolicy {
        game_id: 1,
        peers: vec!["10.13.0.2".parse().unwrap(), "10.13.0.3".parse().unwrap()],
        targets: vec![VpnTarget {
            address: "10.13.40.8".parse().unwrap(),
            port: 80,
        }],
        cooldown_blocks: Vec::new(),
    }]);
    assert_eq!(
        transition_quarantine(&previous, &state(Vec::new())),
        TransitionQuarantine {
            peers: vec!["10.13.0.2".parse().unwrap(), "10.13.0.3".parse().unwrap()],
            blocks: Vec::new(),
        }
    );
}

#[test]
fn kernel_clamped_private_key_has_the_same_hub_identity() {
    let mut raw = [0xaa; 32];
    raw[0] = 0xe2;
    raw[31] = 0xac;
    let desired = Key::new(raw);
    let mut clamped = raw;
    clamped[0] &= 248;
    clamped[31] &= 127;
    clamped[31] |= 64;
    let current = Key::new(clamped);
    let mut changed = clamped;
    changed[1] ^= 1;

    assert_ne!(current, desired);
    assert!(same_hub_key(Some(&current), &desired));
    assert!(!same_hub_key(Some(&Key::new(changed)), &desired));
    assert!(!same_hub_key(None, &desired));
}
