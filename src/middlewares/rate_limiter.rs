//! middlewares/rate_limiter.rs — ported from RSCTF `Middlewares/RateLimiter.cs`.
//!
//! A per-policy request throttle. RSCTF registers one **global** sliding-window
//! limiter (every `/api` request) plus a handful of **named** policies attached
//! to individual endpoints via the `[EnableRateLimiting(...)]` attribute. We
//! mirror that decorator model exactly:
//!
//! * The Global window is a plain [`global_middleware`] layered once over the
//!   whole `/api` router in `server.rs`.
//! * Each named policy is a per-route **decorator** — [`limited`] wraps a single
//!   handler, the direct analogue of `[EnableRateLimiting(policy)]`:
//!   ```ignore
//!   use crate::middlewares::rate_limiter::{limited, Policy};
//!   .route("/api/account/login", limited(Policy::Login, post(login)))
//!   ```
//!
//! Distributed deployments share limits through Redis. If Redis is unavailable,
//! requests fall back to the same sharded in-process limiter used by a single-node
//! deployment: availability is preserved without silently becoming unlimited.
//!
//! Requests are partitioned by client IP, taken from proxy-set headers that a
//! client cannot forge past a trusted reverse proxy: `X-Real-IP`, else the
//! **rightmost** `X-Forwarded-For` hop (the one the proxy appended — leftmost
//! entries are client-supplied and spoofable, which would defeat the per-IP Login
//! brute-force ceiling), else the raw `ConnectInfo` peer address.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State as AxumState};
use axum::http::{header, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;

static AUTHENTICATED_IP_BACKSTOP_PER_MINUTE: LazyLock<u32> = LazyLock::new(|| {
    std::env::var("RSCTF_AUTH_IP_BACKSTOP_PER_MINUTE")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (12_000..=1_000_000).contains(value))
        .unwrap_or(120_000)
});

static CREDENTIAL_IP_ADMISSION_PER_MINUTE: LazyLock<u32> = LazyLock::new(|| {
    std::env::var("RSCTF_CREDENTIAL_IP_ADMISSION_PER_MINUTE")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (3_000..=1_000_000).contains(value))
        .unwrap_or(30_000)
});

/// Maximum distinct plausible flags one participation may enqueue immediately.
/// Four maximum-size batches leave room for ordinary exploit retries without
/// turning the fixed-rate test allowance into a five-minute production burst.
const MIN_AD_SUBMIT_BURST_FLAGS: u32 = 100;
const DEFAULT_AD_SUBMIT_BURST_FLAGS: u32 = 400;
const MAX_AD_SUBMIT_BURST_FLAGS: u32 = 3_200;

fn parse_ad_submit_burst_flags(value: Option<&str>) -> Result<u32, String> {
    let Some(value) = value else {
        return Ok(DEFAULT_AD_SUBMIT_BURST_FLAGS);
    };
    let parsed = value.parse::<u32>().map_err(|_| {
        format!(
            "RSCTF_AD_SUBMIT_BURST_FLAGS must be an integer from \
             {MIN_AD_SUBMIT_BURST_FLAGS} through {MAX_AD_SUBMIT_BURST_FLAGS}"
        )
    })?;
    if !(MIN_AD_SUBMIT_BURST_FLAGS..=MAX_AD_SUBMIT_BURST_FLAGS).contains(&parsed) {
        return Err(format!(
            "RSCTF_AD_SUBMIT_BURST_FLAGS must be an integer from \
             {MIN_AD_SUBMIT_BURST_FLAGS} through {MAX_AD_SUBMIT_BURST_FLAGS}"
        ));
    }
    Ok(parsed)
}

static AD_SUBMIT_BURST_FLAGS: LazyLock<Result<u32, String>> = LazyLock::new(|| {
    parse_ad_submit_burst_flags(std::env::var("RSCTF_AD_SUBMIT_BURST_FLAGS").ok().as_deref())
});

/// Reject an invalid explicit A&D work budget before the server accepts traffic.
pub fn validate_configuration() -> anyhow::Result<()> {
    AD_SUBMIT_BURST_FLAGS
        .as_ref()
        .map(|_| ())
        .map_err(|message| anyhow::anyhow!(message.clone()))
}

fn ad_submit_burst_flags() -> u32 {
    AD_SUBMIT_BURST_FLAGS
        .as_ref()
        .copied()
        .unwrap_or_else(|message| panic!("{message}"))
}

// ---------------------------------------------------------------------------
// Policies
// ---------------------------------------------------------------------------

