use uuid::Uuid;

use super::{
    is_manual_operation_conflict, should_ensure_network, should_provision_vpn, should_reconcile_vpn,
};

#[test]
fn only_a_definitive_manual_workload_conflict_rotates_the_operation() {
    let operation_id = Some(Uuid::new_v4());
    assert!(is_manual_operation_conflict(
        &crate::utils::error::AppError::conflict("changed launch specification"),
        operation_id
    ));
    assert!(!is_manual_operation_conflict(
        &crate::utils::error::AppError::unavailable("runtime unavailable"),
        operation_id
    ));
    assert!(!is_manual_operation_conflict(
        &crate::utils::error::AppError::conflict("automatic repair"),
        None
    ));
}

#[test]
fn round_repair_skips_byoc_vpn_reprovisioning() {
    let need_vpn = should_provision_vpn(true, true, true, false);
    assert!(!need_vpn);
    assert!(!should_reconcile_vpn(need_vpn, false));
}

#[test]
fn explicit_vpn_provisioning_still_configures_byoc() {
    let need_vpn = should_provision_vpn(true, true, true, true);
    assert!(need_vpn);
    assert!(should_reconcile_vpn(need_vpn, false));
}

#[test]
fn managed_service_repairs_still_reconcile_the_network() {
    let need_vpn = should_provision_vpn(true, true, true, false);
    assert!(!need_vpn);
    assert!(should_reconcile_vpn(need_vpn, true));
}

#[test]
fn paged_reconcile_has_one_network_topology_owner() {
    assert!(should_ensure_network(true, true));
    assert!(!should_ensure_network(true, false));
    assert!(!should_ensure_network(false, true));
}
