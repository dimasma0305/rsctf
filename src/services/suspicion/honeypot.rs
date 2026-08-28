//! Bounded aggregation for global honeypot telemetry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const QUEUE_CAPACITY: usize = 2_048;
const MAX_PENDING_BUCKETS: usize = 1_024;
const MAX_BATCH_BUCKETS: usize = 256;
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const ACQUIRE_TIMEOUT: Duration = Duration::from_millis(50);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const RETENTION_DAYS: i32 = 7;
const MAX_RETAINED_BUCKETS: i64 = 250_000;
const BUDGET_RECONCILE_MINUTES: i32 = 10;
static ADMISSION_DROPPED_TOTAL: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HoneypotTelemetryMetrics {
    pub queued: usize,
    pub queue_capacity: usize,
    pub queue_dropped: u64,
    pub admission_dropped: u64,
}

#[derive(Clone, Copy)]
pub enum HoneypotRouteClass {
    Http,
    Tcp,
}

#[derive(Debug, Clone)]
struct Observation {
    user_id: Option<Uuid>,
    bait: String,
    source_hash: String,
    user_agent: Option<String>,
    observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct BucketKey {
    bucket_start: chrono::DateTime<chrono::Utc>,
    bait: String,
    source_hash: String,
}

#[derive(Debug)]
struct Bucket {
    key: BucketKey,
    user_id: Option<Uuid>,
    user_agent: Option<String>,
    count: i64,
    last_hit: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Copy, Debug)]
struct FlushOutcome {
    succeeded: bool,
    capacity_dropped: u64,
}

pub(crate) struct HoneypotQueue {
    sender: mpsc::Sender<Observation>,
    receiver: Mutex<Option<mpsc::Receiver<Observation>>>,
    dropped_since_flush: Arc<AtomicU64>,
    dropped_total: Arc<AtomicU64>,
}

impl HoneypotQueue {
    pub(crate) fn new() -> Self {
        Self::with_capacity(QUEUE_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            dropped_since_flush: Arc::new(AtomicU64::new(0)),
            dropped_total: Arc::new(AtomicU64::new(0)),
        }
    }

    fn record_drop(&self) {
        self.dropped_since_flush.fetch_add(1, Ordering::Relaxed);
        self.dropped_total.fetch_add(1, Ordering::Relaxed);
    }

    fn enqueue(&self, observation: Observation) -> bool {
        if self.sender.try_send(observation).is_ok() {
            true
        } else {
            self.record_drop();
            false
        }
    }

