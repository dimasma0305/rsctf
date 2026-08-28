//! Bounded aggregation for global honeypot telemetry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const QUEUE_CAPACITY: usize = 2_048;
const MAX_PENDING_BUCKETS: usize = 1_024;
const MAX_BATCH_BUCKETS: usize = 256;
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const SOURCE_WINDOW_SECONDS: u64 = 60;
const HTTP_SOURCE_LIMIT: u64 = 120;
const TCP_SOURCE_LIMIT: u64 = 30;
const MAX_SOURCE_WINDOWS: usize = 8_192;
const RETENTION_DAYS: i32 = 7;

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

pub(crate) struct HoneypotQueue {
    sender: mpsc::Sender<Observation>,
    receiver: Mutex<Option<mpsc::Receiver<Observation>>>,
    dropped: Arc<AtomicU64>,
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
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    fn enqueue(&self, observation: Observation) -> bool {
        if self.sender.try_send(observation).is_ok() {
            true
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    fn take_receiver(&self) -> Option<mpsc::Receiver<Observation>> {
        self.receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

#[derive(Default)]
struct SourceWindow {
    epoch: AtomicU64,
    count: AtomicU64,
}

static SOURCE_WINDOWS: std::sync::LazyLock<DashMap<String, Arc<SourceWindow>>> =
    std::sync::LazyLock::new(DashMap::new);

fn unix_second() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Cheap, silent admission used before authentication or database work. The
/// response stays an ordinary 404 whether an observation is sampled or kept.
pub fn admit_honeypot_source(source: &str, route: HoneypotRouteClass) -> bool {
    let source = source.chars().take(64).collect::<String>();
    let epoch = unix_second() / SOURCE_WINDOW_SECONDS;
    if !SOURCE_WINDOWS.contains_key(&source) && SOURCE_WINDOWS.len() >= MAX_SOURCE_WINDOWS {
        SOURCE_WINDOWS
            .retain(|_, window| window.epoch.load(Ordering::Acquire).saturating_add(1) >= epoch);
        // A spray of fresh source identities must not turn this best-effort
        // sensor into an unbounded allocation. Existing sources continue to
        // be metered while new identities are silently ignored at capacity.
        if SOURCE_WINDOWS.len() >= MAX_SOURCE_WINDOWS {
            return false;
        }
    }
    let window = SOURCE_WINDOWS
        .entry(source)
        .or_insert_with(|| Arc::new(SourceWindow::default()))
        .clone();
    let observed = window.epoch.load(Ordering::Acquire);
    if observed != epoch
        && window
            .epoch
            .compare_exchange(observed, epoch, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    {
        window.count.store(0, Ordering::Release);
    }
    let limit = match route {
        HoneypotRouteClass::Http => HTTP_SOURCE_LIMIT,
        HoneypotRouteClass::Tcp => TCP_SOURCE_LIMIT,
    };
    window
        .count
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < limit).then_some(count + 1)
        })
        .is_ok()
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

async fn flush(pool: &sqlx::PgPool, pending: &mut HashMap<BucketKey, Bucket>) -> bool {
    if pending.is_empty() {
        return true;
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
    let users = batch.iter().map(|row| row.user_id).collect::<Vec<_>>();
    let agents = batch
        .iter()
        .map(|row| row.user_agent.clone())
        .collect::<Vec<_>>();
    let counts = batch.iter().map(|row| row.count).collect::<Vec<_>>();
    let last_hits = batch.iter().map(|row| row.last_hit).collect::<Vec<_>>();
    let Some(mut connection) = pool.try_acquire() else {
        restore_failed_batch(pending, batch);
        return false;
    };
    let write = sqlx::query(
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
    .execute(&mut *connection);
    let succeeded = match tokio::time::timeout(WRITE_TIMEOUT, write).await {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => {
            tracing::warn!(%error, "honeypot telemetry batch failed");
            false
        }
        Err(_) => {
            tracing::warn!("honeypot telemetry batch timed out");
            false
        }
    };
    if !succeeded {
        restore_failed_batch(pending, batch);
    }
    succeeded
}

async fn run_writer(
    pool: sqlx::PgPool,
    mut receiver: mpsc::Receiver<Observation>,
    mut shutdown: watch::Receiver<bool>,
    dropped: Arc<AtomicU64>,
) {
    let mut pending = HashMap::new();
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
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                None => break,
            },
            _ = ticker.tick() => {
                let _ = flush(&pool, &mut pending).await;
                let dropped_count = dropped.swap(0, Ordering::Relaxed);
                if dropped_count > 0 {
                    tracing::warn!(dropped = dropped_count, "honeypot telemetry observations sampled at capacity");
                }
            },
        }
        if pending.len() >= MAX_PENDING_BUCKETS {
            let _ = flush(&pool, &mut pending).await;
        }
    }
    while let Ok(observation) = receiver.try_recv() {
        if !merge(&mut pending, observation) {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    let _ = flush(&pool, &mut pending).await;
}

pub fn start_honeypot_writer(
    state: &SharedState,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let receiver = state.honeypot_telemetry.take_receiver();
    let pool = state.pg().clone();
    let dropped = Arc::clone(&state.honeypot_telemetry.dropped);
    tokio::spawn(async move {
        let Some(receiver) = receiver else {
            tracing::warn!("honeypot telemetry writer started more than once");
            return;
        };
        run_writer(pool, receiver, shutdown, dropped).await;
    })
}

pub async fn purge_honeypot_buckets(pool: &sqlx::PgPool, limit: i64) -> AppResult<u64> {
    let result = sqlx::query(
        r#"DELETE FROM "HoneypotHitBuckets"
            WHERE ctid IN (
                SELECT ctid FROM "HoneypotHitBuckets"
                 WHERE last_hit_at_utc < clock_timestamp() - make_interval(days => $1)
                 ORDER BY last_hit_at_utc
                 LIMIT $2
            )"#,
    )
    .bind(RETENTION_DAYS)
    .bind(limit.clamp(1, 10_000))
    .execute(pool)
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
        assert_eq!(queue.dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn repeated_source_and_bait_share_one_bucket() {
        let mut pending = HashMap::new();
        merge(&mut pending, observation("/.env"));
        merge(&mut pending, observation("/.env"));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.values().next().unwrap().count, 2);
    }

    #[test]
    fn source_admission_is_fail_fast() {
        let source = format!("test-source-{}", Uuid::new_v4());
        for _ in 0..HTTP_SOURCE_LIMIT {
            assert!(admit_honeypot_source(&source, HoneypotRouteClass::Http));
        }
        assert!(!admit_honeypot_source(&source, HoneypotRouteClass::Http));
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
               );"#,
        )
        .execute(&pool)
        .await
        .expect("create bucket fixture");

        let sample = observation("/.env");
        let mut first = HashMap::new();
        assert!(merge(&mut first, sample.clone()));
        assert!(merge(&mut first, sample.clone()));
        assert!(flush(&pool, &mut first).await);
        let mut second = HashMap::new();
        assert!(merge(&mut second, sample));
        assert!(flush(&pool, &mut second).await);
        let rows: Vec<(i64,)> = sqlx::query_as(r#"SELECT hit_count FROM "HoneypotHitBuckets""#)
            .fetch_all(&pool)
            .await
            .expect("load aggregate");
        assert_eq!(rows, vec![(3,)]);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .expect("drop isolated schema");
        admin.close().await;
    }
}
