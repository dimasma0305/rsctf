//! utils/single_flight.rs — async request coalescing.
//!
//! When many callers request the same key at once — e.g. a cache entry just
//! expired and every in-flight request misses together — only the FIRST runs the
//! expensive computation; the rest await its cloned result. This turns a
//! cache-expiry *thundering herd* (N concurrent recomputes all dogpiling the DB)
//! into a single recompute per key, mirroring what RSCTF's `CacheHelper` does
//! over its in-process cache. The store is a `std::sync::Mutex` held only for the
//! O(1) leader/follower bookkeeping — never across the `.await`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

pub struct SingleFlight<T> {
    inflight: Mutex<HashMap<String, broadcast::Sender<T>>>,
}

const LEADER_TIMEOUT: Duration = Duration::from_secs(15);

type CoalesceMap = HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>;
static COALESCE_FLIGHTS: std::sync::LazyLock<Mutex<CoalesceMap>> =
    std::sync::LazyLock::new(Default::default);
const MAX_COALESCE_KEYS: usize = 1_024;

impl<T: Clone + Default + Send + 'static> SingleFlight<T> {
    pub fn new() -> Self {
        Self {
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Run `f` for `key`, coalescing concurrent callers on the same key: the
    /// first caller starts a detached leader task and the rest await its cloned
    /// result. Detaching is intentional: cancelling the HTTP request that won
    /// leadership must not cancel the shared cache fill.
    pub async fn run<F, Fut>(&'static self, key: &str, f: F) -> T
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        // Every caller subscribes before the leader starts, so a very fast
        // recompute cannot publish between map lookup and subscription.
        let (mut receiver, leader) = {
            let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(key) {
                Some(tx) => (tx.subscribe(), None),
                None => {
                    let (tx, _) = broadcast::channel(1);
                    let receiver = tx.subscribe();
                    map.insert(key.to_string(), tx);
                    (receiver, Some(f))
                }
            }
        };

        if let Some(f) = leader {
            let key = key.to_owned();
            // The recompute must outlive the HTTP request that happened to win
            // leadership. Browsers and load clients can disconnect at their own
            // deadline; cancelling that waiter must not cancel the shared DB
            // transaction and wake every follower into a recompute herd.
            tokio::spawn(async move {
                struct Cleanup<T: 'static> {
                    map: &'static Mutex<HashMap<String, broadcast::Sender<T>>>,
                    key: String,
                    armed: bool,
                }
                impl<T> Drop for Cleanup<T> {
                    fn drop(&mut self) {
                        if self.armed {
                            self.map
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .remove(&self.key);
                        }
                    }
                }
                let mut cleanup = Cleanup {
                    map: &self.inflight,
                    key: key.clone(),
                    armed: true,
                };
                let value = match tokio::time::timeout(LEADER_TIMEOUT, f()).await {
                    Ok(value) => value,
                    Err(_) => {
                        tracing::warn!(single_flight_key = %key, "single-flight recompute timed out");
                        T::default()
                    }
                };
                // Publish + remove under one short lock so a late caller cannot
                // subscribe after the one-slot broadcast has fired.
                let sender = {
                    let mut map = self
                        .inflight
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let sender = map.remove(&key);
                    // Disarm while the map is still locked. Otherwise a new
                    // flight can be inserted between removal and Drop, and the
                    // cleanup guard would accidentally delete that newer flight.
                    cleanup.armed = false;
                    sender
                };
                if let Some(sender) = sender {
                    let _ = sender.send(value);
                }
            });
        }

        // A spawned task can disappear only on panic/runtime shutdown. Timeout
        // and channel-close use the output type's explicit failure default
        // (`None` for cache fills, `false` for completion dispositions).
        receiver.recv().await.unwrap_or_default()
    }
}

impl<T: Clone + Default + Send + 'static> Default for SingleFlight<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{coalesce, SingleFlight, COALESCE_FLIGHTS};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_leader_does_not_cancel_or_duplicate_the_recompute() {
        let flight: &'static SingleFlight<Option<usize>> = Box::leak(Box::new(SingleFlight::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let first_calls = calls.clone();
        let leader = tokio::spawn(flight.run("cancel-safe", move || async move {
            first_calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(40)).await;
            Some(7)
        }));

        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        leader.abort();

        let fallback_calls = calls.clone();
        let value = flight
            .run("cancel-safe", move || async move {
                fallback_calls.fetch_add(1, Ordering::SeqCst);
                Some(9)
            })
            .await;
        assert_eq!(value, Some(7));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn coalesce_releases_an_idle_lock_after_the_last_guard() {
        let key = "single-flight-test-idle-key";
        {
            let _guard = coalesce(key).await;
            assert!(COALESCE_FLIGHTS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(key)
                .and_then(std::sync::Weak::upgrade)
                .is_some());
        }
        assert!(COALESCE_FLIGHTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .is_some_and(|lock| lock.upgrade().is_none()));
    }
}

/// Owned per-key guard. The key map stores only weak references, so cancellation
/// while waiting cannot retain a user/game/container lock for the process life.
pub struct CoalesceGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

/// A per-`key` coalescing lock. The first caller on a key proceeds while later callers
/// await here, then re-check their cache — suppressing a thundering herd of identical
/// recomputes when a cache TTL expires (used by the A&D scoreboard + timelines).
pub async fn coalesce(key: &str) -> CoalesceGuard {
    let lock = {
        let mut map = COALESCE_FLIGHTS.lock().unwrap_or_else(|e| e.into_inner());
        // Dead weak keys cost only their bounded key metadata. Sweep before
        // reaching the cap; active entries are never evicted.
        if map.len() >= MAX_COALESCE_KEYS {
            map.retain(|_, lock| lock.strong_count() > 0);
        }
        match map.get(key).and_then(std::sync::Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                map.insert(key.to_string(), std::sync::Arc::downgrade(&lock));
                lock
            }
        }
    };
    CoalesceGuard {
        _guard: lock.lock_owned().await,
    }
}

