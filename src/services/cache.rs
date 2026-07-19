//! Distributed cache abstraction. Ports RSCTF `Services/Cache/CacheHelper`.
//!
//! Defaults to a bounded in-process map when Redis is explicitly unconfigured.
//! When `RSCTF_REDIS_URL` is set, Redis remains a required readiness dependency:
//! an unavailable backend fails cache operations safely and reconnects instead
//! of silently changing the process to inconsistent local-only mode.
//!
//! Values are stored and returned as [`Bytes`]: a cache hit is a refcount bump,
//! not a copy of a potentially large body, and scoreboard handlers ship the
//! returned `Bytes` as the response body with **zero copy**. `set` takes `&[u8]`; readers
//! that want text use `str::from_utf8` / `serde_json::from_slice`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;

const LOCAL_MAX_ENTRIES: usize = 16_384;
const LOCAL_MAX_BYTES: usize = 64 * 1024 * 1024;
const L1_MAX_ENTRIES: usize = 4_096;
const L1_MAX_BYTES: usize = 32 * 1024 * 1024;
const EXPIRED_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const REDIS_IO_TIMEOUT: Duration = Duration::from_millis(750);
const REDIS_RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// Health of the cache backend used by dependency-readiness probes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheBackendHealth {
    /// Process-local cache; no shared Redis backend is active.
    Local,
    /// The configured shared backend answered a lightweight probe.
    Ready,
    /// The shared backend did not answer successfully.
    Unavailable,
}

#[async_trait]
pub trait Cache: Send + Sync {
    async fn get(&self, key: &str) -> Option<Bytes>;
    /// Atomically return and remove a value. One-time credentials must use this
    /// instead of a racy `get` followed by `remove`.
    async fn get_and_remove(&self, key: &str) -> Option<Bytes>;
    /// Atomically remove `key` only when its value matches `expected`.
    async fn compare_and_remove(&self, key: &str, expected: &[u8]) -> bool;
    /// Atomically insert `key` only when no live value exists. This is used
    /// when a failed one-time operation restores its reservation without
    /// overwriting a newer value created concurrently.
    async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> bool;
    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>);
    async fn remove(&self, key: &str);

    /// Probe the authoritative shared cache backend. Local-only caches return
    /// [`CacheBackendHealth::Local`] without doing I/O.
    async fn backend_health(&self) -> CacheBackendHealth {
        CacheBackendHealth::Local
    }
}

struct Entry {
    value: Bytes,
    expires_at: Option<Instant>,
    generation: u64,
}

struct MemoryState {
    map: HashMap<String, Entry>,
    /// Insertion order with generations makes eviction O(1) amortized without
    /// taking a write lock on cache hits merely to maintain an exact LRU.
    order: VecDeque<(u64, String)>,
    order_key_bytes: usize,
    payload_bytes: usize,
    next_generation: u64,
    last_expired_sweep: Instant,
}

impl MemoryState {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            order_key_bytes: 0,
            payload_bytes: 0,
            next_generation: 0,
            last_expired_sweep: Instant::now(),
        }
    }

    fn entry_bytes(key: &str, entry: &Entry) -> usize {
        key.len().saturating_add(entry.value.len())
    }

    fn remove(&mut self, key: &str) -> Option<Entry> {
        let entry = self.map.remove(key)?;
        self.payload_bytes = self
            .payload_bytes
            .saturating_sub(Self::entry_bytes(key, &entry));
        Some(entry)
    }

    fn sweep_expired(&mut self, now: Instant) {
        let mut removed_bytes = 0usize;
        self.map.retain(|key, entry| {
            let keep = entry.expires_at.is_none_or(|expires_at| expires_at > now);
            if !keep {
                removed_bytes = removed_bytes.saturating_add(Self::entry_bytes(key, entry));
            }
            keep
        });
        self.payload_bytes = self.payload_bytes.saturating_sub(removed_bytes);
        self.last_expired_sweep = now;
    }

    fn evict_to_limits(&mut self, max_entries: usize, max_bytes: usize) {
        while self.map.len() > max_entries || self.payload_bytes > max_bytes {
            let Some((generation, key)) = self.order.pop_front() else {
                break;
            };
            self.order_key_bytes = self.order_key_bytes.saturating_sub(key.len());
            if self
                .map
                .get(&key)
                .is_some_and(|entry| entry.generation == generation)
            {
                self.remove(&key);
            }
        }

        // Repeated replacements leave stale generation records in the queue.
        // Compact outside the read path so queue metadata is bounded too.
        let compact_at = max_entries.saturating_mul(4).max(64);
        if self.order.len() > compact_at || self.order_key_bytes > max_bytes {
            let mut live: Vec<_> = self
                .map
                .iter()
                .map(|(key, entry)| (entry.generation, key.clone()))
                .collect();
            live.sort_unstable_by_key(|(generation, _)| *generation);
            self.order = live.into();
            self.order_key_bytes = self.order.iter().map(|(_, key)| key.len()).sum();
        }
    }
}

