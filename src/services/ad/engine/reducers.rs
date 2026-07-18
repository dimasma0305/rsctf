//! Operational checker-credit reducers.

use super::*;

/// Per-tick SLA credit for a fresh verdict given the previous verdict.
pub fn tick_credit(current: AdCheckStatus, previous: Option<AdCheckStatus>) -> f64 {
    match current {
        AdCheckStatus::Ok
            if matches!(
                previous,
                Some(AdCheckStatus::Offline) | Some(AdCheckStatus::Mumble)
            ) =>
        {
            SLA_CREDIT_RECOVERING
        }
        AdCheckStatus::Ok => SLA_CREDIT_OK,
        _ => SLA_CREDIT_NONE,
    }
}

/// Credit persisted with a check result for operational history.
pub fn stored_tick_credit(current: AdCheckStatus, previous: Option<AdCheckStatus>) -> f64 {
    tick_credit(current, previous)
}
