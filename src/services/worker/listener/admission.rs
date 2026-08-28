use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

const GLOBAL_RATE: f64 = 512.0;
const GLOBAL_BURST: f64 = 512.0;
const PEER_RATE: f64 = 32.0;
const PEER_BURST: f64 = 64.0;
const WORKER_RATE: f64 = 16.0;
const WORKER_BURST: f64 = 32.0;
const BUCKET_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_TRACKED_KEYS: usize = 4_096;
static PEER_REJECTIONS: AtomicU64 = AtomicU64::new(0);
static GLOBAL_REJECTIONS: AtomicU64 = AtomicU64::new(0);
static CONNECTION_REJECTIONS: AtomicU64 = AtomicU64::new(0);

fn sample(counter: &AtomicU64) -> Option<u64> {
    let count = counter.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    count.is_power_of_two().then_some(count)
}

pub(super) fn sample_peer_rejection() -> Option<u64> {
    sample(&PEER_REJECTIONS)
}

pub(super) fn sample_global_rejection() -> Option<u64> {
    sample(&GLOBAL_REJECTIONS)
}

pub(super) fn sample_connection_rejection() -> Option<u64> {
    sample(&CONNECTION_REJECTIONS)
}

#[derive(Clone)]
pub(super) struct HandshakeAdmission {
    maximum: usize,
    state: Arc<Mutex<AdmissionState>>,
}

struct AdmissionState {
    active: HashMap<IpAddr, usize>,
    peers: HashMap<IpAddr, TokenBucket>,
    workers: HashMap<Uuid, TokenBucket>,
    global: TokenBucket,
}

#[derive(Clone, Copy)]
struct TokenBucket {
    tokens: f64,
    updated_at: Instant,
}

impl TokenBucket {
    fn full(capacity: f64, now: Instant) -> Self {
        Self {
            tokens: capacity,
            updated_at: now,
        }
    }

    fn take(&mut self, rate: f64, capacity: f64, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.updated_at).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate).min(capacity);
        self.updated_at = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

impl HandshakeAdmission {
    pub(super) fn new(maximum: usize) -> Self {
        let now = Instant::now();
        Self {
            maximum,
            state: Arc::new(Mutex::new(AdmissionState {
                active: HashMap::new(),
                peers: HashMap::new(),
                workers: HashMap::new(),
                global: TokenBucket::full(GLOBAL_BURST, now),
            })),
        }
    }

    /// Applies cheap rate admission before an expensive TLS handshake, then
    /// retains the per-source concurrent-work fence through setup.
    pub(super) fn try_admit(&self, peer: IpAddr) -> Option<PeerHandshakePermit> {
        let now = Instant::now();
        let mut state = self.state.lock().ok()?;
        state.prune(now);
        if !state.peers.contains_key(&peer)
            && state.peers.len() + state.workers.len() >= MAX_TRACKED_KEYS
        {
            return None;
        }
        let peer_admitted = state
            .peers
            .entry(peer)
            .or_insert_with(|| TokenBucket::full(PEER_BURST, now))
            .take(PEER_RATE, PEER_BURST, now);
        // A source which has exhausted its own budget must not consume the
        // fleet-wide budget and starve unrelated legitimate reconnects.
        if !peer_admitted || !state.global.take(GLOBAL_RATE, GLOBAL_BURST, now) {
            return None;
        }
        let count = state.active.entry(peer).or_default();
        if *count >= self.maximum {
            return None;
        }
        *count += 1;
        Some(PeerHandshakePermit {
            peer,
            state: self.state.clone(),
        })
    }

    /// Applies a second fail-fast budget after mTLS identity resolution but
    /// before application hello/session database work.
    pub(super) fn admit_worker(&self, worker_id: Uuid) -> bool {
        let now = Instant::now();
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        state.prune(now);
        if !state.workers.contains_key(&worker_id)
            && state.peers.len() + state.workers.len() >= MAX_TRACKED_KEYS
        {
            return false;
        }
        state
            .workers
            .entry(worker_id)
            .or_insert_with(|| TokenBucket::full(WORKER_BURST, now))
            .take(WORKER_RATE, WORKER_BURST, now)
    }
}

impl AdmissionState {
    fn prune(&mut self, now: Instant) {
        if self.peers.len() + self.workers.len() < MAX_TRACKED_KEYS {
            return;
        }
        self.peers
            .retain(|_, bucket| now.saturating_duration_since(bucket.updated_at) < BUCKET_TTL);
        self.workers
            .retain(|_, bucket| now.saturating_duration_since(bucket.updated_at) < BUCKET_TTL);
    }
}

pub(super) struct PeerHandshakePermit {
    peer: IpAddr,
    state: Arc<Mutex<AdmissionState>>,
}

impl Drop for PeerHandshakePermit {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(count) = state.active.get_mut(&self.peer) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            state.active.remove(&self.peer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permit_holds_the_peer_slot_until_application_setup_releases_it() {
        let admission = HandshakeAdmission::new(1);
        let peer: IpAddr = "192.0.2.10".parse().unwrap();
        let permit = admission.try_admit(peer).expect("first handshake");
        assert!(admission.try_admit(peer).is_none());
        drop(permit);
        assert!(admission.try_admit(peer).is_some());
    }

    #[test]
    fn distinct_peers_have_independent_handshake_budgets() {
        let admission = HandshakeAdmission::new(1);
        let first: IpAddr = "192.0.2.10".parse().unwrap();
        let second: IpAddr = "192.0.2.11".parse().unwrap();
        let _first = admission.try_admit(first).expect("first peer");
        assert!(admission.try_admit(second).is_some());
    }

    #[test]
    fn worker_identity_budget_is_bounded_independently_of_source_churn() {
        let admission = HandshakeAdmission::new(64);
        let worker_id = Uuid::new_v4();
        assert!((0..32).all(|_| admission.admit_worker(worker_id)));
        assert!(!admission.admit_worker(worker_id));
        assert!(admission.admit_worker(Uuid::new_v4()));
    }

    #[test]
    fn rejected_source_churn_does_not_drain_the_global_reconnect_budget() {
        let admission = HandshakeAdmission::new(2_000);
        let noisy: IpAddr = "192.0.2.10".parse().unwrap();
        let legitimate: IpAddr = "192.0.2.11".parse().unwrap();
        let permits = (0..1_000)
            .filter_map(|_| admission.try_admit(noisy))
            .collect::<Vec<_>>();
        assert_eq!(permits.len(), PEER_BURST as usize);
        assert!(admission.try_admit(legitimate).is_some());
    }
}
