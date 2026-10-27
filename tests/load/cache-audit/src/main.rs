//! Fixed-arrival-rate component comparison, not an HTTP/platform benchmark.
//! Compile the actual memory cache; candidates cannot change credential semantics.
#![allow(dead_code, unused_imports)]
use bytes::Bytes;
use serde_json::json;
use std::{
    hash::{BuildHasher, RandomState},
    sync::Arc,
    time::{Duration, Instant},
};

#[path = "../../../../src/services/cache.rs"]
mod cache;
#[path = "../../../../src/utils/redis.rs"]
pub(crate) mod redis_helpers;
// Only TieredCache needs these symbols. This comparison exercises InMemoryCache
// directly; no Redis or observability method is called by the measured path.
mod services {
    pub(crate) mod observability {
        use std::sync::atomic::AtomicU64;
        pub struct UnusedTimer;
        impl UnusedTimer {
            pub fn timer(&self) -> Self {
                unreachable!("tiered cache is not benchmarked")
            }
        }
        pub static CACHE_READ: UnusedTimer = UnusedTimer;
        pub static CACHE_L2_READ: UnusedTimer = UnusedTimer;
        pub static CACHE_L1_HITS: AtomicU64 = AtomicU64::new(0);
        pub static CACHE_L1_MISSES: AtomicU64 = AtomicU64::new(0);
        pub static CACHE_L2_HITS: AtomicU64 = AtomicU64::new(0);
        pub static CACHE_L2_MISSES: AtomicU64 = AtomicU64::new(0);
    }
}
mod utils {
    pub(crate) use crate::redis_helpers as redis;
}
use cache::{Cache, InMemoryCache};

const CAPACITY: usize = 4096;
const BYTES: usize = 32 * 1024 * 1024;
const SHARDS: usize = 16;
const WORKERS: usize = 4;

enum Candidate {
    Baseline(InMemoryCache),
    Sharded(cache::response::ResponseCache),
    Moka(moka::future::Cache<String, Bytes>),
}
impl Candidate {
    fn new(mode: &str, ttl: Duration) -> Self {
        match mode {
            "baseline" => Self::Baseline(InMemoryCache::with_limits(CAPACITY, BYTES)),
            "sharded" => Self::Sharded(cache::response::ResponseCache::new()),
            "moka" => Self::Moka(
                moka::future::Cache::builder()
                    // Match the entry limit with fixed-size benchmark values. A weighted
                    // cache alone would not independently preserve both hard limits.
                    .max_capacity(CAPACITY as u64)
                    .time_to_live(ttl)
                    .build(),
            ),
            _ => panic!("expected baseline, sharded or moka"),
        }
    }
    async fn get(&self, key: &str) -> Option<Bytes> {
        match self {
            Self::Baseline(cache) => cache.get(key).await,
            Self::Sharded(cache) => cache.get(key).await,
            Self::Moka(cache) => cache.get(key).await,
        }
    }
    async fn set(&self, key: &str, value: &[u8], ttl: Duration) {
        match self {
            Self::Baseline(cache) => cache.set(key, value, Some(ttl)).await,
            Self::Sharded(cache) => cache.set(key, value, Some(ttl)).await,
            Self::Moka(cache) => {
                cache
                    .insert(key.to_owned(), Bytes::copy_from_slice(value))
                    .await
            }
        }
    }
}

fn usage() -> (f64, i64) {
    let mut value = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // getrusage initializes the provided structure; only read it on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, value.as_mut_ptr()) },
        0
    );
    let value = unsafe { value.assume_init() };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    (
        seconds(value.ru_utime) + seconds(value.ru_stime),
        value.ru_maxrss,
    )
}
fn distribution(mut values: Vec<u64>) -> serde_json::Value {
    values.sort_unstable();
    let percentile =
        |p: f64| values[((values.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0;
    json!({"avg_us": values.iter().map(|v| *v as f64).sum::<f64>() / values.len() as f64 / 1000.0,
        "p50_us": percentile(0.5), "p90_us": percentile(0.9), "p95_us": percentile(0.95),
        "p99_us": percentile(0.99), "max_us": percentile(1.0)})
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).expect("mode");
    let scenario = args.get(2).expect("churn or expiry");
    assert!(matches!(scenario.as_str(), "churn" | "expiry"));
    let rate = std::env::var("CACHE_AUDIT_RATE")
        .ok()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(40_000);
    let seconds = std::env::var("CACHE_AUDIT_SECONDS")
        .ok()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(6);
    assert!(rate >= WORKERS && seconds > 0 && rate * seconds <= 10_000_000);
    let ttl = if scenario == "expiry" {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(60)
    };
    let cache = Arc::new(Candidate::new(mode, ttl));
    let keys: Arc<Vec<String>> = Arc::new(
        (0..CAPACITY * 4)
            .map(|i| format!("response:{i:05}"))
            .collect(),
    );
    let payload = Arc::new(vec![0x5a; 1024]);
    for key in keys.iter().take(CAPACITY) {
        cache.set(key, &payload, ttl).await;
    }
    // Settle Moka maintenance before comparing the same steady-state schedule.
    if let Candidate::Moka(cache) = &*cache {
        cache.run_pending_tasks().await;
    }
    let cpu_start = usage().0;
    let started = Instant::now() + Duration::from_millis(30);
    let mut workers = Vec::new();
    let count = rate * seconds / WORKERS;
    for worker in 0..WORKERS {
        let cache = cache.clone();
        let keys = keys.clone();
        let payload = payload.clone();
        workers.push(tokio::spawn(async move {
            let mut service = Vec::with_capacity(count);
            let mut total = Vec::with_capacity(count);
            let mut hits = 0usize;
            let mut reads = 0usize;
            let mut rng = worker as u64 + 1;
            for index in 0..count {
                let scheduled = started
                    + Duration::from_nanos(
                        ((index * WORKERS + worker) as u64 * 1_000_000_000) / rate as u64,
                    );
                tokio::time::sleep_until(scheduled.into()).await;
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let key = &keys[rng as usize % keys.len()];
                let before = Instant::now();
                if rng % 5 == 0 {
                    cache.set(key, &payload, ttl).await;
                } else {
                    reads += 1;
                    if let Some(value) = cache.get(key).await {
                        assert_eq!(value.len(), 1024);
                        assert_eq!(value[0], 0x5a);
                        hits += 1;
                    }
                }
                let after = Instant::now();
                service.push(after.duration_since(before).as_nanos() as u64);
                total.push(after.duration_since(scheduled).as_nanos() as u64);
            }
            (service, total, hits, reads)
        }));
    }
    let mut service = Vec::new();
    let mut total = Vec::new();
    let mut hits = 0;
    let mut reads = 0;
    for worker in workers {
        let (s, t, h, r) = worker.await.unwrap();
        service.extend(s);
        total.extend(t);
        hits += h;
        reads += r;
    }
    let elapsed = started.elapsed().as_secs_f64();
    let (cpu_end, rss_kib) = usage();
    println!(
        "{}",
        json!({"mode":mode,"scenario":scenario,"rate":rate,"seconds":seconds,
        "operations":service.len(),"elapsed_seconds":elapsed,"cpu_seconds":cpu_end-cpu_start,
        "cpu_percent":(cpu_end-cpu_start)/elapsed*100.0,"peak_rss_kib":rss_kib,
        "hit_rate":hits as f64 / reads as f64,"service":distribution(service),"scheduled":distribution(total)})
    );
}