/// Process-local cache with TTL eviction on read. An `RwLock` (not a `Mutex`) so
/// the read-heavy hot path (the scoreboard is polled thousands of times/sec)
/// takes a shared lock and readers don't serialise on each other; only a write
/// (`set`, or evicting a just-expired key) takes the exclusive lock.
pub struct InMemoryCache {
    state: RwLock<MemoryState>,
    max_entries: usize,
    max_bytes: usize,
}

impl InMemoryCache {
    pub fn new() -> Self {
        Self::with_limits(LOCAL_MAX_ENTRIES, LOCAL_MAX_BYTES)
    }

    fn l1() -> Self {
        Self::with_limits(L1_MAX_ENTRIES, L1_MAX_BYTES)
    }

    fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        assert!(max_entries > 0, "cache entry limit must be positive");
        assert!(max_bytes > 0, "cache byte limit must be positive");
        Self {
            state: RwLock::new(MemoryState::new()),
            max_entries,
            max_bytes,
        }
    }
}

impl Default for InMemoryCache {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Cache for InMemoryCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        // Fast path: a shared read lock + a `Bytes` refcount clone.
        {
            let state = self.state.read().unwrap_or_else(|e| e.into_inner());
            match state.map.get(key) {
                Some(e) if e.expires_at.map(|t| t > Instant::now()).unwrap_or(true) => {
                    return Some(e.value.clone());
                }
                None => return None,
                Some(_) => {} // expired — evict below (keeps evict-on-read for the
                              // one-shot captcha / reset-token / temp-password keys)
            }
        }
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = state.map.get(key) {
            if e.expires_at.map(|t| t <= Instant::now()).unwrap_or(false) {
                state.remove(key);
            }
        }
        None
    }

    async fn get_and_remove(&self, key: &str) -> Option<Bytes> {
        let entry = self
            .state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key)?;
        if entry
            .expires_at
            .is_some_and(|expires_at| expires_at <= Instant::now())
        {
            None
        } else {
            Some(entry.value)
        }
    }

    async fn compare_and_remove(&self, key: &str, expected: &[u8]) -> bool {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let matches = state.map.get(key).is_some_and(|entry| {
            entry.expires_at.is_none_or(|expires_at| expires_at > now)
                && entry.value.as_ref() == expected
        });
        let expired = state
            .map
            .get(key)
            .is_some_and(|entry| entry.expires_at.is_some_and(|expires_at| expires_at <= now));
        if matches || expired {
            state.remove(key);
        }
        matches
    }

    async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> bool {
        let now = Instant::now();
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        if state
            .map
            .get(key)
            .is_some_and(|entry| entry.expires_at.is_some_and(|expires_at| expires_at <= now))
        {
            state.remove(key);
        }
        if state.map.contains_key(key) {
            return false;
        }

        let entry_bytes = key.len().saturating_add(value.len());
        if entry_bytes > self.max_bytes {
            return false;
        }
        if now.duration_since(state.last_expired_sweep) >= EXPIRED_SWEEP_INTERVAL
            || state.map.len() >= self.max_entries
            || state.payload_bytes.saturating_add(entry_bytes) > self.max_bytes
        {
            state.sweep_expired(now);
        }
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1);
        state.map.insert(
            key.to_string(),
            Entry {
                value: Bytes::copy_from_slice(value),
                expires_at: ttl.map(|duration| now + duration),
                generation,
            },
        );
        state.payload_bytes = state.payload_bytes.saturating_add(entry_bytes);
        state.order.push_back((generation, key.to_string()));
        state.order_key_bytes = state.order_key_bytes.saturating_add(key.len());
        state.evict_to_limits(self.max_entries, self.max_bytes);
        true
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
        let now = Instant::now();
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.remove(key);
        if now.duration_since(state.last_expired_sweep) >= EXPIRED_SWEEP_INTERVAL
            || state.map.len() >= self.max_entries
            || state
                .payload_bytes
                .saturating_add(key.len())
                .saturating_add(value.len())
                > self.max_bytes
        {
            state.sweep_expired(now);
        }
        let entry_bytes = key.len().saturating_add(value.len());
        if entry_bytes > self.max_bytes {
            return;
        }
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1);
        state.map.insert(
            key.to_string(),
            Entry {
                value: Bytes::copy_from_slice(value),
                expires_at: ttl.map(|duration| now + duration),
                generation,
            },
        );
        state.payload_bytes = state.payload_bytes.saturating_add(entry_bytes);
        state.order.push_back((generation, key.to_string()));
        state.order_key_bytes = state.order_key_bytes.saturating_add(key.len());
        state.evict_to_limits(self.max_entries, self.max_bytes);
    }

    async fn remove(&self, key: &str) {
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

/// Redis-backed cache using a shared connection manager.
pub struct RedisCache {
    client: redis::Client,
    state: RwLock<RedisConnectionState>,
    reconnect: tokio::sync::Mutex<()>,
}

struct RedisConnectionState {
    conn: Option<redis::aio::ConnectionManager>,
    retry_after: Instant,
}

impl RedisCache {
    pub async fn connect(url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(url)?;
        let conn = tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            crate::utils::redis::connection_manager(&client),
        )
        .await
        .map_err(|_| anyhow::anyhow!("redis connection timed out"))??;
        Ok(Self::with_connection(client, Some(conn)))
    }

    /// Keep a configured Redis backend required even when its initial connection
    /// is unavailable. Operations fail closed until a bounded lazy reconnect
    /// succeeds, and `/healthz` continues to report the backend unavailable.
    pub fn disconnected(url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(url)?;
        Ok(Self::with_connection(client, None))
    }

    fn with_connection(client: redis::Client, conn: Option<redis::aio::ConnectionManager>) -> Self {
        Self {
            client,
            state: RwLock::new(RedisConnectionState {
                conn,
                retry_after: Instant::now(),
            }),
            reconnect: tokio::sync::Mutex::new(()),
        }
    }

    async fn connection(&self) -> Option<redis::aio::ConnectionManager> {
        {
            let state = self.state.read().unwrap_or_else(|error| error.into_inner());
            if let Some(conn) = state.conn.as_ref() {
                return Some(conn.clone());
            }
            if Instant::now() < state.retry_after {
                return None;
            }
        }

        let _reconnect = self.reconnect.lock().await;
        {
            let state = self.state.read().unwrap_or_else(|error| error.into_inner());
            if let Some(conn) = state.conn.as_ref() {
                return Some(conn.clone());
            }
            if Instant::now() < state.retry_after {
                return None;
            }
        }

        let result = tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            crate::utils::redis::connection_manager(&self.client),
        )
        .await;
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        match result {
            Ok(Ok(conn)) => {
                state.conn = Some(conn.clone());
                Some(conn)
            }
            Ok(Err(_)) | Err(_) => {
                state.retry_after = Instant::now() + REDIS_RETRY_INTERVAL;
                None
            }
        }
    }
}