    fn take_receiver(&self) -> Option<mpsc::Receiver<Observation>> {
        self.receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn metrics(&self) -> HoneypotTelemetryMetrics {
        HoneypotTelemetryMetrics {
            queued: self
                .sender
                .max_capacity()
                .saturating_sub(self.sender.capacity()),
            queue_capacity: self.sender.max_capacity(),
            queue_dropped: self.dropped_total.load(Ordering::Relaxed),
            admission_dropped: ADMISSION_DROPPED_TOTAL.load(Ordering::Relaxed),
        }
    }
}

/// Process-local bounded-queue counters for operational monitoring.
pub fn honeypot_telemetry_metrics(state: &SharedState) -> HoneypotTelemetryMetrics {
    state.honeypot_telemetry.metrics()
}

/// Cheap, silent admission used before authentication or database work. The
/// response stays an ordinary 404 whether an observation is sampled or kept.
/// Redis shares the source budget across replicas when configured; its bounded
/// local fallback preserves availability during a Redis outage.
pub async fn admit_honeypot_source(source: &str, route: HoneypotRouteClass) -> bool {
    let source = source.chars().take(64).collect::<String>();
    let admitted = crate::middlewares::rate_limiter::admit_honeypot_source(
        &source,
        matches!(route, HoneypotRouteClass::Tcp),
    )
    .await;
    if !admitted {
        ADMISSION_DROPPED_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
    admitted
}

pub fn enqueue_honeypot_hit(
    state: &SharedState,
    user_id: Option<Uuid>,
    bait: &str,
    remote_ip: Option<&str>,
    user_agent: Option<&str>,
) -> bool {
    let bait = bait.chars().take(128).collect::<String>();
    let source_hash = remote_ip
        .and_then(|ip| crate::services::anti_cheat::hash_ip_identity(state.config.as_ref(), ip))
        .map(|identity| hex::encode(identity.exact))
        .unwrap_or_default()
        .chars()
        .take(128)
        .collect::<String>();
    let user_agent = user_agent.map(|agent| agent.chars().take(256).collect::<String>());
    state.honeypot_telemetry.enqueue(Observation {
        user_id,
        bait,
        source_hash,
        user_agent,
        observed_at: chrono::Utc::now(),
    })
}

fn bucket_start(at: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    use chrono::Timelike;
    at.with_second(0)
        .and_then(|at| at.with_nanosecond(0))
        .unwrap_or(at)
}

fn merge(pending: &mut HashMap<BucketKey, Bucket>, observation: Observation) -> bool {
    let key = BucketKey {
        bucket_start: bucket_start(observation.observed_at),
        bait: observation.bait,
        source_hash: observation.source_hash,
    };
    if let Some(bucket) = pending.get_mut(&key) {
        bucket.count = bucket.count.saturating_add(1);
        bucket.last_hit = bucket.last_hit.max(observation.observed_at);
        bucket.user_id = bucket.user_id.or(observation.user_id);
        if observation.user_agent.is_some() {
            bucket.user_agent = observation.user_agent;
        }
        true
    } else if pending.len() < MAX_PENDING_BUCKETS {
        pending.insert(
            key.clone(),
            Bucket {
                key,
                user_id: observation.user_id,
                user_agent: observation.user_agent,
                count: 1,
                last_hit: observation.observed_at,
            },
        );
        true
    } else {
        false
    }
}

fn restore_failed_batch(pending: &mut HashMap<BucketKey, Bucket>, batch: Vec<Bucket>) {
    for bucket in batch {
        pending.insert(bucket.key.clone(), bucket);
    }
}

fn pending_observation_count(pending: &HashMap<BucketKey, Bucket>) -> u64 {
    pending.values().fold(0u64, |total, bucket| {
        total.saturating_add(u64::try_from(bucket.count).unwrap_or(u64::MAX))
    })
}

async fn flush_batch(pool: &sqlx::PgPool, batch: &[Bucket]) -> AppResult<u64> {
    let mut transaction = tokio::time::timeout(ACQUIRE_TIMEOUT, pool.begin())
        .await
        .map_err(|_| AppError::unavailable("honeypot telemetry pool admission timed out"))?
        .map_err(|error| AppError::internal(error.to_string()))?;
    let retained: i64 = sqlx::query_scalar(
        r#"SELECT row_count
             FROM "HoneypotBucketBudget"
            WHERE singleton = TRUE
              FOR UPDATE"#,
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::internal("honeypot bucket budget is not initialized"))?;

    let bucket_starts = batch
        .iter()
        .map(|row| row.key.bucket_start)
        .collect::<Vec<_>>();
    let baits = batch
        .iter()
        .map(|row| row.key.bait.clone())
        .collect::<Vec<_>>();
    let sources = batch
        .iter()
        .map(|row| row.key.source_hash.clone())
        .collect::<Vec<_>>();
    let existing = sqlx::query_as::<_, (chrono::DateTime<chrono::Utc>, String, String)>(
        r#"SELECT bucket.bucket_start_utc, bucket.bait, bucket.source_hash
             FROM "HoneypotHitBuckets" bucket
             JOIN UNNEST($1::timestamptz[], $2::text[], $3::text[])
                    AS input(bucket_start_utc, bait, source_hash)
               ON input.bucket_start_utc = bucket.bucket_start_utc
              AND input.bait = bucket.bait
              AND input.source_hash = bucket.source_hash"#,
    )
    .bind(&bucket_starts)
    .bind(&baits)
    .bind(&sources)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .into_iter()
    .map(|(bucket_start, bait, source_hash)| BucketKey {
        bucket_start,
        bait,
        source_hash,
    })
    .collect::<std::collections::HashSet<_>>();

    let mut remaining = MAX_RETAINED_BUCKETS.saturating_sub(retained).max(0) as usize;
    let mut accepted = Vec::with_capacity(batch.len());
    let mut capacity_dropped = 0u64;
    let mut inserted = 0i64;
    for bucket in batch {
        if existing.contains(&bucket.key) {
            accepted.push(bucket);
        } else if remaining > 0 {
            remaining -= 1;
            inserted += 1;
            accepted.push(bucket);
        } else {
            capacity_dropped =
                capacity_dropped.saturating_add(u64::try_from(bucket.count).unwrap_or(u64::MAX));
        }
    }
    if accepted.is_empty() {
        transaction
            .commit()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(capacity_dropped);
    }

    let bucket_starts = accepted
        .iter()
        .map(|row| row.key.bucket_start)
        .collect::<Vec<_>>();
    let baits = accepted
        .iter()
        .map(|row| row.key.bait.clone())
        .collect::<Vec<_>>();
    let sources = accepted
        .iter()
        .map(|row| row.key.source_hash.clone())
        .collect::<Vec<_>>();
    let users = accepted.iter().map(|row| row.user_id).collect::<Vec<_>>();
    let agents = accepted
        .iter()
        .map(|row| row.user_agent.clone())
        .collect::<Vec<_>>();
    let counts = accepted.iter().map(|row| row.count).collect::<Vec<_>>();
    let last_hits = accepted.iter().map(|row| row.last_hit).collect::<Vec<_>>();
    sqlx::query(
        r#"INSERT INTO "HoneypotHitBuckets" (
               bucket_start_utc, bait, source_hash, user_id, user_agent,
               hit_count, last_hit_at_utc
           ) SELECT * FROM UNNEST(
               $1::timestamptz[], $2::text[], $3::text[], $4::uuid[],
               $5::text[], $6::bigint[], $7::timestamptz[]
           ) ON CONFLICT (bucket_start_utc, bait, source_hash) DO UPDATE
               SET hit_count = "HoneypotHitBuckets".hit_count + EXCLUDED.hit_count,
                   last_hit_at_utc = GREATEST(
                       "HoneypotHitBuckets".last_hit_at_utc,
                       EXCLUDED.last_hit_at_utc
                   ),
                   user_id = COALESCE("HoneypotHitBuckets".user_id, EXCLUDED.user_id),
                   user_agent = COALESCE(EXCLUDED.user_agent, "HoneypotHitBuckets".user_agent)"#,
    )
    .bind(bucket_starts)
    .bind(baits)
    .bind(sources)
    .bind(users)
    .bind(agents)
    .bind(counts)
    .bind(last_hits)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if inserted > 0 {
        sqlx::query(
            r#"UPDATE "HoneypotBucketBudget"
                  SET row_count = row_count + $1
                WHERE singleton = TRUE"#,
        )
        .bind(inserted)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(capacity_dropped)
}

async fn flush(pool: &sqlx::PgPool, pending: &mut HashMap<BucketKey, Bucket>) -> FlushOutcome {
    if pending.is_empty() {
        return FlushOutcome {
            succeeded: true,
            capacity_dropped: 0,
        };
    }
    let keys = pending
        .keys()
        .take(MAX_BATCH_BUCKETS)
        .cloned()
        .collect::<Vec<_>>();
    let mut batch = keys
        .iter()
        .filter_map(|key| pending.remove(key))
        .collect::<Vec<_>>();
    batch.sort_unstable_by(|left, right| {
        (
            &left.key.bucket_start,
            &left.key.bait,
            &left.key.source_hash,
        )
            .cmp(&(
                &right.key.bucket_start,
                &right.key.bait,
                &right.key.source_hash,
            ))
    });
    let outcome = match tokio::time::timeout(WRITE_TIMEOUT, flush_batch(pool, &batch)).await {
        Ok(Ok(capacity_dropped)) => FlushOutcome {
            succeeded: true,
            capacity_dropped,
        },
        Ok(Err(error)) => {
            tracing::warn!(%error, "honeypot telemetry batch failed");
            FlushOutcome {
                succeeded: false,
                capacity_dropped: 0,
            }
        }
        Err(_) => {
            tracing::warn!("honeypot telemetry batch timed out");
            FlushOutcome {
                succeeded: false,
                capacity_dropped: 0,
            }
        }
    };
    if !outcome.succeeded {
        restore_failed_batch(pending, batch);
    }
    outcome
}

async fn run_writer(
    pool: sqlx::PgPool,
    mut receiver: mpsc::Receiver<Observation>,
    mut shutdown: watch::Receiver<bool>,
    dropped_since_flush: Arc<AtomicU64>,
    dropped_total: Arc<AtomicU64>,
) {
    let mut pending = HashMap::new();
    let mut last_admission_dropped = 0;
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            observation = receiver.recv() => match observation {
                Some(observation) => {
                    if !merge(&mut pending, observation) {
                        dropped_since_flush.fetch_add(1, Ordering::Relaxed);
                        dropped_total.fetch_add(1, Ordering::Relaxed);
                    }
                }
                None => break,
            },
            _ = ticker.tick() => {
                let outcome = flush(&pool, &mut pending).await;
                if outcome.capacity_dropped > 0 {
                    dropped_since_flush.fetch_add(outcome.capacity_dropped, Ordering::Relaxed);
                    dropped_total.fetch_add(outcome.capacity_dropped, Ordering::Relaxed);
                }
                let dropped_count = dropped_since_flush.swap(0, Ordering::Relaxed);
                if dropped_count > 0 {
                    tracing::warn!(
                        dropped = dropped_count,
                        dropped_total = dropped_total.load(Ordering::Relaxed),
                        "honeypot telemetry observations sampled at capacity"
                    );
                }
                let admission_dropped = ADMISSION_DROPPED_TOTAL.load(Ordering::Relaxed);
                if admission_dropped > last_admission_dropped {
                    tracing::warn!(
                        dropped = admission_dropped - last_admission_dropped,
                        dropped_total = admission_dropped,
                        "honeypot observations sampled by distributed source admission"
                    );
                    last_admission_dropped = admission_dropped;
                }
            },
        }
        if pending.len() >= MAX_PENDING_BUCKETS {
            let outcome = flush(&pool, &mut pending).await;
            if outcome.capacity_dropped > 0 {
                dropped_since_flush.fetch_add(outcome.capacity_dropped, Ordering::Relaxed);
                dropped_total.fetch_add(outcome.capacity_dropped, Ordering::Relaxed);
            }
        }
    }
    while let Ok(observation) = receiver.try_recv() {
        if !merge(&mut pending, observation) {
            dropped_since_flush.fetch_add(1, Ordering::Relaxed);
            dropped_total.fetch_add(1, Ordering::Relaxed);
        }
    }
    let outcome = flush(&pool, &mut pending).await;
    if outcome.capacity_dropped > 0 {
        dropped_total.fetch_add(outcome.capacity_dropped, Ordering::Relaxed);
    }
    let shutdown_dropped = pending_observation_count(&pending);
    if shutdown_dropped > 0 {
        dropped_total.fetch_add(shutdown_dropped, Ordering::Relaxed);
        tracing::warn!(
            dropped = shutdown_dropped,
            "honeypot telemetry shutdown discarded bounded pending work"
        );
    }
}