/// The rate-limit policies, mirroring RSCTF's `RateLimiter.LimitPolicy` plus the
/// always-on `Global` sliding window that every `/api` request passes through.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Policy {
    /// 150 requests / 60s sliding window — all `/api` requests.
    Global,
    /// 50 / 60s on `POST /api/account/login` (per-IP brute-force ceiling).
    Login,
    /// 20 / 300s on the mail-triggering endpoints + oauth start.
    Register,
    /// Token bucket, ~1 token / 5s, small burst — flag submission.
    Submit,
    /// Token bucket, ~1 / 10s — container create/delete/extend.
    Container,
    /// Token bucket, ~1 / 10s with a ~30 burst — heavy DB query routes.
    Query,
    /// One-at-a-time heavy admin routes; modelled as a tight ~1 / 10s bucket.
    Concurrency,
    /// High per-IP abuse backstop for authenticated traffic.
    GlobalIpBackstop,
    /// Cheap source-IP admission before JWT verification or A&D token lookup.
    /// Appended to preserve every shipped Redis policy discriminant.
    CredentialIpAdmission,
    /// Team-scoped A&D batch work budget. The cost is the number of distinct,
    /// plausible flags in the request rather than one token per HTTP request.
    /// Appended to preserve every shipped Redis policy discriminant.
    AdSubmit,
    /// Source-IP admission for privileged hub negotiation and WebSocket upgrade.
    /// Frames inside an established connection are intentionally not charged.
    /// Appended to preserve every shipped Redis policy discriminant.
    PrivilegedHubAdmission,
    /// Source-IP admission for anonymous/public hub negotiation and upgrade.
    /// Long-lived socket counts are bounded separately by `hubs::admission`.
    /// Appended to preserve every shipped Redis policy discriminant.
    PublicHubAdmission,
}

/// The shape of a policy: either a sliding window (log of hit instants) or a
/// token bucket (fractional tokens refilled continuously).
#[derive(Clone, Copy)]
enum Kind {
    /// Allow at most `permit` hits within any `window`.
    Sliding { permit: u32, window: Duration },
    /// A bucket of at most `capacity` tokens refilled at `refill_per_sec`; each
    /// request costs one token.
    Bucket { capacity: f64, refill_per_sec: f64 },
}

impl Policy {
    fn kind(self) -> Kind {
        match self {
            // GlobalPermitLimit = 150, GlobalWindow = 1 min.
            Policy::Global => Kind::Sliding {
                permit: 150,
                window: Duration::from_secs(60),
            },
            // These ceilings are intentionally large. A sliding window would
            // retain tens of thousands of `Instant`s per busy source; an O(1)
            // token bucket enforces the same sustained per-minute rate with one
            // timestamp and one float.
            Policy::GlobalIpBackstop => {
                let capacity = *AUTHENTICATED_IP_BACKSTOP_PER_MINUTE as f64;
                Kind::Bucket {
                    capacity,
                    refill_per_sec: capacity / 60.0,
                }
            }
            Policy::CredentialIpAdmission => {
                let capacity = *CREDENTIAL_IP_ADMISSION_PER_MINUTE as f64;
                Kind::Bucket {
                    capacity,
                    refill_per_sec: capacity / 60.0,
                }
            }
            // A maximum-size batch may contain 100 distinct flags. Bound the
            // default immediate queue to four such batches while limiting
            // sustained lookup work to ten flags/s per participation. The
            // larger opt-in ceiling exists only for an isolated fixed-rate
            // campaign that deliberately needs a longer pre-funded window.
            Policy::AdSubmit => Kind::Bucket {
                capacity: ad_submit_burst_flags() as f64,
                refill_per_sec: 10.0,
            },
            Policy::PrivilegedHubAdmission => Kind::Bucket {
                capacity: 120.0,
                refill_per_sec: 10.0,
            },
            Policy::PublicHubAdmission => Kind::Bucket {
                capacity: 512.0,
                refill_per_sec: 10.0,
            },
            // LoginPermitLimit = 50, LoginWindow = 1 min.
            Policy::Login => Kind::Sliding {
                permit: 50,
                window: Duration::from_secs(60),
            },
            // RegisterPermitLimit = 20, RegisterWindow = 5 min.
            Policy::Register => Kind::Sliding {
                permit: 20,
                window: Duration::from_secs(300),
            },
            // ~1 token / 5s, small burst.
            Policy::Submit => Kind::Bucket {
                capacity: 12.0,
                refill_per_sec: 1.0 / 5.0,
            },
            // ~1 token / 10s.
            Policy::Container => Kind::Bucket {
                capacity: 6.0,
                refill_per_sec: 1.0 / 10.0,
            },
            // ~1 token / 10s, burst ~30.
            Policy::Query => Kind::Bucket {
                capacity: 30.0,
                refill_per_sec: 1.0 / 10.0,
            },
            // "1 concurrent" heavy admin route, modelled as a tight ~1 / 10s cap.
            Policy::Concurrency => Kind::Bucket {
                capacity: 1.0,
                refill_per_sec: 1.0 / 10.0,
            },
        }
    }