#[async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        let mut conn = self.connection().await?;
        tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            redis::cmd("GET")
                .arg(key)
                .query_async::<Option<Vec<u8>>>(&mut conn),
        )
        .await
        .ok()?
        .ok()
        .flatten()
        .map(Bytes::from)
    }

    async fn get_and_remove(&self, key: &str) -> Option<Bytes> {
        let mut conn = self.connection().await?;
        tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            redis::cmd("GETDEL")
                .arg(key)
                .query_async::<Option<Vec<u8>>>(&mut conn),
        )
        .await
        .ok()?
        .ok()
        .flatten()
        .map(Bytes::from)
    }

    async fn compare_and_remove(&self, key: &str, expected: &[u8]) -> bool {
        const SCRIPT: &str = r#"
            if redis.call('GET', KEYS[1]) == ARGV[1] then
                redis.call('DEL', KEYS[1])
                return 1
            end
            return 0
        "#;
        let Some(mut conn) = self.connection().await else {
            return false;
        };
        tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            redis::Script::new(SCRIPT)
                .key(key)
                .arg(expected)
                .invoke_async::<i64>(&mut conn),
        )
        .await
        .is_ok_and(|result| result.is_ok_and(|removed| removed == 1))
    }

    async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> bool {
        let Some(mut conn) = self.connection().await else {
            return false;
        };
        let mut cmd = redis::cmd("SET");
        cmd.arg(key).arg(value).arg("NX");
        if let Some(ttl) = ttl {
            cmd.arg("EX").arg(ttl.as_secs().max(1));
        }
        tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            cmd.query_async::<Option<String>>(&mut conn),
        )
        .await
        .is_ok_and(|result| result.is_ok_and(|reply| reply.as_deref() == Some("OK")))
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
        let Some(mut conn) = self.connection().await else {
            return;
        };
        let mut cmd = redis::cmd("SET");
        cmd.arg(key).arg(value);
        if let Some(ttl) = ttl {
            cmd.arg("EX").arg(ttl.as_secs().max(1));
        }
        let _ = tokio::time::timeout(REDIS_IO_TIMEOUT, cmd.query_async::<()>(&mut conn)).await;
    }

    async fn remove(&self, key: &str) {
        let Some(mut conn) = self.connection().await else {
            return;
        };
        let _ = tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            redis::cmd("DEL").arg(key).query_async::<i64>(&mut conn),
        )
        .await;
    }

    async fn backend_health(&self) -> CacheBackendHealth {
        let Some(mut conn) = self.connection().await else {
            return CacheBackendHealth::Unavailable;
        };
        match tokio::time::timeout(
            REDIS_IO_TIMEOUT,
            redis::cmd("PING").query_async::<String>(&mut conn),
        )
        .await
        {
            Ok(Ok(response)) if response == "PONG" => CacheBackendHealth::Ready,
            _ => CacheBackendHealth::Unavailable,
        }
    }
}

