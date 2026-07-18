//! Shared fixed safety bounds for capture ownership and kernel admission.

use std::time::Duration;

pub(crate) const OWNER_LEASE_SECONDS: i32 = 12;
pub(crate) const KERNEL_LIVE_TIMEOUT_SECONDS: u32 = 15;
pub(crate) const KERNEL_FAIL_CLOSED_WAIT: Duration =
    Duration::from_secs(KERNEL_LIVE_TIMEOUT_SECONDS as u64 + 1);
pub(crate) const FULL_FAIL_CLOSED_WAIT: Duration =
    Duration::from_secs(OWNER_LEASE_SECONDS as u64 + KERNEL_LIVE_TIMEOUT_SECONDS as u64 + 1);