pub fn start_honeypot_writer(
    state: &SharedState,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let receiver = state.honeypot_telemetry.take_receiver();
    let pool = state.pg().clone();
    let dropped_since_flush = Arc::clone(&state.honeypot_telemetry.dropped_since_flush);
    let dropped_total = Arc::clone(&state.honeypot_telemetry.dropped_total);
    tokio::spawn(async move {
        let Some(receiver) = receiver else {
            tracing::warn!("honeypot telemetry writer started more than once");
            return;
        };
        run_writer(pool, receiver, shutdown, dropped_since_flush, dropped_total).await;
    })
}

pub async fn purge_honeypot_buckets(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    tokio::time::timeout(WRITE_TIMEOUT, purge_honeypot_buckets_inner(pool, limit))
        .await
        .map_err(|_| AppError::unavailable("honeypot retention sweep timed out"))?
}

async fn purge_honeypot_buckets_inner(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let (mut retained, reconciliation_due): (i64, bool) = sqlx::query_as(
        r#"SELECT row_count,
                  reconciled_at_utc < clock_timestamp() - make_interval(mins => $1)
             FROM "HoneypotBucketBudget"
            WHERE singleton = TRUE
              FOR UPDATE"#,
    )
    .bind(BUDGET_RECONCILE_MINUTES)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::internal("honeypot bucket budget is not initialized"))?;
    let reconciled = reconciliation_due || retained >= MAX_RETAINED_BUCKETS;
    if reconciled {
        retained = sqlx::query_scalar(r#"SELECT COUNT(*)::BIGINT FROM "HoneypotHitBuckets""#)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let result = sqlx::query(
        r#"DELETE FROM "HoneypotHitBuckets"
            WHERE ctid IN (
                SELECT ctid FROM "HoneypotHitBuckets"
                 WHERE last_hit_at_utc < clock_timestamp() - make_interval(days => $1)
                    OR $3
                 ORDER BY last_hit_at_utc
                 LIMIT $2
            )"#,
    )
    .bind(RETENTION_DAYS)
    .bind(limit.clamp(1, 10_000))
    .bind(retained >= MAX_RETAINED_BUCKETS)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let deleted = i64::try_from(result.rows_affected()).unwrap_or(i64::MAX);
    sqlx::query(
        r#"UPDATE "HoneypotBucketBudget"
              SET row_count = $1,
                  reconciled_at_utc = CASE WHEN $2 THEN clock_timestamp()
                                           ELSE reconciled_at_utc END
            WHERE singleton = TRUE"#,
    )
    .bind(retained.saturating_sub(deleted).max(0))
    .bind(reconciled)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(result.rows_affected())
}

