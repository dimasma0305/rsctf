//! Bounded per-process single-flight for identical live-session lease checks.

use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

const MAX_CONCURRENT_LEASE_QUERIES: usize = 64;
static LEASE_QUERY_WORK: std::sync::LazyLock<tokio::sync::Semaphore> =
    std::sync::LazyLock::new(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_LEASE_QUERIES));

struct CachedLease {
    checked_at: tokio::time::Instant,
    valid: bool,
}

pub(super) struct LeaseCache<K> {
    entries: DashMap<K, Arc<tokio::sync::Mutex<CachedLease>>>,
    maximum: usize,
    freshness: Duration,
}

impl<K> LeaseCache<K>
where
    K: Clone + Eq + Hash,
{
    pub(super) fn new(maximum: usize, freshness: Duration) -> Self {
        Self {
            entries: DashMap::new(),
            maximum,
            freshness,
        }
    }

    pub(super) async fn validate<F, Fut>(&self, key: K, check: F) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = bool>,
    {
        if self.entries.len() >= self.maximum && !self.entries.contains_key(&key) {
            self.sweep_stale();
            if self.entries.len() >= self.maximum {
                return run_bounded_check(check).await;
            }
        }
        let cell = self
            .entries
            .entry(key)
            .or_insert_with(|| {
                Arc::new(tokio::sync::Mutex::new(CachedLease {
                    checked_at: tokio::time::Instant::now() - self.freshness,
                    valid: false,
                }))
            })
            .clone();
        let mut cached = cell.lock().await;
        if cached.checked_at.elapsed() < self.freshness {
            return cached.valid;
        }
        cached.valid = run_bounded_check(check).await;
        cached.checked_at = tokio::time::Instant::now();
        cached.valid
    }

    fn sweep_stale(&self) {
        let retention = self
            .freshness
            .saturating_mul(240)
            .max(Duration::from_secs(60));
        self.entries.retain(|_, value| {
            Arc::strong_count(value) > 1
                || value
                    .try_lock()
                    .map_or(true, |cached| cached.checked_at.elapsed() < retention)
        });
    }
}

async fn run_bounded_check<F, Fut>(check: F) -> bool
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = bool>,
{
    let Ok(_permit) = LEASE_QUERY_WORK.try_acquire() else {
        return false;
    };
    tokio::time::timeout(Duration::from_secs(3), check())
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_identical_checks_have_one_owner() {
        let cache = Arc::new(LeaseCache::new(8, Duration::from_secs(1)));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let cache = cache.clone();
            let calls = calls.clone();
            tasks.push(tokio::spawn(async move {
                cache
                    .validate(7, || async move {
                        calls.fetch_add(1, Ordering::Relaxed);
                        tokio::task::yield_now().await;
                        true
                    })
                    .await
            }));
        }
        for task in tasks {
            assert!(task.await.unwrap());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
