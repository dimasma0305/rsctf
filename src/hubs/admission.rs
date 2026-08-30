//! Bounded admission for the read-only event-stream WebSocket hubs.
//!
//! These sockets are intentionally long-lived, so request-rate limiting alone
//! cannot bound their retained tasks, broadcast receivers, and file descriptors.
//! Hold one permit for the complete connection lifetime and partition the
//! ceilings by source and game so one client or event cannot monopolize the
//! process-wide pool.

use std::hash::Hash;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_CONNECTIONS: usize = 2_048;
const MAX_CONNECTIONS_PER_CLIENT: usize = 128;
const MAX_CONNECTIONS_PER_GAME: usize = 1_024;
const MAX_GLOBAL_SCOPE_CONNECTIONS: usize = 256;
const MAX_INBOUND_FRAMES_PER_SECOND: u64 = 8_192;
const MAX_INBOUND_BYTES_PER_SECOND: u64 = 32 * 1024 * 1024;

static CONNECTIONS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONNECTIONS)));
static CLIENT_CONNECTIONS: LazyLock<Arc<DashMap<String, usize>>> =
    LazyLock::new(|| Arc::new(DashMap::new()));
static SCOPE_CONNECTIONS: LazyLock<Arc<DashMap<Scope, usize>>> =
    LazyLock::new(|| Arc::new(DashMap::new()));
static INBOUND_WINDOW: InboundWindow = InboundWindow::new();
static OPERATIONAL_COUNTERS: LazyLock<OperationalCounters> =
    LazyLock::new(OperationalCounters::new);

const WINDOW_RESETTING: u64 = 1 << 63;

struct InboundWindow {
    second: AtomicU64,
    frames: AtomicU64,
    bytes: AtomicU64,
}

impl InboundWindow {
    const fn new() -> Self {
        Self {
            second: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        }
    }