pub async fn run_honeypot_chain_checks(_st: &SharedState, _game_id: i32) -> AppResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use std::str::FromStr;

    fn observation(bait: &str) -> Observation {
        Observation {
            user_id: None,
            bait: bait.to_string(),
            source_hash: "source".to_string(),
            user_agent: None,
            observed_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn saturated_handoff_is_nonblocking_and_bounded() {
        let queue = HoneypotQueue::with_capacity(1);
        assert!(queue.enqueue(observation("/.env")));
        assert!(!queue.enqueue(observation("/.git/config")));
        assert_eq!(queue.dropped_since_flush.load(Ordering::Relaxed), 1);
        assert_eq!(queue.dropped_total.load(Ordering::Relaxed), 1);
        let metrics = queue.metrics();
        assert_eq!(metrics.queued, 1);
        assert_eq!(metrics.queue_capacity, 1);
        assert!(metrics.queue_dropped >= 1);
    }

    #[test]
    fn repeated_source_and_bait_share_one_bucket() {
        let mut pending = HashMap::new();
        merge(&mut pending, observation("/.env"));
        merge(&mut pending, observation("/.env"));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.values().next().unwrap().count, 2);
        assert_eq!(pending_observation_count(&pending), 2);
    }

    #[tokio::test]
    async fn source_admission_is_fail_fast_and_bounded() {
        let source = format!("test-source-{}", Uuid::new_v4());
        for _ in 0..120 {
            assert!(admit_honeypot_source(&source, HoneypotRouteClass::Http).await);
        }
        assert!(!admit_honeypot_source(&source, HoneypotRouteClass::Http).await);
    }

    #[tokio::test]
    async fn database_outage_restores_only_the_bounded_pending_work() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(25))
            .connect_lazy_with(
                PgConnectOptions::new()
                    .host("127.0.0.1")
                    .port(1)
                    .username("rsctf")
                    .database("rsctf"),
            );
        let mut pending = HashMap::new();
        for index in 0..MAX_PENDING_BUCKETS {
            let mut item = observation("/.env");
            item.source_hash = format!("source-{index}");
            assert!(merge(&mut pending, item));
        }

        let started = std::time::Instant::now();
        let outcome = flush(&pool, &mut pending).await;
        assert!(!outcome.succeeded);
        assert_eq!(pending.len(), MAX_PENDING_BUCKETS);
        assert!(!merge(&mut pending, observation("/server-status")));
        assert!(started.elapsed() <= WRITE_TIMEOUT + Duration::from_secs(1));
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn replica_batches_upsert_one_bounded_forensic_bucket() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("honeypot_bucket_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create isolated schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse test database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated pool");
        sqlx::raw_sql(
            r#"CREATE TABLE "HoneypotHitBuckets" (
                 bucket_start_utc TIMESTAMPTZ NOT NULL,
                 bait TEXT NOT NULL,
                 source_hash TEXT NOT NULL,
                 user_id UUID,
                 user_agent TEXT,
                 hit_count BIGINT NOT NULL,
                 last_hit_at_utc TIMESTAMPTZ NOT NULL,
                 PRIMARY KEY (bucket_start_utc, bait, source_hash)
               );
               CREATE TABLE "HoneypotBucketBudget" (
                 singleton BOOLEAN PRIMARY KEY CHECK (singleton),
                 row_count BIGINT NOT NULL,
                 reconciled_at_utc TIMESTAMPTZ NOT NULL
               );
               INSERT INTO "HoneypotBucketBudget"
                    (singleton, row_count, reconciled_at_utc)
               VALUES (TRUE, 0, CURRENT_TIMESTAMP);"#,
        )
        .execute(&pool)
        .await
        .expect("create bucket fixture");

        let sample = observation("/.env");
        let mut first = HashMap::new();
        assert!(merge(&mut first, sample.clone()));
        assert!(merge(&mut first, sample.clone()));
        let mut second = HashMap::new();
        assert!(merge(&mut second, sample));
        let (first_outcome, second_outcome) =
            tokio::join!(flush(&pool, &mut first), flush(&pool, &mut second));
        assert!(first_outcome.succeeded);
        assert!(second_outcome.succeeded);
        let rows: Vec<(i64,)> = sqlx::query_as(r#"SELECT hit_count FROM "HoneypotHitBuckets""#)
            .fetch_all(&pool)
            .await
            .expect("load aggregate");
        assert_eq!(rows, vec![(3,)]);
        let retained: i64 = sqlx::query_scalar(r#"SELECT row_count FROM "HoneypotBucketBudget""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(retained, 1);

        sqlx::query(r#"UPDATE "HoneypotBucketBudget" SET row_count = $1"#)
            .bind(MAX_RETAINED_BUCKETS)
            .execute(&pool)
            .await
            .unwrap();
        let mut at_capacity = HashMap::new();
        assert!(merge(&mut at_capacity, observation("/.env")));
        assert!(merge(&mut at_capacity, observation("/.git/config")));
        let outcome = flush(&pool, &mut at_capacity).await;
        assert!(outcome.succeeded);
        assert_eq!(outcome.capacity_dropped, 1);
        let rows: Vec<(String, i64)> =
            sqlx::query_as(r#"SELECT bait, hit_count FROM "HoneypotHitBuckets" ORDER BY bait"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, vec![("/.env".into(), 4)]);

        sqlx::query(
            r#"UPDATE "HoneypotHitBuckets"
                  SET last_hit_at_utc = CURRENT_TIMESTAMP - interval '8 days'"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(purge_honeypot_buckets(&pool, 10).await.unwrap(), 1);
        let retained: i64 = sqlx::query_scalar(r#"SELECT row_count FROM "HoneypotBucketBudget""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(retained, 0);
        let mut reclaimed = HashMap::new();
        assert!(merge(&mut reclaimed, observation("/server-status")));
        let outcome = flush(&pool, &mut reclaimed).await;
        assert!(outcome.succeeded);
        assert_eq!(outcome.capacity_dropped, 0);
        let retained: i64 = sqlx::query_scalar(r#"SELECT row_count FROM "HoneypotBucketBudget""#)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(retained, 1);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
