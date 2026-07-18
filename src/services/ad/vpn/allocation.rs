use std::collections::HashSet;
use std::net::Ipv4Addr;

use super::parse_ipv4_network;

/// Deterministically allocate a /32 for a participation while probing past
/// collisions. Network, hub, and broadcast addresses are never returned.
#[cfg(test)]
pub(super) fn assign_deterministic_ip(cidr: &str, participation_id: i32) -> Option<String> {
    assign_available_ip(cidr, participation_id, &HashSet::new())
}

pub(super) fn assign_available_ip(
    cidr: &str,
    participation_id: i32,
    used: &HashSet<Ipv4Addr>,
) -> Option<String> {
    let network = parse_ipv4_network(cidr, "VPN client CIDR").ok()?;
    if network.prefix_len() > 30 {
        return None;
    }
    let host_count: u64 = 1u64 << (32 - network.prefix_len());
    let usable = host_count.checked_sub(3)?;
    if usable == 0 {
        return None;
    }
    let start = i64::from(participation_id).unsigned_abs() % usable;
    for offset in 0..usable {
        let index = 2 + ((start + offset) % usable);
        let address = Ipv4Addr::from(u32::from(network.network()).checked_add(index as u32)?);
        if !used.contains(&address) {
            return Some(address.to_string());
        }
    }
    None
}