/// Two-tier cache: a process-local L1 in front of a shared L2 (Redis), mirroring
/// RSCTF's `CacheHelper` (in-process `MemoryCache` over an `IDistributedCache`).
///
/// The L1 answers hot reads with **zero network I/O** — the whole point, since a
/// heavily-polled cached value (the scoreboard) otherwise pays a Redis round-trip
/// on every hit. A short L1 TTL bounds how stale a value can be relative to L2
/// (so on multi-node the other replicas' L1 lag at most `l1_ttl`), and `remove`
/// clears both tiers, so an explicit invalidation is immediate on the node that
/// issues it and normally `≤ l1_ttl` everywhere else. Callers whose fill can
/// race a mutation must also account for a stale post-remove write to L2.
pub struct TieredCache {
    l1: InMemoryCache,
    l2: Arc<dyn Cache>,
    l1_ttl: Duration,
}

impl TieredCache {
    pub fn new(l2: Arc<dyn Cache>, l1_ttl: Duration) -> Self {
        Self {
            l1: InMemoryCache::l1(),
            l2,
            l1_ttl,
        }
    }

    /// Cap the L1 copy's lifetime at `l1_ttl` (never hold it longer than L2 would).
    fn l1_ttl(&self, requested: Option<Duration>) -> Option<Duration> {
        Some(requested.map_or(self.l1_ttl, |t| t.min(self.l1_ttl)))
    }
}

#[async_trait]
impl Cache for TieredCache {
    async fn get(&self, key: &str) -> Option<Bytes> {
        if let Some(v) = self.l1.get(key).await {
            return Some(v);
        }
        let v = self.l2.get(key).await?;
        self.l1.set(key, v.as_ref(), Some(self.l1_ttl)).await;
        Some(v)
    }

    async fn get_and_remove(&self, key: &str) -> Option<Bytes> {
        // L2 is authoritative for a distributed one-time consume. Clear L1 even
        // when L2 misses so a stale local copy can never resurrect the value.
        let value = self.l2.get_and_remove(key).await;
        self.l1.remove(key).await;
        value
    }

