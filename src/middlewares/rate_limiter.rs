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
//! RSCTF is Redis-backed with a `FailOpenRateLimiter` wrapper: a Redis outage
//! degrades to "unlimited" rather than 500ing. A single-node **in-memory** store
//! is therefore the faithful fail-open behaviour for a one-replica deployment —
//! it is exactly the fallback path RSCTF takes when Redis isn't configured.
//!
//! Requests are partitioned by client IP, taken from proxy-set headers that a
//! client cannot forge past a trusted reverse proxy: `X-Real-IP`, else the
//! **rightmost** `X-Forwarded-For` hop (the one the proxy appended — leftmost
//! entries are client-supplied and spoofable, which would defeat the per-IP Login
//! brute-force ceiling), else the raw `ConnectInfo` peer address.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
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

    /// A fixed-window `(limit, window-in-ms)` approximation of this policy, used by
    /// the optional Redis-backed distributed limiter — the single-node sliding
    /// window / token bucket can't be shared across replicas, so an equivalent
    /// counter is used instead. A sliding window maps directly; a bucket maps to
    /// `(capacity, capacity / refill)` — its burst over the time to refill it.
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
            if hits.len() as u32 >= permit {
                // The oldest retained hit frees a slot when it expires.
                let oldest = *hits.front().expect("len >= permit >= 1");
                let wait = (oldest + window).saturating_duration_since(now);
                Err(ceil_secs(wait.as_secs_f64()))
            } else {
                hits.push_back(now);
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
            if *tokens >= 1.0 {
                *tokens -= 1.0;
                Ok(())
            } else {
                let need = 1.0 - *tokens;
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
        Policy::Login | Policy::Register | Policy::GlobalIpBackstop | Policy::CredentialIpAdmission
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

/// A Redis-backed fixed-window limiter shared across replicas. **Off by default** —
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

fn redis_key(policy: Policy, partition: &str) -> String {
    format!("rl:{}:{}", policy as u8, partition)
}

impl DistributedLimiter {
    /// Atomic fixed-window in one round-trip: INCR the `(policy, ip)` counter, set
    /// its expiry on the first hit of the window, and return the remaining TTL (ms)
    /// iff the count now exceeds the limit — a Lua script so there is no
    /// INCR-then-EXPIRE race (a key that never expired would ban an IP forever).
    async fn check(&self, policy: Policy, ip: &str) -> Result<(), u64> {
        const SCRIPT: &str = r"
            local c = redis.call('INCR', KEYS[1])
            if c == 1 then redis.call('PEXPIRE', KEYS[1], ARGV[1]) end
            if c > tonumber(ARGV[2]) then return redis.call('PTTL', KEYS[1]) else return 0 end
        ";
        let (limit, window_ms) = policy.fixed_window();
        let key = redis_key(policy, ip);
        let mut conn = self.conn.clone();
        // Fail-open: a Redis blip must not lock every client out of the platform.
        redis_result(
            redis::Script::new(SCRIPT)
                .key(key)
                .arg(window_ms)
                .arg(limit)
                .invoke_async(&mut conn)
                .await,
        )
    }

    /// Check the authenticated identity ceiling and source-IP backstop in one
    /// atomic Redis invocation. The script deliberately processes Global first
    /// and returns immediately when it rejects, leaving the backstop untouched;
    /// this exactly preserves the old two-call ordering and counter semantics.
    async fn check_authenticated(&self, identity: &str, ip: &str) -> Result<(), u64> {
        const SCRIPT: &str = r"
            local function hit(key, window_ms, limit)
                local c = redis.call('INCR', key)
                if c == 1 then redis.call('PEXPIRE', key, window_ms) end
                if c > tonumber(limit) then return redis.call('PTTL', key) else return 0 end
            end

            local ttl = hit(KEYS[1], ARGV[1], ARGV[2])
            if ttl > 0 then return ttl end
            return hit(KEYS[2], ARGV[3], ARGV[4])
        ";
        let (identity_limit, identity_window_ms) = Policy::Global.fixed_window();
        let (ip_limit, ip_window_ms) = Policy::GlobalIpBackstop.fixed_window();
        let identity_key = redis_key(Policy::Global, identity);
        let ip_key = redis_key(Policy::GlobalIpBackstop, ip);
        let mut conn = self.conn.clone();
        redis_result(
            redis::Script::new(SCRIPT)
                .key(identity_key)
                .key(ip_key)
                .arg(identity_window_ms)
                .arg(identity_limit)
                .arg(ip_window_ms)
                .arg(ip_limit)
                .invoke_async(&mut conn)
                .await,
        )
    }
}

/// Convert the Redis script's millisecond TTL to the public whole-second
/// response. Any Redis error remains fail-open, matching the original limiter.
fn redis_result(result: redis::RedisResult<i64>) -> Result<(), u64> {
    let ttl_ms = result.unwrap_or(0);
    if ttl_ms > 0 {
        Err((ttl_ms as u64).div_ceil(1000).max(1))
    } else {
        Ok(())
    }
}

/// Initialise the distributed limiter from a Redis URL — called once at startup when
/// `RSCTF_DISTRIBUTED_RATELIMIT` is set; afterwards [`check_async`] routes through it.
pub async fn init_distributed(redis_url: &str) -> anyhow::Result<()> {
    let client = redis::Client::open(redis_url)?;
    let conn = crate::utils::redis::connection_manager(&client).await?;
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
mod tests {
    use super::*;

    fn claims(subject: &str) -> crate::services::token::Claims {
        crate::services::token::Claims {
            sub: subject.to_string(),
            role: 1,
            name: "player".to_string(),
            stamp: "stamp".to_string(),
            iat: 1,
            exp: i64::MAX,
        }
    }

    async fn set_redis_counter(
        conn: &mut redis::aio::ConnectionManager,
        key: &str,
        count: u32,
        ttl_ms: u64,
    ) {
        redis::cmd("SET")
            .arg(key)
            .arg(count)
            .arg("PX")
            .arg(ttl_ms)
            .query_async::<()>(conn)
            .await
            .unwrap();
    }

    async fn redis_counter(conn: &mut redis::aio::ConnectionManager, key: &str) -> u32 {
        redis::cmd("GET").arg(key).query_async(conn).await.unwrap()
    }

    #[test]
    fn authenticated_partitions_do_not_share_a_nat_bucket() {
        let mut first = Request::builder()
            .header("x-real-ip", "192.0.2.10")
            .body(axum::body::Body::empty())
            .unwrap();
        first.extensions_mut().insert(
            crate::middlewares::privilege_authentication::VerifiedSessionClaims(claims("user-a")),
        );
        first.extensions_mut().insert(ConnectInfo(
            "192.0.2.10:1234".parse::<SocketAddr>().unwrap(),
        ));
        let mut second = Request::builder()
            .header("x-real-ip", "192.0.2.10")
            .body(axum::body::Body::empty())
            .unwrap();
        second.extensions_mut().insert(
            crate::middlewares::privilege_authentication::VerifiedSessionClaims(claims("user-b")),
        );
        second.extensions_mut().insert(ConnectInfo(
            "192.0.2.10:5678".parse::<SocketAddr>().unwrap(),
        ));
        assert_eq!(partition_key(Policy::Submit, &first).len(), 68);
        assert_eq!(partition_key(Policy::Submit, &second).len(), 68);
        assert_ne!(
            partition_key(Policy::Submit, &first),
            partition_key(Policy::Submit, &second)
        );
        assert_eq!(
            partition_key(Policy::Login, &first),
            partition_key(Policy::Login, &second)
        );
        assert_eq!(partition_key(Policy::Register, &first), "192.0.2.10");
    }

    #[test]
    fn session_partition_binds_subject_and_security_stamp_without_exposing_either() {
        let a = claims("user-a");
        let mut rotated = a.clone();
        rotated.stamp = "stamp-2".to_string();
        let key = session_partition_key(&a);
        assert_eq!(key.len(), 68);
        assert!(key.starts_with("jwt:"));
        assert!(!key.contains(&a.sub));
        assert!(!key.contains(&a.stamp));
        assert_ne!(key, session_partition_key(&rotated));
        assert_ne!(key, session_partition_key(&claims("user-b")));
    }

    #[test]
    fn named_policy_reuses_verified_session_partition_key() {
        let session = claims("user-a");
        let expected = session_partition_key(&session);
        let mut request = Request::builder()
            .header("x-real-ip", "192.0.2.10")
            .body(axum::body::Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(crate::middlewares::privilege_authentication::VerifiedSessionClaims(session));
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.10:1234".parse::<SocketAddr>().unwrap(),
        ));

        // The fallback remains available to callers that construct the verified
        // claims extension without passing through global_middleware.
        assert_eq!(partition_key(Policy::Submit, &request), expected);

        let cached = "jwt:already-computed".to_string();
        request
            .extensions_mut()
            .insert(VerifiedSessionPartitionKey(cached.clone()));
        assert_eq!(partition_key(Policy::Submit, &request), cached);
        // Anonymous-facing policies must remain source-IP partitioned even when a
        // verified session key is present.
        assert_eq!(partition_key(Policy::Login, &request), "192.0.2.10");
    }

    #[test]
    fn redis_result_rounds_retry_after_and_fails_open() {
        assert_eq!(redis_result(Ok(1)), Err(1));
        assert_eq!(redis_result(Ok(1_001)), Err(2));
        assert_eq!(redis_result(Ok(0)), Ok(()));
        assert_eq!(redis_result(Ok(-1)), Ok(()));

        let unavailable = redis::RedisError::from((redis::ErrorKind::Io, "test outage"));
        assert_eq!(redis_result(Err(unavailable)), Ok(()));
    }

    #[test]
    fn local_authenticated_check_short_circuits_before_ip_backstop() {
        let identity = "test-local-identity-denied".to_string();
        let ip = "test-local-ip-not-counted".to_string();
        let (limit, _) = Policy::Global.fixed_window();
        let now = Instant::now();
        {
            let mut shard = shard_for(Policy::Global, &identity)
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            shard.insert(
                (Policy::Global, identity.clone()),
                State::Sliding(VecDeque::from(vec![now; limit as usize])),
            );
        }

        assert!(check_authenticated_local(identity.clone(), ip.clone()).is_err());
        let ip_was_counted = shard_for(Policy::GlobalIpBackstop, &ip)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&(Policy::GlobalIpBackstop, ip.clone()));
        assert!(!ip_was_counted);

        shard_for(Policy::Global, &identity)
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(Policy::Global, identity));
    }

    #[test]
    fn sweep_evicts_buckets_that_refilled_while_idle() {
        let now = Instant::now();
        let mut store = HashMap::new();
        for index in 0..2_048 {
            store.insert(
                (Policy::Submit, format!("idle-{index}")),
                State::Bucket {
                    tokens: 0.0,
                    last: now - Duration::from_secs(120),
                },
            );
        }
        maybe_sweep(&mut store, now);
        assert!(store.is_empty());
    }

    #[test]
    fn high_source_ceilings_have_constant_size_state() {
        for policy in [Policy::GlobalIpBackstop, Policy::CredentialIpAdmission] {
            assert!(matches!(policy.kind(), Kind::Bucket { .. }));
            assert_eq!(policy.fixed_window().1, 60_000);
        }
    }

    /// Two `DistributedLimiter` instances = two replicas sharing one Redis. Proves
    /// the whole point of the distributed limiter: N nodes enforce ONE combined
    /// quota, not N independent ones (two in-process stores would each admit `limit`,
    /// i.e. `2 × limit` total — the per-replica bug this fixes). Runs only when
    /// `RSCTF_TEST_REDIS_URL` points at a reachable Redis; otherwise it's a no-op.
    #[tokio::test]
    async fn distributed_limiter_shares_one_counter_across_replicas() {
        let Ok(url) = std::env::var("RSCTF_TEST_REDIS_URL") else {
            return;
        };
        // The opt-in live test keeps Redis's short defaults so a stalled test
        // server fails promptly; production construction uses the shared helper.
        let connect = || async {
            redis::Client::open(url.as_str())
                .unwrap()
                .get_connection_manager()
                .await
                .unwrap()
        };
        let node_a = DistributedLimiter {
            conn: connect().await,
        };
        let node_b = DistributedLimiter {
            conn: connect().await,
        };

        let ip = "test_two_replica_client";
        let key = format!("rl:{}:{}", Policy::Global as u8, ip);
        let mut admin = connect().await;
        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut admin)
            .await
            .unwrap();

        let (limit, _) = Policy::Global.fixed_window(); // 150 / 60s
        let mut allowed = 0u32;
        for i in 0..(limit + 40) {
            // Alternate replicas — requests are spread across both nodes.
            let node = if i % 2 == 0 { &node_a } else { &node_b };
            if node.check(Policy::Global, ip).await.is_ok() {
                allowed += 1;
            }
        }

        // Exactly `limit` allowed IN TOTAL across BOTH replicas (a shared counter),
        // NOT `limit` per replica — that's the multi-node correctness guarantee.
        assert_eq!(
            allowed, limit,
            "distributed limiter must enforce one combined quota across replicas"
        );

        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut admin)
            .await
            .unwrap();
    }

    /// The batched script must be observationally identical to the old ordered
    /// pair of Redis checks: Global always increments first, an identity denial
    /// leaves the backstop unchanged, and a backstop denial retains both hits.
    #[tokio::test]
    async fn distributed_authenticated_check_preserves_order_and_counters() {
        let Ok(url) = std::env::var("RSCTF_TEST_REDIS_URL") else {
            return;
        };
        // The opt-in live test keeps Redis's short defaults so a stalled test
        // server fails promptly; production construction uses the shared helper.
        let connect = || async {
            redis::Client::open(url.as_str())
                .unwrap()
                .get_connection_manager()
                .await
                .unwrap()
        };
        let limiter = DistributedLimiter {
            conn: connect().await,
        };
        let mut admin = connect().await;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let (identity_limit, _) = Policy::Global.fixed_window();
        let (ip_limit, _) = Policy::GlobalIpBackstop.fixed_window();

        let denied_identity = format!("batch-denied-identity-{nonce}");
        let untouched_ip = format!("batch-untouched-ip-{nonce}");
        let denied_identity_key = redis_key(Policy::Global, &denied_identity);
        let untouched_ip_key = redis_key(Policy::GlobalIpBackstop, &untouched_ip);
        set_redis_counter(&mut admin, &denied_identity_key, identity_limit, 20_000).await;
        set_redis_counter(&mut admin, &untouched_ip_key, 41, 50_000).await;

        let retry = limiter
            .check_authenticated(&denied_identity, &untouched_ip)
            .await
            .unwrap_err();
        assert!((1..=20).contains(&retry));
        assert_eq!(
            redis_counter(&mut admin, &denied_identity_key).await,
            identity_limit + 1
        );
        assert_eq!(
            redis_counter(&mut admin, &untouched_ip_key).await,
            41,
            "identity denial must short-circuit before the IP counter"
        );

        let allowed_identity = format!("batch-allowed-identity-{nonce}");
        let denied_ip = format!("batch-denied-ip-{nonce}");
        let allowed_identity_key = redis_key(Policy::Global, &allowed_identity);
        let denied_ip_key = redis_key(Policy::GlobalIpBackstop, &denied_ip);
        set_redis_counter(
            &mut admin,
            &allowed_identity_key,
            identity_limit - 1,
            50_000,
        )
        .await;
        set_redis_counter(&mut admin, &denied_ip_key, ip_limit, 7_000).await;

        let retry = limiter
            .check_authenticated(&allowed_identity, &denied_ip)
            .await
            .unwrap_err();
        assert!((1..=7).contains(&retry));
        assert_eq!(
            redis_counter(&mut admin, &allowed_identity_key).await,
            identity_limit
        );
        assert_eq!(
            redis_counter(&mut admin, &denied_ip_key).await,
            ip_limit + 1
        );

        let fresh_identity = format!("batch-fresh-identity-{nonce}");
        let fresh_ip = format!("batch-fresh-ip-{nonce}");
        let fresh_identity_key = redis_key(Policy::Global, &fresh_identity);
        let fresh_ip_key = redis_key(Policy::GlobalIpBackstop, &fresh_ip);
        limiter
            .check_authenticated(&fresh_identity, &fresh_ip)
            .await
            .unwrap();
        for key in [&fresh_identity_key, &fresh_ip_key] {
            assert_eq!(redis_counter(&mut admin, key).await, 1);
        }

        for key in [
            denied_identity_key,
            untouched_ip_key,
            allowed_identity_key,
            denied_ip_key,
            fresh_identity_key,
            fresh_ip_key,
        ] {
            redis::cmd("DEL")
                .arg(key)
                .query_async::<()>(&mut admin)
                .await
                .unwrap();
        }
    }
}