    fn admit(&self, bytes: usize, second: u64, frame_limit: u64, byte_limit: u64) -> bool {
        debug_assert_eq!(second & WINDOW_RESETTING, 0);
        loop {
            let observed = self.second.load(Ordering::Acquire);
            if observed == second {
                break;
            }
            if observed & WINDOW_RESETTING != 0 {
                std::hint::spin_loop();
                continue;
            }
            // A caller may have sampled the previous Unix second immediately
            // before another thread advanced the window. Never move the
            // generation backwards: doing so would clear the counters again
            // and let boundary races exceed the aggregate ceiling.
            if observed > second {
                break;
            }
            if self
                .second
                .compare_exchange(
                    observed,
                    second | WINDOW_RESETTING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.frames.store(0, Ordering::Release);
                self.bytes.store(0, Ordering::Release);
                self.second.store(second, Ordering::Release);
                break;
            }
        }
        let frames = self.frames.fetch_add(1, Ordering::AcqRel) + 1;
        let bytes = self.bytes.fetch_add(bytes as u64, Ordering::AcqRel) + bytes as u64;
        frames <= frame_limit && bytes <= byte_limit
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CloseReason {
    Quota,
    Protocol,
    InvalidHandshake,
    IdleTimeout,
    Authorization,
    FeedResync,
}

/// Fixed-cardinality process-lifetime counters. These never participate in
/// admission decisions and never reset with the one-second abuse window, so an
/// operator can observe rejected work without introducing labels or storage
/// whose size depends on client input.
struct OperationalCounters {
    started_at_unix_ms: i64,
    connections_rejected: AtomicU64,
    inbound_frames: AtomicU64,
    inbound_bytes: AtomicU64,
    inbound_quota_rejections: AtomicU64,
    protocol_rejections: AtomicU64,
    quota_closes: AtomicU64,
    protocol_closes: AtomicU64,
    invalid_handshake_closes: AtomicU64,
    idle_timeout_closes: AtomicU64,
    authorization_closes: AtomicU64,
    feed_resync_closes: AtomicU64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebSocketOperationalMetrics {
    pub started_at_unix_ms: i64,
    pub connections_rejected: u64,
    pub inbound_frames: u64,
    pub inbound_bytes: u64,
    pub inbound_quota_rejections: u64,
    pub protocol_rejections: u64,
    pub quota_closes: u64,
    pub protocol_closes: u64,
    pub invalid_handshake_closes: u64,
    pub idle_timeout_closes: u64,
    pub authorization_closes: u64,
    pub feed_resync_closes: u64,
}

fn saturating_increment(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

impl OperationalCounters {
    fn new() -> Self {
        let started_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(i64::MAX);
        Self {
            started_at_unix_ms,
            connections_rejected: AtomicU64::new(0),
            inbound_frames: AtomicU64::new(0),
            inbound_bytes: AtomicU64::new(0),
            inbound_quota_rejections: AtomicU64::new(0),
            protocol_rejections: AtomicU64::new(0),
            quota_closes: AtomicU64::new(0),
            protocol_closes: AtomicU64::new(0),
            invalid_handshake_closes: AtomicU64::new(0),
            idle_timeout_closes: AtomicU64::new(0),
            authorization_closes: AtomicU64::new(0),
            feed_resync_closes: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> WebSocketOperationalMetrics {
        WebSocketOperationalMetrics {
            started_at_unix_ms: self.started_at_unix_ms,
            connections_rejected: self.connections_rejected.load(Ordering::Relaxed),
            inbound_frames: self.inbound_frames.load(Ordering::Relaxed),
            inbound_bytes: self.inbound_bytes.load(Ordering::Relaxed),
            inbound_quota_rejections: self.inbound_quota_rejections.load(Ordering::Relaxed),
            protocol_rejections: self.protocol_rejections.load(Ordering::Relaxed),
            quota_closes: self.quota_closes.load(Ordering::Relaxed),
            protocol_closes: self.protocol_closes.load(Ordering::Relaxed),
            invalid_handshake_closes: self.invalid_handshake_closes.load(Ordering::Relaxed),
            idle_timeout_closes: self.idle_timeout_closes.load(Ordering::Relaxed),
            authorization_closes: self.authorization_closes.load(Ordering::Relaxed),
            feed_resync_closes: self.feed_resync_closes.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Scope {
    Game(i32),
    Global,
}

impl Scope {
    fn limit(self, limits: Limits) -> usize {
        match self {
            Self::Game(_) => limits.per_game,
            Self::Global => limits.global_scope,
        }
    }
}

#[derive(Clone, Copy)]
struct Limits {
    per_client: usize,
    per_game: usize,
    global_scope: usize,
}

const LIMITS: Limits = Limits {
    per_client: MAX_CONNECTIONS_PER_CLIENT,
    per_game: MAX_CONNECTIONS_PER_GAME,
    global_scope: MAX_GLOBAL_SCOPE_CONNECTIONS,
};

pub(super) struct ConnectionPermit {
    #[allow(dead_code)]
    global: OwnedSemaphorePermit,
    #[allow(dead_code)]
    client: ClientPermit,
    #[allow(dead_code)]
    scope: ScopePermit,
}

struct ClientPermit {
    key: String,
    counts: Arc<DashMap<String, usize>>,
}

impl Drop for ClientPermit {
    fn drop(&mut self) {
        release(&self.counts, self.key.clone());
    }
}

struct ScopePermit {
    key: Scope,
    counts: Arc<DashMap<Scope, usize>>,
}

impl Drop for ScopePermit {
    fn drop(&mut self) {
        release(&self.counts, self.key);
    }
}

fn increment<K>(counts: &DashMap<K, usize>, key: K, limit: usize) -> bool
where
    K: Eq + Hash,
{
    match counts.entry(key) {
        Entry::Occupied(mut entry) if *entry.get() < limit => {
            *entry.get_mut() += 1;
            true
        }
        Entry::Vacant(entry) => {
            entry.insert(1);
            true
        }
        Entry::Occupied(_) => false,
    }
}

fn release<K>(counts: &DashMap<K, usize>, key: K)
where
    K: Eq + Hash,
{
    if let Entry::Occupied(mut entry) = counts.entry(key) {
        if *entry.get() <= 1 {
            entry.remove();
        } else {
            *entry.get_mut() -= 1;
        }
    }
}

fn try_connection_permit_with(
    client_key: String,
    scope_key: Scope,
    global: &Arc<Semaphore>,
    clients: &Arc<DashMap<String, usize>>,
    scopes: &Arc<DashMap<Scope, usize>>,
    limits: Limits,
) -> Option<ConnectionPermit> {
    let global = Arc::clone(global).try_acquire_owned().ok()?;
    if !increment(clients, client_key.clone(), limits.per_client) {
        return None;
    }
    if !increment(scopes, scope_key, scope_key.limit(limits)) {
        release(clients, client_key);
        return None;
    }
    Some(ConnectionPermit {
        global,
        client: ClientPermit {
            key: client_key,
            counts: Arc::clone(clients),
        },
        scope: ScopePermit {
            key: scope_key,
            counts: Arc::clone(scopes),
        },
    })
}

pub(super) fn try_connection_permit(client_key: String, scope: Scope) -> Option<ConnectionPermit> {
    let permit = try_connection_permit_with(
        client_key,
        scope,
        &CONNECTIONS,
        &CLIENT_CONNECTIONS,
        &SCOPE_CONNECTIONS,
        LIMITS,
    );
    if permit.is_none() {
        saturating_increment(&OPERATIONAL_COUNTERS.connections_rejected, 1);
    }
    permit
}

pub(super) fn client_key(headers: &HeaderMap, peer: IpAddr) -> String {
    crate::services::anti_cheat::client_ip(headers, Some(peer))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Cheap process-wide abuse backstop for application frames received after a
/// read-only feed is admitted. A one-second fixed window is deliberately
/// lock-free; per-connection token buckets apply the finer fairness boundary.
pub(super) fn try_inbound_frame(bytes: usize) -> bool {
    let second = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    INBOUND_WINDOW.admit(
        bytes,
        second,
        MAX_INBOUND_FRAMES_PER_SECOND,
        MAX_INBOUND_BYTES_PER_SECOND,
    )
}

pub(super) fn record_inbound_attempt(bytes: usize, admitted: bool) {
    saturating_increment(&OPERATIONAL_COUNTERS.inbound_frames, 1);
    saturating_increment(
        &OPERATIONAL_COUNTERS.inbound_bytes,
        u64::try_from(bytes).unwrap_or(u64::MAX),
    );
    if !admitted {
        saturating_increment(&OPERATIONAL_COUNTERS.inbound_quota_rejections, 1);
    }
}

pub(super) fn record_protocol_rejection() {
    saturating_increment(&OPERATIONAL_COUNTERS.protocol_rejections, 1);
}

pub(super) fn record_close(reason: CloseReason) {
    let counter = match reason {
        CloseReason::Quota => &OPERATIONAL_COUNTERS.quota_closes,
        CloseReason::Protocol => &OPERATIONAL_COUNTERS.protocol_closes,
        CloseReason::InvalidHandshake => &OPERATIONAL_COUNTERS.invalid_handshake_closes,
        CloseReason::IdleTimeout => &OPERATIONAL_COUNTERS.idle_timeout_closes,
        CloseReason::Authorization => &OPERATIONAL_COUNTERS.authorization_closes,
        CloseReason::FeedResync => &OPERATIONAL_COUNTERS.feed_resync_closes,
    };
    saturating_increment(counter, 1);
}

pub(crate) fn operational_metrics() -> WebSocketOperationalMetrics {
    OPERATIONAL_COUNTERS.snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> Limits {
        Limits {
            per_client: 2,
            per_game: 3,
            global_scope: 1,
        }
    }

    #[test]
    fn permits_bound_clients_scopes_and_the_global_pool() {
        let global = Arc::new(Semaphore::new(4));
        let clients = Arc::new(DashMap::new());
        let scopes = Arc::new(DashMap::new());
        let limits = test_limits();

        let first = try_connection_permit_with(
            "client-a".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        let second = try_connection_permit_with(
            "client-a".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        assert!(try_connection_permit_with(
            "client-a".into(),
            Scope::Game(8),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());

        let third = try_connection_permit_with(
            "client-b".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        assert!(try_connection_permit_with(
            "client-c".into(),
            Scope::Game(7),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());

        let global_scope = try_connection_permit_with(
            "client-c".into(),
            Scope::Global,
            &global,
            &clients,
            &scopes,
            limits,
        )
        .unwrap();
        assert!(try_connection_permit_with(
            "client-d".into(),
            Scope::Global,
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());
        assert!(try_connection_permit_with(
            "client-d".into(),
            Scope::Game(8),
            &global,
            &clients,
            &scopes,
            limits,
        )
        .is_none());

        drop((first, second, third, global_scope));
        assert!(clients.is_empty());
        assert!(scopes.is_empty());
        assert_eq!(global.available_permits(), 4);
    }

    #[test]
    fn operational_counters_are_saturating_and_fixed_cardinality() {
        let counters = OperationalCounters::new();
        counters.inbound_frames.store(u64::MAX, Ordering::Relaxed);
        saturating_increment(&counters.inbound_frames, 1);
        saturating_increment(&counters.inbound_bytes, 42);
        saturating_increment(&counters.protocol_closes, 1);
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.inbound_frames, u64::MAX);
        assert_eq!(snapshot.inbound_bytes, 42);
        assert_eq!(snapshot.protocol_closes, 1);

        let json = serde_json::to_value(snapshot).unwrap();
        assert!(json.get("startedAtUnixMs").is_some());
        assert!(json.get("inboundQuotaRejections").is_some());
        assert_eq!(json.as_object().unwrap().len(), 12);
    }

    #[test]
    fn aggregate_inbound_window_strictly_bounds_frames_bytes_and_resets() {
        let window = InboundWindow::new();
        assert!(window.admit(4, 10, 2, 8));
        assert!(window.admit(4, 10, 2, 8));
        assert!(!window.admit(0, 10, 2, 8));
        assert!(!window.admit(1, 10, 99, 8));
        assert!(window.admit(8, 11, 2, 8));
        assert!(!window.admit(1, 11, 2, 8));

        let boundary = InboundWindow::new();
        assert!(boundary.admit(1, 20, 2, 8));
        assert!(boundary.admit(1, 19, 2, 8));
        assert!(!boundary.admit(1, 20, 2, 8));
        assert_eq!(boundary.second.load(Ordering::Acquire), 20);
    }
}