    /// A fixed-window `(limit, window-in-ms)` representation. Redis uses this for
    /// sliding policies; bucket policies use their native capacity/refill values.
    /// Keeping the bucket-derived representation is useful for diagnostics and
    /// tests that compare the sustained rate of both backends.
    fn fixed_window(self) -> (u32, u64) {
        match self.kind() {
            Kind::Sliding { permit, window } => (permit, window.as_millis() as u64),
            Kind::Bucket {
                capacity,
                refill_per_sec,
            } => (
                capacity as u32,
                ((capacity / refill_per_sec) * 1000.0) as u64,
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared in-memory store
// ---------------------------------------------------------------------------

/// Per-`(policy, partition-key)` limiter state.
enum State {
    /// Sliding window: the instants of the retained hits, oldest at the front.
    Sliding(VecDeque<Instant>),
    /// Token bucket: current (fractional) token count and the last refill time.
    Bucket { tokens: f64, last: Instant },
}

/// The store is split into [`SHARDS`] independently-locked maps. It began as a
/// single `Mutex<HashMap>` taken on **every** `/api` request; under load that one
/// blocking lock became a futex convoy that serialised all request-processing
/// threads (measured: throughput collapsed to hundreds/sec and *fell* as
/// concurrency rose, with CPU idle — the signature of lock contention). Sharding
/// by `hash(policy, key)` turns it into N independent locks — two requests
/// contend only when they hash to the same shard — mirroring how RSCTF's
/// in-process `PartitionedRateLimiter` (a striped `ConcurrentDictionary`) keeps
/// the hot path off a single lock. 256 shards keeps contention negligible even at
/// hundreds of concurrent requests.
const SHARDS: usize = 256;

type Shard = Mutex<HashMap<(Policy, String), State>>;

/// The single-node store. `LazyLock` gives us a process-wide singleton without a
/// dependency; each shard lock is only ever held for one cheap bookkeeping check
/// on its slice of the keyspace, so contention is negligible.
static STORE: LazyLock<Box<[Shard]>> =
    LazyLock::new(|| (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect());

/// The shard owning `(policy, key)` — hashed the same way for the life of the key,
/// so a key always lands in exactly one shard.
fn shard_for(policy: Policy, key: &str) -> &'static Shard {
    let mut h = DefaultHasher::new();
    policy.hash(&mut h);
    key.hash(&mut h);
    &STORE[(h.finish() as usize) % SHARDS]
}

/// Check one policy for one partition key. `Ok(())` allows the request;
/// `Err(secs)` denies it and reports the `Retry-After` value in seconds.
fn check(policy: Policy, key: String) -> Result<(), u64> {
    check_weighted(policy, key, 1)
}

/// Check a policy while charging more than one unit atomically. A&D batches use
/// this to bound actual distinct-flag adjudication work; repeated flags in one
/// request do not amplify database cost and therefore consume one unit.
fn check_weighted(policy: Policy, key: String, cost: u32) -> Result<(), u64> {
    let now = Instant::now();
    let mut store = shard_for(policy, &key)
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // Bound memory: RSCTF's Redis keys expire; our in-memory map must evict idle
    // partitions itself or an attacker rotating keys grows it without limit. When
    // a shard gets large, drop entries with no recent activity (a fully-drained
    // window or a fully-refilled bucket) — both reconstruct on the next hit at no
    // cost. Runs per-shard under that shard's lock, so it never blocks requests
    // whose keys live in other shards.
    maybe_sweep(&mut store, now);

    let entry = store
        .entry((policy, key))
        .or_insert_with(|| match policy.kind() {
            Kind::Sliding { .. } => State::Sliding(VecDeque::new()),
            Kind::Bucket { capacity, .. } => State::Bucket {
                tokens: capacity,
                last: now,
            },
        });

    match (policy.kind(), entry) {
        (Kind::Sliding { permit, window }, State::Sliding(hits)) => {
            // Drop hits that have aged out of the window.
            while let Some(&front) = hits.front() {
                if now.duration_since(front) >= window {
                    hits.pop_front();
                } else {
                    break;
                }
            }
            let cost = cost.max(1);
            if cost > permit {
                return Err(ceil_secs(window.as_secs_f64()));
            }
            if hits.len() as u32 > permit - cost {
                // The oldest retained hit frees a slot when it expires.
                let oldest = *hits.front().expect("len >= permit >= 1");
                let wait = (oldest + window).saturating_duration_since(now);
                Err(ceil_secs(wait.as_secs_f64()))
            } else {
                hits.extend(std::iter::repeat_n(now, cost as usize));
                Ok(())
            }
        }
        (
            Kind::Bucket {
                capacity,
                refill_per_sec,
            },
            State::Bucket { tokens, last },
        ) => {
            // Continuously refill based on elapsed time, capped at capacity.
            let elapsed = now.duration_since(*last).as_secs_f64();
            *tokens = (*tokens + elapsed * refill_per_sec).min(capacity);
            *last = now;
            let cost = f64::from(cost.max(1));
            if cost <= capacity && *tokens >= cost {
                *tokens -= cost;
                Ok(())
            } else {
                let need = (cost - *tokens).max(1.0);
                Err(ceil_secs(need / refill_per_sec))
            }
        }
        // Unreachable: an entry's variant always matches its policy's kind, but
        // fail open rather than panic if it ever diverges.
        _ => Ok(()),
    }
}

/// Evict idle partitions once a shard grows past a threshold. An entry is idle
/// when its sliding window has fully drained (no hit still inside the window) or
/// its bucket has fully refilled (`tokens >= capacity`), meaning it's had no
/// recent traffic; it will be recreated identically on the next hit. Only runs
/// when a shard is large, so the common case pays nothing; the retain touches at
/// most one shard's worth of the keyspace (~total/256) under that shard's lock.
fn maybe_sweep(store: &mut HashMap<(Policy, String), State>, now: Instant) {
    // Per-shard cap: with 256 shards this bounds the whole store to a few hundred
    // thousand idle partitions under sustained key-rotation before eviction kicks
    // in, while a normally-loaded shard (well under this) never sweeps.
    const SWEEP_THRESHOLD: usize = 2048;
    if store.len() < SWEEP_THRESHOLD {
        return;
    }
    store.retain(|(policy, _), state| match (policy.kind(), state) {
        (Kind::Sliding { window, .. }, State::Sliding(hits)) => hits
            .back()
            .is_some_and(|&last| now.duration_since(last) < window),
        (
            Kind::Bucket {
                capacity,
                refill_per_sec,
            },
            State::Bucket { tokens, last },
        ) => {
            let virtual_tokens =
                (*tokens + now.duration_since(*last).as_secs_f64() * refill_per_sec).min(capacity);
            virtual_tokens < capacity
        }
        _ => true,
    });
}

/// Round a positive number of seconds up, with a floor of 1 (a 0-second
/// `Retry-After` would be meaningless to a client).
fn ceil_secs(secs: f64) -> u64 {
    let s = secs.ceil();
    if s.is_finite() && s >= 1.0 {
        s as u64
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// Client IP / partition-key extraction
// ---------------------------------------------------------------------------

/// The client IP for partitioning, from sources a client cannot forge past a
/// trusted reverse proxy: `X-Real-IP` (proxy-set), else the **rightmost**
/// `X-Forwarded-For` hop (the entry the trusted proxy appended — leftmost
/// entries are attacker-supplied), else the `ConnectInfo` peer address.
fn client_ip(req: &Request) -> String {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    crate::services::anti_cheat::client_ip(req.headers(), peer)
        // No address (e.g. in-process test transport): one shared fail-closed bucket.
        .unwrap_or_else(|| "unknown".to_string())
}

/// The partition key derived from a verified signed session by
/// [`global_middleware`]. Named route limiters run inside that middleware, so
/// carrying the fixed-size key in request extensions avoids hashing and hex
/// encoding the same claims again for every decorated route.
#[derive(Clone)]
struct VerifiedSessionPartitionKey(String);

fn partition_key(policy: Policy, req: &Request) -> String {
    // Credential, registration, recovery, mail, and OAuth-start abuse remains
    // strictly source-IP scoped. A valid-but-revoked JWT must never create a
    // fresh brute-force/mail bucket for these anonymous-facing routes.
    if matches!(
        policy,
        Policy::Login
            | Policy::Register
            | Policy::GlobalIpBackstop
            | Policy::CredentialIpAdmission
            | Policy::PrivilegedHubAdmission
            | Policy::PublicHubAdmission
    ) {
        return client_ip(req);
    }
    if let Some(credential) = req
        .extensions()
        .get::<crate::services::ad::api_token::VerifiedTeamToken>()
    {
        return credential.partition_key.clone();
    }
    if let Some(key) = req.extensions().get::<VerifiedSessionPartitionKey>() {
        return key.0.clone();
    }
    req.extensions()
        .get::<crate::middlewares::privilege_authentication::VerifiedSessionClaims>()
        .map(|claims| session_partition_key(&claims.0))
        .unwrap_or_else(|| client_ip(req))
}

/// Fixed-size identity for a signed session. Including the live-revocation stamp
/// keeps sessions issued across credential rotations in separate generations;
/// hashing avoids putting account identifiers or attacker-sized claim strings in
/// memory and Redis keys.
fn session_partition_key(claims: &crate::services::token::Claims) -> String {
    let mut digest = Sha256::new();
    digest.update(b"rsctf-rate-session-v1\0");
    digest.update((claims.sub.len() as u64).to_be_bytes());
    digest.update(claims.sub.as_bytes());
    digest.update((claims.stamp.len() as u64).to_be_bytes());
    digest.update(claims.stamp.as_bytes());
    format!("jwt:{}", hex::encode(digest.finalize()))
}

// ---------------------------------------------------------------------------
// Optional Redis-backed distributed limiter (multi-node)
// ---------------------------------------------------------------------------

/// A Redis-backed limiter shared across replicas. **Off by default** —
/// the in-process sharded store above is faster (no network hop) and correct on a
/// single node. It is only needed when running several replicas behind a load
/// balancer, where each node's independent in-process counters would each admit the
/// full quota (N nodes ⇒ up to N× the intended limit). Enabling it is what makes the
/// rate limits — and thus horizontal scaling — actually hold across nodes. Gated on
/// `RSCTF_DISTRIBUTED_RATELIMIT`; the shared cache needs no analogous switch because
/// its L2 (Redis) is already shared and `remove` clears it immediately, so an
/// invalidation propagates on the next miss everywhere and each node's L1 can only
/// lag by its ≤1 s TTL.
pub struct DistributedLimiter {
    conn: redis::aio::ConnectionManager,
}

static DISTRIBUTED: std::sync::OnceLock<DistributedLimiter> = std::sync::OnceLock::new();

/// Keep an unavailable shared limiter off the request path. The connection
/// manager deliberately reconnects in the background, but its command future
/// can otherwise wait indefinitely for that reconnect while Redis is down.
/// Falling back after this short deadline preserves availability and still
/// applies the process-local abuse ceiling.
const REDIS_COMMAND_TIMEOUT: Duration = Duration::from_millis(100);
const REDIS_CONNECT_TIMEOUT: Duration = Duration::from_millis(750);
const REDIS_FALLBACK_LOG_INTERVAL: Duration = Duration::from_secs(30);
static REDIS_FALLBACK_CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);
static REDIS_FALLBACK_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);

fn redis_key(policy: Policy, partition: &str) -> String {
    match policy.kind() {
        // Bucket state is a Redis hash. Keep it in a separate namespace from
        // fixed-window counters so a rolling upgrade never observes WRONGTYPE
        // errors from an older deployment's string value. Both remain below the
        // `rl:*` operational prefix used for inspection and cleanup.
        Kind::Bucket { .. } => format!("rl:tb:{}:{}", policy as u8, partition),
        Kind::Sliding { .. } => format!("rl:{}:{}", policy as u8, partition),
    }
}

impl DistributedLimiter {
    /// Check one shared policy in one Redis round trip. Sliding policies retain
    /// their fixed-window counter. Bucket policies use a continuously-refilled
    /// hash driven by Redis's clock, so replicas cannot disagree about elapsed
    /// time. A Redis error falls back to the process-local limiter.
    async fn check(&self, policy: Policy, ip: &str) -> Result<(), u64> {
        self.check_weighted(policy, ip, 1).await
    }

    /// Weighted counterpart to [`Self::check`]. The complete charge, refill,
    /// decision, state update, and expiry update are one Lua invocation. A denied
    /// charge does not consume tokens, matching the local bucket.
    async fn check_weighted(&self, policy: Policy, ip: &str, cost: u32) -> Result<(), u64> {
        const SLIDING_SCRIPT: &str = r"
            local c = redis.call('INCRBY', KEYS[1], ARGV[3])
            local ttl = redis.call('PTTL', KEYS[1])
            if ttl < 0 then
                redis.call('PEXPIRE', KEYS[1], ARGV[1])
                ttl = tonumber(ARGV[1])
            end
            if c > tonumber(ARGV[2]) then return ttl else return 0 end
        ";

        const BUCKET_SCRIPT: &str = r"
            local capacity = tonumber(ARGV[1])
            local refill_per_sec = tonumber(ARGV[2])
            local cost = tonumber(ARGV[3])
            local clock = redis.call('TIME')
            local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)

            local tokens = tonumber(redis.call('HGET', KEYS[1], 'tokens'))
            local last_ms = tonumber(redis.call('HGET', KEYS[1], 'last_ms'))
            if not tokens or not last_ms then
                tokens = capacity
                last_ms = now_ms
            else
                tokens = math.max(0, math.min(capacity, tokens))
                -- Never mint tokens when the wall clock moves backwards. Keep
                -- the prior timestamp until Redis's clock catches up.
                if now_ms > last_ms then
                    tokens = math.min(capacity, tokens + ((now_ms - last_ms) * refill_per_sec / 1000))
                    last_ms = now_ms
                end
            end

            local allowed = cost <= capacity and tokens >= cost
            if allowed then tokens = tokens - cost end

            redis.call('HSET', KEYS[1], 'tokens', tokens, 'last_ms', last_ms)
            local full_in_ms = math.ceil((capacity - tokens) * 1000 / refill_per_sec)
            redis.call('PEXPIRE', KEYS[1], math.max(1, full_in_ms))

            if allowed then return 0 end
            if cost > capacity then
                return math.max(1, math.ceil(capacity * 1000 / refill_per_sec))
            end
            return math.max(1, math.ceil((cost - tokens) * 1000 / refill_per_sec))
        ";

        let cost = cost.max(1);
        let key = redis_key(policy, ip);
        let mut conn = self.conn.clone();
        let result = match policy.kind() {
            Kind::Sliding { permit, window } => {
                redis_with_timeout(async {
                    redis::Script::new(SLIDING_SCRIPT)
                        .key(&key)
                        .arg(window.as_millis() as u64)
                        .arg(permit)
                        .arg(cost)
                        .invoke_async(&mut conn)
                        .await
                })
                .await
            }
            Kind::Bucket {
                capacity,
                refill_per_sec,
            } => {
                redis_with_timeout(async {
                    redis::Script::new(BUCKET_SCRIPT)
                        .key(&key)
                        .arg(capacity)
                        .arg(refill_per_sec)
                        .arg(cost)
                        .invoke_async(&mut conn)
                        .await
                })
                .await
            }
        };
        redis_or_local(result, || check_weighted(policy, ip.to_owned(), cost))
    }

    /// Check the authenticated identity ceiling and source-IP backstop in one
    /// atomic Redis invocation. The script deliberately processes Global first
    /// and returns immediately when it rejects, leaving the backstop untouched;
    /// this exactly preserves the old two-call admission ordering.
    async fn check_authenticated(&self, identity: &str, ip: &str) -> Result<(), u64> {
        const SCRIPT: &str = r"
            local function fixed_hit(key, window_ms, limit)
                local c = redis.call('INCR', key)
                local ttl = redis.call('PTTL', key)
                if ttl < 0 then
                    redis.call('PEXPIRE', key, window_ms)
                    ttl = tonumber(window_ms)
                end
                if c > tonumber(limit) then return ttl else return 0 end
            end

            local function bucket_hit(key, capacity, refill_per_sec)
                capacity = tonumber(capacity)
                refill_per_sec = tonumber(refill_per_sec)
                local clock = redis.call('TIME')
                local now_ms = tonumber(clock[1]) * 1000 + math.floor(tonumber(clock[2]) / 1000)
                local tokens = tonumber(redis.call('HGET', key, 'tokens'))
                local last_ms = tonumber(redis.call('HGET', key, 'last_ms'))
                if not tokens or not last_ms then
                    tokens = capacity
                    last_ms = now_ms
                else
                    tokens = math.max(0, math.min(capacity, tokens))
                    if now_ms > last_ms then
                        tokens = math.min(capacity, tokens + ((now_ms - last_ms) * refill_per_sec / 1000))
                        last_ms = now_ms
                    end
                end

                local allowed = tokens >= 1
                if allowed then tokens = tokens - 1 end
                redis.call('HSET', key, 'tokens', tokens, 'last_ms', last_ms)
                local full_in_ms = math.ceil((capacity - tokens) * 1000 / refill_per_sec)
                redis.call('PEXPIRE', key, math.max(1, full_in_ms))
                if allowed then return 0 end
                return math.max(1, math.ceil((1 - tokens) * 1000 / refill_per_sec))
            end

            local ttl = fixed_hit(KEYS[1], ARGV[1], ARGV[2])
            if ttl > 0 then return ttl end
            return bucket_hit(KEYS[2], ARGV[3], ARGV[4])
        ";
        let (identity_limit, identity_window_ms) = Policy::Global.fixed_window();
        let Kind::Bucket {
            capacity: ip_capacity,
            refill_per_sec: ip_refill_per_sec,
        } = Policy::GlobalIpBackstop.kind()
        else {
            return check_authenticated_local(identity.to_owned(), ip.to_owned());
        };
        let identity_key = redis_key(Policy::Global, identity);
        let ip_key = redis_key(Policy::GlobalIpBackstop, ip);
        let mut conn = self.conn.clone();
        redis_or_local(
            redis_with_timeout(async {
                redis::Script::new(SCRIPT)
                    .key(identity_key)
                    .key(ip_key)
                    .arg(identity_window_ms)
                    .arg(identity_limit)
                    .arg(ip_capacity)
                    .arg(ip_refill_per_sec)
                    .invoke_async(&mut conn)
                    .await
            })
            .await,
            || check_authenticated_local(identity.to_owned(), ip.to_owned()),
        )
    }
}

async fn redis_with_timeout(
    operation: impl Future<Output = redis::RedisResult<i64>>,
) -> redis::RedisResult<i64> {
    redis_with_timeout_for(
        operation,
        REDIS_COMMAND_TIMEOUT,
        "rate limiter Redis command timed out",
    )
    .await
}

async fn redis_with_timeout_for<T>(
    operation: impl Future<Output = redis::RedisResult<T>>,
    timeout: Duration,
    timeout_message: &'static str,
) -> redis::RedisResult<T> {
    tokio::time::timeout(timeout, operation)
        .await
        .unwrap_or_else(|_| {
            Err(redis::RedisError::from((
                redis::ErrorKind::Io,
                timeout_message,
            )))
        })
}

/// Convert the Redis script's millisecond TTL to the public whole-second
/// response. Redis failures use the supplied in-process fallback instead of
/// bypassing the limiter entirely.
fn redis_or_local(
    result: redis::RedisResult<i64>,
    local: impl FnOnce() -> Result<(), u64>,
) -> Result<(), u64> {
    let ttl_ms = match result {
        Ok(ttl_ms) => ttl_ms,
        Err(error) => {
            let now_ms = REDIS_FALLBACK_CLOCK
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64;
            if claim_redis_fallback_log_slot(
                &REDIS_FALLBACK_LAST_LOG_MS,
                now_ms.max(1),
                REDIS_FALLBACK_LOG_INTERVAL.as_millis() as u64,
            ) {
                tracing::warn!(
                    %error,
                    "distributed rate limiter unavailable; using per-process fallback"
                );
            }
            return local();
        }
    };
    if ttl_ms > 0 {
        Err((ttl_ms as u64).div_ceil(1000).max(1))
    } else {
        Ok(())
    }
}

fn claim_redis_fallback_log_slot(last_log_ms: &AtomicU64, now_ms: u64, interval_ms: u64) -> bool {
    let mut observed = last_log_ms.load(Ordering::Relaxed);
    loop {
        if observed != 0 && now_ms.saturating_sub(observed) < interval_ms {
            return false;
        }
        match last_log_ms.compare_exchange_weak(
            observed,
            now_ms,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(actual) => observed = actual,
        }
    }
}

/// Initialise the distributed limiter from a Redis URL — called once at startup when
/// `RSCTF_DISTRIBUTED_RATELIMIT` is set; afterwards [`check_async`] routes through it.
pub async fn init_distributed(redis_url: &str) -> anyhow::Result<()> {
    let client = redis::Client::open(redis_url)?;
    let conn = redis_with_timeout_for(
        crate::utils::redis::connection_manager(&client),
        REDIS_CONNECT_TIMEOUT,
        "rate limiter Redis connection timed out",
    )
    .await?;
    let _ = DISTRIBUTED.set(DistributedLimiter { conn });
    tracing::info!("rate limiter: distributed (Redis-backed) mode enabled");
    Ok(())
}

/// Check a policy for a partition key, routing through the distributed limiter when
/// one is configured (multi-node) or the in-process store otherwise (single-node —
/// the default, no network hop).
async fn check_async(policy: Policy, ip: String) -> Result<(), u64> {
    match DISTRIBUTED.get() {
        Some(d) => d.check(policy, &ip).await,
        None => check(policy, ip),
    }
}

async fn check_weighted_async(policy: Policy, key: String, cost: u32) -> Result<(), u64> {
    match DISTRIBUTED.get() {
        Some(distributed) => distributed.check_weighted(policy, &key, cost).await,
        None => check_weighted(policy, key, cost),
    }
}

/// Enforce the team-scoped A&D work budget after authentication has resolved a
/// canonical participation. Returning the normal 429 response preserves the
/// public error envelope and `Retry-After` header.
pub(crate) async fn admit_ad_submit(
    game_id: i32,
    participation_id: i32,
    distinct_plausible_flags: usize,
) -> Option<Response> {
    let cost = u32::try_from(distinct_plausible_flags.max(1)).unwrap_or(u32::MAX);
    let key = format!("game:{game_id}:participation:{participation_id}");
    check_weighted_async(Policy::AdSubmit, key, cost)
        .await
        .err()
        .map(too_many_requests)
}

fn check_authenticated_local(identity: String, ip: String) -> Result<(), u64> {
    check(Policy::Global, identity)?;
    check(Policy::GlobalIpBackstop, ip)
}

/// Check the two post-verification authenticated ceilings. Distributed mode
/// combines them into one Redis RTT; local mode intentionally keeps the original
/// ordered pair of in-memory checks.
async fn check_authenticated_async(identity: String, ip: String) -> Result<(), u64> {
    match DISTRIBUTED.get() {
        Some(d) => d.check_authenticated(&identity, &ip).await,
        None => check_authenticated_local(identity, ip),
    }
}

// ---------------------------------------------------------------------------
// Middleware / decorator API
// ---------------------------------------------------------------------------

/// The always-on Global sliding window, layered once over the whole `/api`
/// router in `server.rs`. Non-`/api` traffic (health checks, static SPA assets)
/// is never limited, matching RSCTF scoping the global limiter to `/api`.
///
/// Layer it **after** CORS so `OPTIONS` preflights don't consume quota:
/// ```ignore
/// .layer(axum::middleware::from_fn(rate_limiter::global_middleware))
/// ```
pub async fn global_middleware(
    AxumState(st): AxumState<SharedState>,
    mut req: Request,
    next: Next,
) -> Response {
    if !req.uri().path().starts_with("/api") {
        return next.run(req).await;
    }
    let ip = client_ip(&req);
    let credential = crate::middlewares::privilege_authentication::session_token(req.headers());
    if credential.is_some() {
        // This high source ceiling is deliberately separate from per-account
        // fairness. It bounds signature/DB work from rotating invalid bearer
        // floods while leaving ordinary shared-NAT event traffic unaffected.
        if let Err(retry_after) = check_async(Policy::CredentialIpAdmission, ip.clone()).await {
            return too_many_requests(retry_after);
        }
    }

    let attempted_ad = credential
        .as_deref()
        .is_some_and(crate::services::ad::api_token::is_well_formed);
    let verified_ad = if attempted_ad {
        match crate::services::ad::api_token::authenticate(
            st.pg(),
            credential
                .as_deref()
                .expect("attempted A&D token is present"),
        )
        .await
        {
            Ok(credential) => credential,
            Err(error) => return error.into_response(),
        }
    } else {
        None
    };
    if attempted_ad && verified_ad.is_none() {
        req.extensions_mut()
            .insert(crate::services::ad::api_token::RejectedTeamToken);
    }
    // A syntactically valid A&D credential has already received its definitive
    // DB decision above. Do not reinterpret a rejected one as a session JWT.
    let verified_session = if attempted_ad {
        None
    } else {
        credential.and_then(|token| st.token.verify(&token).ok())
    };
    if let Some(verified) = verified_ad {
        let key = verified.partition_key.clone();
        req.extensions_mut().insert(verified);
        if let Err(retry_after) = check_authenticated_async(key, ip).await {
            return too_many_requests(retry_after);
        }
    } else if let Some(claims) = verified_session {
        let key = session_partition_key(&claims);
        req.extensions_mut()
            .insert(crate::middlewares::privilege_authentication::VerifiedSessionClaims(claims));
        req.extensions_mut()
            .insert(VerifiedSessionPartitionKey(key.clone()));
        // Reject an overactive account before recording it in the larger
        // shared-source backstop. This bounds backstop memory by traffic that
        // already passed its per-account quota.
        if let Err(retry_after) = check_authenticated_async(key, ip).await {
            return too_many_requests(retry_after);
        }
    } else {
        if let Err(retry_after) = check_async(Policy::Global, ip).await {
            return too_many_requests(retry_after);
        }
    }
    next.run(req).await
}

/// Decorate a single route handler with a named policy — the axum analogue of
/// RSCTF's `[EnableRateLimiting(policy)]` attribute:
/// ```ignore
/// use crate::middlewares::rate_limiter::{limited, Policy};
/// .route("/api/account/login", limited(Policy::Login, post(login)))
/// ```
/// The wrapped handler is checked against `policy` (partitioned by verified
/// account when authenticated, otherwise by client IP) in addition to the
/// always-on Global window; a denial short-circuits with a 429 + `Retry-After`
/// and the handler never runs.
pub fn limited(policy: Policy, handler: MethodRouter<SharedState>) -> MethodRouter<SharedState> {
    handler.layer(axum::middleware::from_fn(
        move |req: Request, next: Next| run_policy(policy, req, next),
    ))
}

/// Per-route policy check backing [`limited`].
async fn run_policy(policy: Policy, req: Request, next: Next) -> Response {
    if let Err(retry_after) = check_async(policy, partition_key(policy, &req)).await {
        return too_many_requests(retry_after);
    }
    next.run(req).await
}

/// Build the 429 response: RSCTF's `RequestResponse { title, status }` envelope
/// (via [`crate::utils::shared::MessageResponse`]) plus a `Retry-After` header in
/// whole seconds.
fn too_many_requests(retry_after: u64) -> Response {
    let mut resp = crate::utils::shared::MessageResponse::new(
        format!("Too many requests. Please retry after {retry_after} seconds."),
        429,
    )
    .into_response();
    if let Ok(val) = HeaderValue::from_str(&retry_after.to_string()) {
        resp.headers_mut().insert(header::RETRY_AFTER, val);
    }
    resp
}

#[cfg(test)]
#[path = "rate_limiter_tests.rs"]
mod tests;
