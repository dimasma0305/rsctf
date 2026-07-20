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

/// Credential issuance may retain a roster transaction while VPN allocation or
/// reconciliation retains another transaction and performs a query. Bound the
/// outer transactions so the pool floor can reserve forward progress explicitly.
pub(crate) const ROSTER_ACCESS_CONCURRENCY: usize = 2;
/// Account deletion/update is admin-only and may retain one session plus one
/// transaction connection. One admitted operation per replica preserves pool
/// headroom while PostgreSQL still serializes the same account across replicas.
pub(crate) const ACCOUNT_LIFECYCLE_CONCURRENCY: usize = 1;
static ROSTER_ACCESS_GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(ROSTER_ACCESS_CONCURRENCY))
    });
static PROVISIONING_GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        let permits = std::env::var("RSCTF_PROVISIONING_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(4);
        std::sync::Arc::new(tokio::sync::Semaphore::new(permits))
    });
use tokio::sync::broadcast;

/// Bound operations that retain a team-roster transaction while issuing
/// follow-up queries or reconciling external credentials. Every caller takes
/// this permit before checking out PostgreSQL, so unrelated teams cannot fill
/// the pool with outer transactions waiting for nested work.
pub(crate) async fn roster_access_permit(
) -> Result<tokio::sync::OwnedSemaphorePermit, tokio::sync::AcquireError> {
    ROSTER_ACCESS_GATE.clone().acquire_owned().await
}

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
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{advisory_lock_key, coalesce, PgAdvisoryLock, SingleFlight, COALESCE_FLIGHTS};
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

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn shared_advisory_readers_exclude_a_roster_writer() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(3)
            .connect(&database_url)
            .await
            .unwrap();
        let key = format!("team-roster-test:{}", uuid::Uuid::new_v4());
        let first = PgAdvisoryLock::try_acquire_shared(&pool, &key)
            .await
            .unwrap()
            .unwrap();
        let second = PgAdvisoryLock::try_acquire_shared(&pool, &key)
            .await
            .unwrap()
            .unwrap();
        let mut writer = pool.begin().await.unwrap();
        let writer_acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(advisory_lock_key(&key))
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        assert!(!writer_acquired);
        writer.rollback().await.unwrap();

        first.release().await.unwrap();
        second.release().await.unwrap();
        let mut writer = pool.begin().await.unwrap();
        let writer_acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(advisory_lock_key(&key))
            .fetch_one(&mut *writer)
            .await
            .unwrap();
        assert!(writer_acquired);
        writer.rollback().await.unwrap();
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

/// Add one transaction-scoped lock to a transaction owned by a longer-lived
/// session lease. This is the hand-off used by roster mutations that must keep
/// their session-scoped team ownership after the short database transaction
/// commits and while external cleanup or provisioning runs.
pub(crate) async fn acquire_transaction_advisory_lock(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key: &str,
) -> anyhow::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(advisory_lock_key(key))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

pub(crate) async fn try_acquire_transaction_advisory_lock(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    key: &str,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(advisory_lock_key(key))
        .fetch_one(&mut **transaction)
        .await?)
}

impl PgAdvisoryLock {
    pub async fn acquire(pool: &sqlx::PgPool, key: &str) -> anyhow::Result<Self> {
        Self::acquire_with_permit(pool, key, None).await
    }

    /// Try to take a short shared transaction lock against an existing
    /// exclusive advisory-lock domain. Shared readers never block one another;
    /// a concurrent roster mutation makes this fail immediately so a poll does
    /// not occupy a pool connection while waiting behind credential teardown.
    pub(crate) async fn try_acquire_shared(
        pool: &sqlx::PgPool,
        key: &str,
    ) -> anyhow::Result<Option<Self>> {
        let lock_key = advisory_lock_key(key);
        let mut transaction = super::database::begin_sqlx_transaction(pool).await?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock_shared($1)")
            .bind(lock_key)
            .fetch_one(&mut *transaction)
            .await?;
        if !acquired {
            transaction.rollback().await?;
            return Ok(None);
        }
        Ok(Some(Self {
            transaction: Some(transaction),
            _concurrency_permit: None,
        }))
    }

    /// Try to take an exclusive transaction lock without waiting. This is used
    /// when a caller already owns a higher-level lock and blocking here would
    /// invert another mutation path's lock order.
    pub(crate) async fn try_acquire(
        pool: &sqlx::PgPool,
        key: &str,
    ) -> anyhow::Result<Option<Self>> {
        let lock_key = advisory_lock_key(key);
        let mut transaction = super::database::begin_sqlx_transaction(pool).await?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(lock_key)
            .fetch_one(&mut *transaction)
            .await?;
        if !acquired {
            transaction.rollback().await?;
            return Ok(None);
        }
        Ok(Some(Self {
            transaction: Some(transaction),
            _concurrency_permit: None,
        }))
    }