/// Cross-replica mutex backed by a PostgreSQL transaction-scoped advisory lock.
/// Keep this guard alive across the protected read/create/persist sequence. An
/// explicit `release` commits the otherwise-empty transaction; dropping it on an
/// error queues a rollback and releases the lock with the pooled connection.
pub struct PgAdvisoryLock {
    transaction: Option<sqlx::Transaction<'static, sqlx::Postgres>>,
    _concurrency_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// Session-scoped advisory lock for operations that must serialize external
/// work without keeping a database transaction open. The pooled connection is
/// marked close-on-drop before locking, so cancellation cannot return a still-
/// locked PostgreSQL session to the pool.
pub struct PgSessionAdvisoryLock {
    connection: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
    lock_key: i64,
    _concurrency_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

fn advisory_lock_key(key: &str) -> i64 {
    let digest = Sha256::digest(key.as_bytes());
    i64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is exactly eight bytes"),
    )
}

impl PgAdvisoryLock {
    pub async fn acquire(pool: &sqlx::PgPool, key: &str) -> anyhow::Result<Self> {
        Self::acquire_with_permit(pool, key, None).await
    }

    /// Advisory lock for a sequence that calls an external container runtime.
    /// Bound the number of held DB connections per replica so a provisioning burst
    /// cannot consume the connection pool while image pulls are in flight. The
    /// default still leaves most of the standard 32-connection pool available.
    pub async fn acquire_provisioning(pool: &sqlx::PgPool, key: &str) -> anyhow::Result<Self> {
        static GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::LazyLock::new(|| {
                let permits = std::env::var("RSCTF_PROVISIONING_CONCURRENCY")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(4);
                std::sync::Arc::new(tokio::sync::Semaphore::new(permits))
            });
        let permit = GATE.clone().acquire_owned().await?;
        Self::acquire_with_permit(pool, key, Some(permit)).await
    }

    /// Definition saves, publication fences, and explicit rollouts can issue
    /// additional queries while retaining their transaction-scoped lock. Keep
    /// their admission independent from provisioning because test-container
    /// creation legitimately nests both lock classes. A small fixed gate bounds
    /// retained pool connections without making that nesting self-deadlock.
    pub async fn acquire_definition(pool: &sqlx::PgPool, key: &str) -> anyhow::Result<Self> {
        static GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(4)));
        let permit = GATE.clone().acquire_owned().await?;
        Self::acquire_with_permit(pool, key, Some(permit)).await
    }

    /// Distributed lock for mutable image-tag builds.
    ///
    /// A build can be reached from inside a container-provisioning critical
    /// section. It therefore needs a distinct one-at-a-time gate: recursively
    /// taking the provisioning semaphore would self-deadlock when its configured
    /// concurrency is one. The advisory lock still serializes the same
    /// challenge across replicas. A session lock is used because the image
    /// operation is slow external I/O and must not hold a DB transaction open.
    pub async fn acquire_build(
        pool: &sqlx::PgPool,
        key: &str,
    ) -> anyhow::Result<PgSessionAdvisoryLock> {
        static GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
        let permit = GATE.clone().acquire_owned().await?;
        PgSessionAdvisoryLock::acquire_with_permit(pool, key, Some(permit)).await
    }

    async fn acquire_with_permit(
        pool: &sqlx::PgPool,
        key: &str,
        concurrency_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> anyhow::Result<Self> {
        let lock_key = advisory_lock_key(key);
        let mut transaction = super::database::begin_sqlx_transaction(pool).await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock_key)
            .execute(&mut *transaction)
            .await?;
        Ok(Self {
            transaction: Some(transaction),
            _concurrency_permit: concurrency_permit,
        })
    }

    pub async fn release(mut self) -> anyhow::Result<()> {
        if let Some(transaction) = self.transaction.take() {
            transaction.commit().await?;
        }
        Ok(())
    }

    /// Run short database mutations in the same transaction that owns the lock.
    /// This avoids reserving a second pool connection while waiting on a guarded
    /// write, which can deadlock when many distinct keys are active at once.
    pub(crate) fn transaction_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Postgres> {
        self.transaction
            .as_mut()
            .expect("advisory lock transaction is live until release")
    }
}

impl PgSessionAdvisoryLock {
    async fn acquire_with_permit(
        pool: &sqlx::PgPool,
        key: &str,
        concurrency_permit: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> anyhow::Result<Self> {
        let lock_key = advisory_lock_key(key);
        let mut connection = pool.acquire().await?;
        connection.close_on_drop();
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(lock_key)
            .execute(&mut *connection)
            .await?;
        Ok(Self {
            connection: Some(connection),
            lock_key,
            _concurrency_permit: concurrency_permit,
        })
    }

    pub async fn release(mut self) -> anyhow::Result<()> {
        let Some(mut connection) = self.connection.take() else {
            return Ok(());
        };
        let result = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(self.lock_key)
            .fetch_one(&mut *connection)
            .await;
        // `close_on_drop` ensures an unlock error or cancellation cannot leak
        // session-level lock state through a reused pool connection.
        drop(connection);
        match result {
            Ok(true) => Ok(()),
            Ok(false) => anyhow::bail!("PostgreSQL session advisory lock was not owned"),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn connection_mut(&mut self) -> &mut sqlx::PgConnection {
        &mut **self
            .connection
            .as_mut()
            .expect("advisory lock session is live until release")
    }
}
