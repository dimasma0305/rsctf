//! Bounded global-honeypot telemetry handoff.
//!
//! Global HTTP/TCP baits cannot safely select one participation, so their
//! aggregate rows remain non-actionable. Admission, sampling, queue bounds,
//! and persistence live in `honeypot_telemetry`; this module keeps the stable
//! suspicion-service entry points without putting PostgreSQL on request tasks.

use crate::app_state::SharedState;
use crate::middlewares::privilege_authentication::CurrentUser;
use crate::services::honeypot_telemetry::HoneypotAdmission;
use crate::utils::error::AppResult;

/// Enqueue an admitted HTTP hit. Authentication is used only as optional
/// forensic context; no game or participation is inferred from a global bait.
pub(crate) fn record_honeypot_hit(
    st: &SharedState,
    user: Option<CurrentUser>,
    bait: &str,
    user_agent: Option<&str>,
    admission: HoneypotAdmission,
) {
    if !st.honeypot_telemetry.enqueue_http(
        user.as_ref().map(|current| current.id),
        bait,
        user_agent,
        admission,
    ) {
        tracing::debug!(bait, "honeypot HTTP telemetry queue saturated");
    }
}

/// Enqueue an admitted protocol hit without awaiting database pool admission.
pub(crate) fn record_honeypot_tcp_hit(st: &SharedState, bait: &str, admission: HoneypotAdmission) {
    if !st.honeypot_telemetry.enqueue_tcp(bait, admission) {
        tracing::debug!(bait, "honeypot TCP telemetry queue saturated");
    }
}

/// Global honeypot aggregates deliberately have no participant chain.
pub async fn run_honeypot_chain_checks(_st: &SharedState, _game_id: i32) -> AppResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn source_contains_no_request_path_database_write_or_participant_attribution() {
        let source = include_str!("honeypot.rs");
        let production = source
            .split_once("#[cfg(test)]")
            .expect("honeypot source keeps tests after production code")
            .0;
        assert!(!production.contains(".execute("));
        assert!(!production.contains("SuspicionEvaluationOutbox"));
        assert!(!production.contains("SuspicionEvents"));
        assert!(production.contains("enqueue_http"));
        assert!(production.contains("enqueue_tcp"));
    }
}