    async fn compare_and_remove(&self, key: &str, expected: &[u8]) -> bool {
        let removed = self.l2.compare_and_remove(key, expected).await;
        // Always evict L1: on a failed comparison it may contain the stale value
        // that caused the mismatch and must be refreshed from authoritative L2.
        self.l1.remove(key).await;
        removed
    }

    async fn set_if_absent(&self, key: &str, value: &[u8], ttl: Option<Duration>) -> bool {
        let inserted = self.l2.set_if_absent(key, value, ttl).await;
        if inserted {
            self.l1.set(key, value, self.l1_ttl(ttl)).await;
        } else {
            // A local stale value must not hide the authoritative winner.
            self.l1.remove(key).await;
        }
        inserted
    }

    async fn set(&self, key: &str, value: &[u8], ttl: Option<Duration>) {
        self.l1.set(key, value, self.l1_ttl(ttl)).await;
        self.l2.set(key, value, ttl).await;
    }

    async fn remove(&self, key: &str) {
        self.l1.remove(key).await;
        self.l2.remove(key).await;
    }

    async fn backend_health(&self) -> CacheBackendHealth {
        self.l2.backend_health().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_get_and_remove_is_single_use() {
        let cache = InMemoryCache::new();
        cache
            .set("one-time", b"value", Some(Duration::from_secs(60)))
            .await;
        assert_eq!(
            cache.get_and_remove("one-time").await.as_deref(),
            Some(b"value".as_slice())
        );
        assert!(cache.get_and_remove("one-time").await.is_none());
        assert!(cache.get("one-time").await.is_none());
    }

    #[tokio::test]
    async fn in_memory_compare_and_remove_checks_value() {
        let cache = InMemoryCache::new();
        cache.set("current", b"new", None).await;
        assert!(!cache.compare_and_remove("current", b"old").await);
        assert!(cache.compare_and_remove("current", b"new").await);
        assert!(cache.get("current").await.is_none());
    }

    #[tokio::test]
    async fn in_memory_set_if_absent_never_overwrites_a_live_value() {
        let cache = InMemoryCache::new();
        assert!(
            cache
                .set_if_absent("reserved", b"first", Some(Duration::from_secs(60)))
                .await
        );
        assert!(
            !cache
                .set_if_absent("reserved", b"second", Some(Duration::from_secs(60)))
                .await
        );
        assert_eq!(
            cache.get("reserved").await.as_deref(),
            Some(b"first".as_slice())
        );

        cache.set("expired", b"old", Some(Duration::ZERO)).await;
        assert!(cache.set_if_absent("expired", b"fresh", None).await);
        assert_eq!(
            cache.get("expired").await.as_deref(),
            Some(b"fresh".as_slice())
        );
    }

    #[tokio::test]
    async fn in_memory_cache_evicts_old_entries_at_the_entry_limit() {
        let cache = InMemoryCache::with_limits(2, 1_024);
        cache.set("first", b"1", None).await;
        cache.set("second", b"2", None).await;
        cache.set("third", b"3", None).await;

        assert!(cache.get("first").await.is_none());
        assert_eq!(cache.get("second").await.as_deref(), Some(b"2".as_slice()));
        assert_eq!(cache.get("third").await.as_deref(), Some(b"3".as_slice()));
    }

    #[tokio::test]
    async fn in_memory_cache_enforces_payload_bytes_and_sweeps_expiry() {
        let cache = InMemoryCache::with_limits(8, 10);
        cache.set("old", b"123", Some(Duration::ZERO)).await;
        cache.set("large", b"12345", None).await;

        // `large` occupies exactly ten key+value bytes. The expired entry was
        // swept before pressure eviction, so the live value remains available.
        assert!(cache.get("old").await.is_none());
        assert_eq!(
            cache.get("large").await.as_deref(),
            Some(b"12345".as_slice())
        );

        cache.set("too-large", b"12", None).await;
        assert!(cache.get("too-large").await.is_none());
    }

    #[tokio::test]
    async fn in_memory_hits_clone_bytes_without_copying_payload() {
        let cache = InMemoryCache::new();
        cache.set("shared", b"payload", None).await;
        let first = cache.get("shared").await.unwrap();
        let second = cache.get("shared").await.unwrap();
        assert_eq!(first.as_ptr(), second.as_ptr());
    }
}