    /// Advisory lock for a sequence that calls an external container runtime.
    /// Bound the number of held DB connections per replica so a provisioning burst
    /// cannot consume the connection pool while image pulls are in flight. The
    /// default still leaves most of the standard 32-connection pool available.
    pub async fn acquire_provisioning(pool: &sqlx::PgPool, key: &str) -> anyhow::Result<Self> {
        let permit = PROVISIONING_GATE.clone().acquire_owned().await?;
        Self::acquire_with_permit(pool, key, Some(permit)).await
    }

    /// Acquire shared parent fences before the exclusive leaf key on one
    /// transaction. This ordering lets a mode transition retain an exclusive
    /// parent while it takes leaf locks without deadlocking a publisher that
    /// started moments earlier.
    pub(crate) async fn acquire_provisioning_below_shared(
        pool: &sqlx::PgPool,
        shared_keys: &[String],
        key: &str,
    ) -> anyhow::Result<Self> {
        let permit = PROVISIONING_GATE.clone().acquire_owned().await?;
        let mut transaction = super::database::begin_sqlx_transaction(pool).await?;
        for shared_key in shared_keys {
            sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
                .bind(advisory_lock_key(shared_key))
                .execute(&mut *transaction)
                .await?;
        }
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(advisory_lock_key(key))
            .execute(&mut *transaction)
            .await?;
        Ok(Self {
            transaction: Some(transaction),
            _concurrency_permit: Some(permit),
        })
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

    /// Runtime eligibility transitions retain their cross-replica fence while
    /// taking game/definition locks and reconciling external runtimes. Admit
    /// one such outer operation per replica before checking out PostgreSQL so
    /// distinct challenges cannot fill a small pool with transactions that all
    /// need a second connection to make progress. This gate stays independent
    /// from definition and provisioning because transition operations nest both.
    pub async fn acquire_transition(pool: &sqlx::PgPool, key: &str) -> anyhow::Result<Self> {
        static GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
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

    /// Acquire another transaction-scoped advisory lock on this guard's existing
    /// connection. Callers that need an ordered lock hierarchy can retain one
    /// transaction (and one pool connection) instead of nesting independent
    /// [`PgAdvisoryLock`] guards and risking pool starvation under contention.
    pub(crate) async fn acquire_additional(&mut self, key: &str) -> anyhow::Result<()> {
        acquire_transaction_advisory_lock(self.transaction_mut(), key).await
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
    /// Serialize one synchronous admin bulk-build request per game across all
    /// replicas. The session lease spans slow Docker work without an open
    /// transaction; close-on-drop releases it if the HTTP request is cancelled.
    pub(crate) async fn acquire_build_batch(
        pool: &sqlx::PgPool,
        key: &str,
    ) -> anyhow::Result<Self> {
        static GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(1)));
        let permit = GATE.clone().acquire_owned().await?;
        Self::acquire_with_permit(pool, key, Some(permit)).await
    }

    /// Serialize a multi-stage account deletion with admin updates that could
    /// otherwise unban or rename the account between its durable fence and
    /// external roster teardown. Keep this gate independent from roster access:
    /// deletion legitimately holds this lease while acquiring roster leases.
    pub(crate) async fn acquire_account_lifecycle(
        pool: &sqlx::PgPool,
        key: &str,
    ) -> anyhow::Result<Self> {
        static GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
            std::sync::LazyLock::new(|| {
                std::sync::Arc::new(tokio::sync::Semaphore::new(ACCOUNT_LIFECYCLE_CONCURRENCY))
            });
        let permit = GATE.clone().acquire_owned().await?;
        Self::acquire_with_permit(pool, key, Some(permit)).await
    }

    /// Serialize roster deletion teardown across replicas without holding an
    /// open transaction. The shared roster admission gate bounds retained pool
    /// connections while the operation performs nested DB and network work.
    pub(crate) async fn acquire_roster(pool: &sqlx::PgPool, key: &str) -> anyhow::Result<Self> {
        let permit = roster_access_permit().await?;
        Self::acquire_with_permit(pool, key, Some(permit)).await
    }

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
        self.connection
            .as_mut()
            .expect("advisory lock session is live until release")
    }
}
