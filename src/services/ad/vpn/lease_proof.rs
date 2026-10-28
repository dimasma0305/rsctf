//! Conservative process-local freshness for the dedicated PostgreSQL session.
//! Wall time is exported only for operators; admission uses a monotonic clock.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Acquire, Ordering::Release};

pub(super) const FRESHNESS_MS: u64 = 10_000;
pub(super) const QUERY_TIMEOUT_MS: u64 = 2_000;

#[derive(Default)]
pub(super) struct LeaseProof {
    held: AtomicBool,
    confirmed_at: AtomicU64,
    confirmed_wall_seconds: AtomicU64,
    probe_micros: AtomicU64,
}

impl LeaseProof {
    pub(super) fn grant(&self, started: u64, wall_seconds: u64) {
        self.confirmed_at.store(started, Release);
        self.confirmed_wall_seconds.store(wall_seconds, Release);
        self.held.store(true, Release);
    }

    pub(super) fn valid_at(&self, now: u64) -> bool {
        let confirmed = self.confirmed_at.load(Acquire);
        self.held.load(Acquire)
            && confirmed > 0
            && now >= confirmed
            && now - confirmed < FRESHNESS_MS
    }

    pub(super) fn refresh(&self, started: u64, now: u64, wall_seconds: u64) -> bool {
        // A task/process pause may outlive a previously good connection. A late
        // response cannot revive expired proof, even when that session is alive.
        if !self.valid_at(now)
            || started < self.confirmed_at.load(Acquire)
            || now < started
            || now - started >= QUERY_TIMEOUT_MS
        {
            self.revoke();
            return false;
        }
        self.confirmed_at.store(started, Release);
        self.confirmed_wall_seconds.store(wall_seconds, Release);
        self.probe_micros.store((now - started) * 1000, Release);
        true
    }

    pub(super) fn revoke(&self) {
        self.held.store(false, Release);
    }

    pub(super) fn metrics(&self, now: u64) -> [(&'static str, f64); 5] {
        let confirmed = self.confirmed_at.load(Acquire);
        [
            (
                "network_owner_lease_valid",
                u8::from(self.valid_at(now)) as f64,
            ),
            (
                "network_owner_confirmation_age_seconds",
                if confirmed == 0 {
                    0.0
                } else {
                    now.saturating_sub(confirmed) as f64 / 1000.0
                },
            ),
            (
                "network_owner_confirmation_timestamp_seconds",
                self.confirmed_wall_seconds.load(Acquire) as f64,
            ),
            (
                "network_owner_proof_lifetime_seconds",
                FRESHNESS_MS as f64 / 1000.0,
            ),
            (
                "network_owner_probe_seconds",
                self.probe_micros.load(Acquire) as f64 / 1_000_000.0,
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_or_released_proof_cannot_be_revived_by_a_late_response() {
        let proof = LeaseProof::default();
        assert!(!proof.valid_at(1));
        proof.grant(1, 100);
        assert!(proof.valid_at(FRESHNESS_MS));
        assert!(!proof.valid_at(FRESHNESS_MS + 1));
        assert!(!proof.refresh(FRESHNESS_MS, FRESHNESS_MS + 1, 120));
        assert!(!proof.valid_at(FRESHNESS_MS + 1));
        proof.grant(20_000, 130);
        proof.revoke();
        assert!(!proof.refresh(20_001, 20_002, 140));
    }

    #[test]
    fn confirmation_uses_query_start_and_rejects_slow_or_reordered_probes() {
        let proof = LeaseProof::default();
        proof.grant(1, 100);
        assert!(proof.refresh(3000, 4000, 200));
        assert!(proof.valid_at(12_999));
        assert!(!proof.valid_at(13_000));
        assert_eq!(proof.metrics(5000)[1].1, 2.0);
        assert_eq!(proof.metrics(5000)[4].1, 1.0);
        assert!(!proof.refresh(5000, 5000 + QUERY_TIMEOUT_MS, 201));
        proof.grant(10_000, 300);
        assert!(!proof.refresh(9000, 10_001, 301));
        proof.grant(10_000, 300);
        assert!(!proof.valid_at(9999));
    }

    #[test]
    fn wall_clock_adjustments_change_observability_but_not_expiry() {
        let proof = LeaseProof::default();
        proof.grant(1, 1000);
        assert!(proof.refresh(3000, 3001, 1));
        assert_eq!(proof.metrics(3001)[2].1, 1.0);
        assert!(proof.valid_at(12_999));
        assert!(!proof.valid_at(13_000));
        assert!(!proof.refresh(13_001, 13_002, 9_999_999));
    }
}
