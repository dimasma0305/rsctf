//! Bounded, sampled handoff and aggregate writer for public honeypot telemetry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeZone as _, Utc};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::models::internal::configs::AppConfig;

mod bounds;
use bounds::*;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct AdmissionKey {
    source: [u8; 32],
    bait: u64,
}

#[derive(Clone, Copy, Debug)]
struct GateEntry {
    window: u64,
    seen: u32,
    last_seen: Instant,
}

#[derive(Debug)]
struct AdmissionShard(Mutex<HashMap<AdmissionKey, GateEntry>>);

#[derive(Clone, Debug)]
pub(crate) struct HoneypotAdmission {
    source_hash: Vec<u8>,
    estimated_hits: i64,
    sampled_hits: i64,
}

#[derive(Debug)]
struct HoneypotObservation {
    user_id: Option<Uuid>,
    bait: String,
    source_hash: Vec<u8>,
    user_agent: Option<String>,
    observed_at: DateTime<Utc>,
    estimated_hits: i64,
    sampled_hits: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HoneypotTelemetryCounters {
    pub accepted: u64,
    pub sampled: u64,
    pub rate_dropped: u64,
    pub queue_dropped: u64,
    pub persisted: u64,
    pub database_failures: u64,
    pub database_dropped_hits: u64,
}

#[derive(Debug, Default)]
struct CounterSet {
    accepted: AtomicU64,
    sampled: AtomicU64,
    rate_dropped: AtomicU64,
    queue_dropped: AtomicU64,
    persisted: AtomicU64,
    database_failures: AtomicU64,
    database_dropped_hits: AtomicU64,
}

impl CounterSet {
    fn snapshot(&self) -> HoneypotTelemetryCounters {
        HoneypotTelemetryCounters {
            accepted: self.accepted.load(Ordering::Relaxed),
            sampled: self.sampled.load(Ordering::Relaxed),
            rate_dropped: self.rate_dropped.load(Ordering::Relaxed),
            queue_dropped: self.queue_dropped.load(Ordering::Relaxed),
            persisted: self.persisted.load(Ordering::Relaxed),
            database_failures: self.database_failures.load(Ordering::Relaxed),
            database_dropped_hits: self.database_dropped_hits.load(Ordering::Relaxed),
        }
    }
}

/// State-owned request/connection admission and bounded writer handoff.
pub(crate) struct HoneypotTelemetry {
    sender: mpsc::Sender<HoneypotObservation>,
    receiver: Mutex<Option<mpsc::Receiver<HoneypotObservation>>>,
    admission: Box<[AdmissionShard]>,
    global_window: AtomicU64,
    counters: CounterSet,
}

impl HoneypotTelemetry {
    pub(crate) fn new() -> Self {
        Self::with_capacity(QUEUE_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            admission: (0..ADMISSION_SHARDS)
                .map(|_| AdmissionShard(Mutex::new(HashMap::new())))
                .collect(),
            global_window: AtomicU64::new(0),
            counters: CounterSet::default(),
        }
    }

    pub(crate) fn admit_source(
        &self,
        config: &AppConfig,
        raw_ip: &str,
        bait: &str,
    ) -> Option<HoneypotAdmission> {
        let source_hash = crate::services::anti_cheat::hash_ip_identity(config, raw_ip)?.exact;
        let epoch_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.admit_hashed_at(source_hash, bait, epoch_seconds, Instant::now())
    }

    fn admit_hashed_at(
        &self,
        source_hash: Vec<u8>,
        bait: &str,
        epoch_seconds: u64,
        now: Instant,
    ) -> Option<HoneypotAdmission> {
        let source: [u8; 32] = source_hash.as_slice().try_into().ok()?;
        let key = AdmissionKey {
            source,
            bait: stable_hash(bait),
        };
        let window = epoch_seconds / SOURCE_WINDOW_SECONDS;
        let mut entries = self.shard(&key);
        if entries.len() >= MAX_KEYS_PER_SHARD && !entries.contains_key(&key) {
            entries
                .retain(|_, entry| now.saturating_duration_since(entry.last_seen) < GATE_IDLE_TTL);
            if entries.len() >= MAX_KEYS_PER_SHARD {
                if let Some(oldest) = entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_seen)
                    .map(|(key, _)| *key)
                {
                    entries.remove(&oldest);
                }
            }
        }
        let entry = entries.entry(key).or_insert(GateEntry {
            window,
            seen: 0,
            last_seen: now,
        });
        if entry.window != window {
            entry.window = window;
            entry.seen = 0;
        }
        entry.seen = entry.seen.saturating_add(1);
        entry.last_seen = now;
        let (estimated_hits, sampled_hits) = if entry.seen <= SOURCE_BURST {
            (1, 0)
        } else if (entry.seen - SOURCE_BURST).is_multiple_of(SAMPLE_EVERY) {
            (i64::from(SAMPLE_EVERY), i64::from(SAMPLE_EVERY))
        } else {
            drop(entries);
            self.counters.rate_dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        drop(entries);
        // Only observations selected by the per-source sampler consume the
        // process-wide budget. A single hot source therefore cannot exhaust
        // global admission with requests that would be discarded anyway.
        if !self.admit_global(epoch_seconds) {
            self.counters.rate_dropped.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.counters.accepted.fetch_add(1, Ordering::Relaxed);
        if sampled_hits > 0 {
            self.counters
                .sampled
                .fetch_add(sampled_hits as u64, Ordering::Relaxed);
        }
        Some(HoneypotAdmission {
            source_hash,
            estimated_hits,
            sampled_hits,
        })
    }

    fn admit_global(&self, epoch_seconds: u64) -> bool {
        loop {
            let current = self.global_window.load(Ordering::Relaxed);
            let current_window = current >> 32;
            let current_count = current as u32;
            let next = if current_window == epoch_seconds {
                if current_count >= GLOBAL_EVENTS_PER_SECOND {
                    return false;
                }
                (epoch_seconds << 32) | u64::from(current_count + 1)
            } else {
                (epoch_seconds << 32) | 1
            };
            if self
                .global_window
                .compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn shard(&self, key: &AdmissionKey) -> MutexGuard<'_, HashMap<AdmissionKey, GateEntry>> {
        let index = (stable_hash(key) as usize) % self.admission.len();
        self.admission[index]
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn enqueue_http(
        &self,
        user_id: Option<Uuid>,
        bait: &str,
        user_agent: Option<&str>,
        admission: HoneypotAdmission,
    ) -> bool {
        self.enqueue(HoneypotObservation {
            user_id,
            bait: cap_text(bait, MAX_BAIT_BYTES),
            source_hash: admission.source_hash,
            user_agent: user_agent.map(|value| cap_text(value, MAX_USER_AGENT_BYTES)),
            observed_at: Utc::now(),
            estimated_hits: admission.estimated_hits,
            sampled_hits: admission.sampled_hits,
        })
    }

    pub(crate) fn enqueue_tcp(&self, bait: &str, admission: HoneypotAdmission) -> bool {
        self.enqueue(HoneypotObservation {
            user_id: None,
            bait: cap_text(bait, MAX_BAIT_BYTES),
            source_hash: admission.source_hash,
            user_agent: None,
            observed_at: Utc::now(),
            estimated_hits: admission.estimated_hits,
            sampled_hits: admission.sampled_hits,
        })
    }

    fn enqueue(&self, observation: HoneypotObservation) -> bool {
        if self.sender.try_send(observation).is_ok() {
            true
        } else {
            self.counters.queue_dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    fn take_receiver(&self) -> Option<mpsc::Receiver<HoneypotObservation>> {
        self.receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    #[cfg(test)]
    pub(crate) fn counters(&self) -> HoneypotTelemetryCounters {
        self.counters.snapshot()
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct AggregateKey {
    bucket_millis: i64,
    bait: String,
    source_hash: Vec<u8>,
}

#[derive(Debug)]
struct Aggregate {
    key: AggregateKey,
    user_id: Option<Uuid>,
    user_agent: Option<String>,
    first_seen: DateTime<Utc>,
    last_seen: DateTime<Utc>,
    hit_count: i64,
    sampled_count: i64,
}

fn merge_observation(
    pending: &mut HashMap<AggregateKey, Aggregate>,
    observation: HoneypotObservation,
) {
    let bucket_millis = observation
        .observed_at
        .timestamp_millis()
        .div_euclid(BUCKET_MILLIS)
        * BUCKET_MILLIS;
    let key = AggregateKey {
        bucket_millis,
        bait: observation.bait,
        source_hash: observation.source_hash,
    };
    match pending.get_mut(&key) {
        Some(aggregate) => {
            aggregate.first_seen = aggregate.first_seen.min(observation.observed_at);
            if observation.observed_at >= aggregate.last_seen {
                aggregate.last_seen = observation.observed_at;
                if observation.user_agent.is_some() {
                    aggregate.user_agent = observation.user_agent;
                }
            }
            if aggregate.user_id != observation.user_id {
                // A shared source may represent multiple people. Once mixed,
                // keep the global aggregate anonymous rather than attributing
                // its combined count to whichever account happened to be last.
                aggregate.user_id = None;
            }
            aggregate.hit_count = aggregate
                .hit_count
                .saturating_add(observation.estimated_hits);
            aggregate.sampled_count = aggregate
                .sampled_count
                .saturating_add(observation.sampled_hits);
        }
        None => {
            pending.insert(
                key.clone(),
                Aggregate {
                    key: AggregateKey {
                        bucket_millis,
                        bait: key.bait.clone(),
                        source_hash: key.source_hash.clone(),
                    },
                    user_id: observation.user_id,
                    user_agent: observation.user_agent,
                    first_seen: observation.observed_at,
                    last_seen: observation.observed_at,
                    hit_count: observation.estimated_hits,
                    sampled_count: observation.sampled_hits,
                },
            );
        }
    }
}

async fn persist_batch_with_budget(
    pool: &sqlx::PgPool,
    batch: Vec<Aggregate>,
    row_budget: i64,
    delete_batch: i64,
) -> Result<(u64, u64), sqlx::Error> {
    if batch.is_empty() {
        return Ok((0, 0));
    }
    let mut bucket_starts = Vec::with_capacity(batch.len());
    let mut baits = Vec::with_capacity(batch.len());
    let mut source_hashes = Vec::with_capacity(batch.len());
    let mut user_ids = Vec::with_capacity(batch.len());
    let mut user_agents = Vec::with_capacity(batch.len());
    let mut first_seen = Vec::with_capacity(batch.len());
    let mut last_seen = Vec::with_capacity(batch.len());
    let mut hit_counts = Vec::with_capacity(batch.len());
    let mut sampled_counts = Vec::with_capacity(batch.len());
    for aggregate in batch {
        bucket_starts.push(
            Utc.timestamp_millis_opt(aggregate.key.bucket_millis)
                .single()
                .expect("minute bucket is a valid timestamp"),
        );
        baits.push(aggregate.key.bait);
        source_hashes.push(aggregate.key.source_hash);
        user_ids.push(aggregate.user_id);
        user_agents.push(aggregate.user_agent);
        first_seen.push(aggregate.first_seen);
        last_seen.push(aggregate.last_seen);
        hit_counts.push(aggregate.hit_count);
        sampled_counts.push(aggregate.sampled_count);
    }
    let mut transaction = pool.begin().await?;
    // Serialize the aggregate insert and trim across every process. Without
    // this lock, each replica can independently admit a full in-memory rate
    // between periodic sweeps and multiply the nominal global row budget.
    sqlx::query("SELECT pg_advisory_xact_lock(285, 3)")
        .execute(&mut *transaction)
        .await?;
    let result = sqlx::query(
        r#"WITH input AS (
               SELECT * FROM UNNEST(
                   $1::TIMESTAMPTZ[], $2::TEXT[], $3::BYTEA[], $4::UUID[],
                   $5::TEXT[], $6::TIMESTAMPTZ[], $7::TIMESTAMPTZ[],
                   $8::BIGINT[], $9::BIGINT[]
               ) AS value(
                   bucket_start_utc, bait, source_hash, user_id,
                   user_agent, first_seen, last_seen, hit_count, sampled_count
               )
           )
           INSERT INTO "HoneypotHits" (
               game_id, participation_id, user_id, bait, remote_ip, user_agent,
               hit_at_utc, bucket_start_utc, source_hash, hit_count,
               sampled_count, last_hit_at_utc
           )
           SELECT NULL, NULL, user_id, bait, '', user_agent, first_seen,
                  bucket_start_utc, source_hash, hit_count,
                  sampled_count, last_seen
             FROM input
           ON CONFLICT (bucket_start_utc, bait, source_hash)
               WHERE bucket_start_utc IS NOT NULL AND source_hash IS NOT NULL
           DO UPDATE SET
               hit_count = "HoneypotHits".hit_count + EXCLUDED.hit_count,
               sampled_count = "HoneypotHits".sampled_count + EXCLUDED.sampled_count,
               hit_at_utc = LEAST("HoneypotHits".hit_at_utc, EXCLUDED.hit_at_utc),
               last_hit_at_utc = GREATEST(
                   COALESCE("HoneypotHits".last_hit_at_utc, "HoneypotHits".hit_at_utc),
                   EXCLUDED.last_hit_at_utc
               ),
               user_agent = COALESCE(EXCLUDED.user_agent, "HoneypotHits".user_agent),
               user_id = CASE
                   WHEN "HoneypotHits".user_id IS NOT DISTINCT FROM EXCLUDED.user_id
                   THEN "HoneypotHits".user_id
                   ELSE NULL
               END"#,
    )
    .bind(bucket_starts)
    .bind(baits)
    .bind(source_hashes)
    .bind(user_ids)
    .bind(user_agents)
    .bind(first_seen)
    .bind(last_seen)
    .bind(hit_counts)
    .bind(sampled_counts)
    .execute(&mut *transaction)
    .await?;
    let deleted = sqlx::query(
        r#"WITH boundary AS (
               SELECT COALESCE(hit.bucket_start_utc, hit.hit_at_utc) AS observed_at,
                      hit.id
                 FROM "HoneypotHits" hit
                WHERE hit.game_id IS NULL AND hit.participation_id IS NULL
                ORDER BY observed_at DESC, hit.id DESC
                OFFSET $1 LIMIT 1
           ), doomed AS (
               SELECT hit.id FROM "HoneypotHits" hit CROSS JOIN boundary
                WHERE hit.game_id IS NULL AND hit.participation_id IS NULL
                  AND (COALESCE(hit.bucket_start_utc, hit.hit_at_utc), hit.id)
                      <= (boundary.observed_at, boundary.id)
                ORDER BY COALESCE(hit.bucket_start_utc, hit.hit_at_utc), hit.id
                LIMIT $2
           )
           DELETE FROM "HoneypotHits" hit USING doomed
            WHERE hit.id = doomed.id"#,
    )
    .bind(row_budget)
    .bind(delete_batch)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;
    Ok((result.rows_affected(), deleted))
}

async fn persist_batch(pool: &sqlx::PgPool, batch: Vec<Aggregate>) -> Result<u64, sqlx::Error> {
    let (written, _) =
        persist_batch_with_budget(pool, batch, RETENTION_ROW_BUDGET, RETENTION_DELETE_BATCH)
            .await?;
    Ok(written)
}

async fn enforce_age_retention(pool: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        r#"WITH lease AS MATERIALIZED (
               SELECT pg_try_advisory_xact_lock(285, 1) AS acquired
           ), doomed AS (
               SELECT hit.id FROM "HoneypotHits" hit CROSS JOIN lease
                WHERE lease.acquired
                  AND hit.game_id IS NULL AND hit.participation_id IS NULL
                  AND COALESCE(hit.bucket_start_utc, hit.hit_at_utc)
                      < clock_timestamp() - ($1 * INTERVAL '1 day')
                ORDER BY COALESCE(hit.bucket_start_utc, hit.hit_at_utc), hit.id
                LIMIT $2
           )
           DELETE FROM "HoneypotHits" hit USING doomed
            WHERE hit.id = doomed.id"#,
    )
    .bind(RETENTION_AGE_DAYS)
    .bind(RETENTION_DELETE_BATCH)
    .execute(pool)
    .await?
    .rows_affected())
}

async fn enforce_row_budget_with(
    pool: &sqlx::PgPool,
    row_budget: i64,
    delete_batch: i64,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        r#"WITH lease AS MATERIALIZED (
               SELECT pg_try_advisory_xact_lock(285, 2) AS acquired
           ), boundary AS (
               SELECT COALESCE(hit.bucket_start_utc, hit.hit_at_utc) AS observed_at,
                      hit.id
                 FROM "HoneypotHits" hit CROSS JOIN lease
                WHERE lease.acquired
                  AND hit.game_id IS NULL AND hit.participation_id IS NULL
                ORDER BY observed_at DESC, hit.id DESC
                OFFSET $1 LIMIT 1
           ), doomed AS (
               SELECT hit.id FROM "HoneypotHits" hit CROSS JOIN boundary
                WHERE hit.game_id IS NULL AND hit.participation_id IS NULL
                  AND (COALESCE(hit.bucket_start_utc, hit.hit_at_utc), hit.id)
                      <= (boundary.observed_at, boundary.id)
                ORDER BY COALESCE(hit.bucket_start_utc, hit.hit_at_utc), hit.id
                LIMIT $2
           )
           DELETE FROM "HoneypotHits" hit USING doomed
            WHERE hit.id = doomed.id"#,
    )
    .bind(row_budget)
    .bind(delete_batch)
    .execute(pool)
    .await?
    .rows_affected())
}

async fn enforce_row_budget(pool: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    enforce_row_budget_with(pool, RETENTION_ROW_BUDGET, RETENTION_DELETE_BATCH).await
}

async fn flush_pending(
    pool: &sqlx::PgPool,
    pending: &mut HashMap<AggregateKey, Aggregate>,
    counters: &CounterSet,
) {
    if pending.is_empty() {
        return;
    }
    let batch: Vec<_> = pending.drain().map(|(_, aggregate)| aggregate).collect();
    let dropped_hits = batch
        .iter()
        .map(|aggregate| u64::try_from(aggregate.hit_count).unwrap_or_default())
        .fold(0_u64, u64::saturating_add);
    match tokio::time::timeout(WRITE_TIMEOUT, persist_batch(pool, batch)).await {
        Ok(Ok(rows)) => {
            counters.persisted.fetch_add(rows, Ordering::Relaxed);
        }
        Ok(Err(error)) => {
            counters.database_failures.fetch_add(1, Ordering::Relaxed);
            counters
                .database_dropped_hits
                .fetch_add(dropped_hits, Ordering::Relaxed);
            tracing::warn!(%error, "honeypot aggregate batch write failed");
        }
        Err(_) => {
            counters.database_failures.fetch_add(1, Ordering::Relaxed);
            counters
                .database_dropped_hits
                .fetch_add(dropped_hits, Ordering::Relaxed);
            tracing::warn!("honeypot aggregate batch write exceeded its deadline");
        }
    }
}

async fn run_writer(
    pool: sqlx::PgPool,
    mut receiver: mpsc::Receiver<HoneypotObservation>,
    counters: &CounterSet,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut pending = HashMap::new();
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    flush.tick().await;
    let mut retention = tokio::time::interval(RETENTION_INTERVAL);
    retention.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    retention.tick().await;
    let mut row_budget = tokio::time::interval(ROW_BUDGET_SWEEP_INTERVAL);
    row_budget.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    row_budget.tick().await;
    // Reconcile a backlog left by a preceding replica/process before waiting
    // for this process to accept its first observation.
    let mut row_budget_dirty = true;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            observation = receiver.recv() => {
                let Some(observation) = observation else { break; };
                merge_observation(&mut pending, observation);
                row_budget_dirty = true;
                if pending.len() >= MAX_AGGREGATES_PER_BATCH {
                    flush_pending(&pool, &mut pending, counters).await;
                }
            }
            _ = flush.tick() => {
                flush_pending(&pool, &mut pending, counters).await;
            }
            _ = retention.tick() => {
                match tokio::time::timeout(WRITE_TIMEOUT, enforce_age_retention(&pool)).await {
                    Ok(Ok(deleted)) if deleted > 0 => {
                        tracing::info!(deleted, "honeypot aggregate age retention removed bounded backlog");
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        counters.database_failures.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(%error, "honeypot aggregate retention failed");
                    }
                    Err(_) => {
                        counters.database_failures.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!("honeypot aggregate retention exceeded its deadline");
                    }
                }
                let snapshot = counters.snapshot();
                tracing::info!(
                    accepted = snapshot.accepted,
                    sampled = snapshot.sampled,
                    rate_dropped = snapshot.rate_dropped,
                    queue_dropped = snapshot.queue_dropped,
                    persisted = snapshot.persisted,
                    database_failures = snapshot.database_failures,
                    database_dropped_hits = snapshot.database_dropped_hits,
                    "honeypot telemetry counters"
                );
            }
            _ = row_budget.tick(), if row_budget_dirty => {
                flush_pending(&pool, &mut pending, counters).await;
                match tokio::time::timeout(WRITE_TIMEOUT, enforce_row_budget(&pool)).await {
                    Ok(Ok(deleted)) => {
                        // A full trim batch means older backlog may remain;
                        // keep scheduling bounded sweeps until it converges.
                        row_budget_dirty = deleted >= RETENTION_DELETE_BATCH as u64;
                        if deleted > 0 {
                            tracing::info!(deleted, "honeypot aggregate row budget removed bounded backlog");
                        }
                    }
                    Ok(Err(error)) => {
                        counters.database_failures.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(%error, "honeypot aggregate row-budget enforcement failed");
                    }
                    Err(_) => {
                        counters.database_failures.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!("honeypot aggregate row-budget enforcement exceeded its deadline");
                    }
                }
            }
        }
    }
    flush_pending(&pool, &mut pending, counters).await;
}

/// Start the single optional aggregate writer owned by an API/network process.
pub fn start_writer(
    state: &SharedState,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let receiver = state.honeypot_telemetry.take_receiver();
    let pool = state.pg().clone();
    let state = state.clone();
    tokio::spawn(async move {
        let Some(receiver) = receiver else {
            tracing::warn!("honeypot telemetry writer was started more than once");
            return;
        };
        run_writer(pool, receiver, &state.honeypot_telemetry.counters, shutdown).await;
    })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    fn observation(at: DateTime<Utc>, hits: i64) -> HoneypotObservation {
        HoneypotObservation {
            user_id: None,
            bait: "/.env".to_string(),
            source_hash: vec![7; 32],
            user_agent: Some("agent".to_string()),
            observed_at: at,
            estimated_hits: hits,
            sampled_hits: hits.saturating_sub(1),
        }
    }

    #[test]
    fn admission_samples_excess_and_keeps_the_queue_bounded() {
        let telemetry = HoneypotTelemetry::with_capacity(1);
        let now = Instant::now();
        let source = vec![3; 32];
        let mut admitted = Vec::new();
        for _ in 0..(SOURCE_BURST + SAMPLE_EVERY) {
            if let Some(token) = telemetry.admit_hashed_at(source.clone(), "/.env", 100, now) {
                admitted.push(token);
            }
        }
        assert_eq!(admitted.len(), SOURCE_BURST as usize + 1);
        assert_eq!(
            admitted.last().unwrap().estimated_hits,
            i64::from(SAMPLE_EVERY)
        );
        assert!(telemetry.enqueue_tcp("ssh:22", admitted.remove(0)));
        assert!(!telemetry.enqueue_tcp("ssh:22", admitted.remove(0)));
        let counters = telemetry.counters();
        assert_eq!(counters.accepted, u64::from(SOURCE_BURST + 1));
        assert_eq!(counters.sampled, u64::from(SAMPLE_EVERY));
        assert_eq!(counters.queue_dropped, 1);
        assert_eq!(telemetry.sender.max_capacity(), 1);
    }

    #[test]
    fn discarded_hot_source_requests_do_not_spend_global_admission() {
        let telemetry = HoneypotTelemetry::with_capacity(1);
        let now = Instant::now();
        for _ in 0..1_000 {
            let _ = telemetry.admit_hashed_at(vec![1; 32], "/.env", 100, now);
        }
        assert!(telemetry
            .admit_hashed_at(vec![2; 32], "/.env", 100, now)
            .is_some());
    }

    #[test]
    fn aggregation_coalesces_one_source_bait_and_minute() {
        let mut pending = HashMap::new();
        let first = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 1).unwrap();
        merge_observation(&mut pending, observation(first, 1));
        merge_observation(
            &mut pending,
            observation(first + chrono::Duration::seconds(30), 32),
        );
        assert_eq!(pending.len(), 1);
        let aggregate = pending.values().next().unwrap();
        assert_eq!(aggregate.hit_count, 33);
        assert_eq!(aggregate.sampled_count, 31);
        assert_eq!(aggregate.first_seen, first);
        assert_eq!(aggregate.last_seen, first + chrono::Duration::seconds(30));
    }

    #[test]
    fn aggregation_key_is_source_bait_bucket_not_actor() {
        let mut pending = HashMap::new();
        let first = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 1).unwrap();
        merge_observation(&mut pending, observation(first, 1));
        let mut authenticated = observation(first + chrono::Duration::seconds(1), 1);
        authenticated.user_id = Some(Uuid::new_v4());
        merge_observation(&mut pending, authenticated);

        assert_eq!(pending.len(), 1);
        let aggregate = pending.values().next().unwrap();
        assert_eq!(aggregate.hit_count, 2);
        assert_eq!(aggregate.user_id, None, "mixed sources stay anonymous");
    }

    #[test]
    fn stored_fields_are_utf8_safe_and_strictly_capped() {
        let value = "é".repeat(200);
        let capped = cap_text(&value, 255);
        assert!(capped.len() <= 255);
        assert!(capped.is_char_boundary(capped.len()));
        assert_eq!(cap_text("short", 10), "short");
    }

    #[test]
    fn row_budget_trim_covers_a_maximum_aggregate_batch() {
        assert!(
            RETENTION_DELETE_BATCH
                >= i64::try_from(MAX_AGGREGATES_PER_BATCH).expect("batch size fits i64")
        );
    }

    #[tokio::test]
    async fn failed_database_batch_is_dropped_with_an_explicit_hit_count() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgresql://unused:unused@127.0.0.1:9/unused")
            .unwrap();
        pool.close().await;
        let mut pending = HashMap::new();
        merge_observation(&mut pending, observation(Utc::now(), 32));
        let counters = CounterSet::default();
        flush_pending(&pool, &mut pending, &counters).await;
        let counters = counters.snapshot();
        assert_eq!(counters.database_failures, 1);
        assert_eq!(counters.database_dropped_hits, 32);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_batches_upsert_counts_and_expire_only_global_aggregates() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("connect test database");
        let schema = format!("honeypot_aggregate_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .expect("create test schema");
        let options = PgConnectOptions::from_str(&database_url)
            .expect("parse database URL")
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("connect isolated pool");
        sqlx::raw_sql(
            r#"
            CREATE TABLE "HoneypotHits" (
                id INTEGER GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
                game_id INTEGER NULL,
                participation_id INTEGER NULL,
                user_id UUID NULL,
                bait TEXT NOT NULL,
                remote_ip TEXT NOT NULL DEFAULT '',
                user_agent TEXT NULL,
                hit_at_utc TIMESTAMPTZ NOT NULL,
                bucket_start_utc TIMESTAMPTZ NULL,
                source_hash BYTEA NULL,
                hit_count BIGINT NOT NULL DEFAULT 1,
                sampled_count BIGINT NOT NULL DEFAULT 0,
                last_hit_at_utc TIMESTAMPTZ NULL
            );
            CREATE UNIQUE INDEX ux_honeypot_hits_aggregate_bucket
                ON "HoneypotHits" (bucket_start_utc, bait, source_hash)
                WHERE bucket_start_utc IS NOT NULL AND source_hash IS NOT NULL;
            CREATE INDEX ix_honeypot_hits_global_retention
                ON "HoneypotHits" ((COALESCE(bucket_start_utc, hit_at_utc)), id)
                WHERE game_id IS NULL AND participation_id IS NULL;
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let at = Utc::now() - chrono::Duration::days(RETENTION_AGE_DAYS + 1);
        let mut pending = HashMap::new();
        merge_observation(&mut pending, observation(at, 1));
        merge_observation(
            &mut pending,
            observation(at + chrono::Duration::seconds(1), 32),
        );
        persist_batch(&pool, pending.into_values().collect())
            .await
            .unwrap();
        let mut second = HashMap::new();
        merge_observation(
            &mut second,
            observation(at + chrono::Duration::seconds(2), 1),
        );
        persist_batch(&pool, second.into_values().collect())
            .await
            .unwrap();
        let mut replicas = Vec::new();
        for offset in 0..8 {
            let pool = pool.clone();
            replicas.push(tokio::spawn(async move {
                let mut batch = HashMap::new();
                merge_observation(
                    &mut batch,
                    observation(at + chrono::Duration::seconds(3 + offset), 1),
                );
                persist_batch(&pool, batch.into_values().collect())
                    .await
                    .unwrap();
            }));
        }
        for replica in replicas {
            replica.await.unwrap();
        }
        let row: (i64, i64, i64) = sqlx::query_as(
            r#"SELECT COUNT(*)::BIGINT, MAX(hit_count), MAX(sampled_count)
                 FROM "HoneypotHits""#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, (1, 42, 31));

        sqlx::query(
            r#"INSERT INTO "HoneypotHits"
                   (game_id, participation_id, bait, remote_ip, hit_at_utc)
               VALUES (NULL, NULL, 'legacy-global', '', $1)"#,
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "HoneypotHits"
                   (game_id, participation_id, bait, remote_ip, hit_at_utc)
               VALUES (1, 2, 'retained-event-evidence', '', $1)"#,
        )
        .bind(at)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(enforce_age_retention(&pool).await.unwrap(), 2);
        let remaining: Vec<(Option<i32>, String)> =
            sqlx::query_as(r#"SELECT game_id, bait FROM "HoneypotHits" ORDER BY id"#)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            remaining,
            vec![(Some(1), "retained-event-evidence".to_string())]
        );

        sqlx::query(
            r#"INSERT INTO "HoneypotHits"
                   (game_id, participation_id, bait, remote_ip, hit_at_utc)
               SELECT NULL, NULL, 'budget-' || value, '', clock_timestamp()
                 FROM generate_series(1, 9) AS value"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let (first_replica, second_replica) = tokio::join!(
            enforce_row_budget_with(&pool, 5, 3),
            enforce_row_budget_with(&pool, 5, 3),
        );
        let concurrently_deleted = first_replica.unwrap() + second_replica.unwrap();
        assert!((3..=4).contains(&concurrently_deleted));
        let _ = enforce_row_budget_with(&pool, 5, 3).await.unwrap();
        let global_rows: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::BIGINT FROM "HoneypotHits"
                WHERE game_id IS NULL AND participation_id IS NULL"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(global_rows, 5);

        // Independent replica writers cannot each grow the table between
        // periodic sweeps: insert and trim share one transaction-level lock.
        let mut replica_writes = Vec::new();
        for offset in 0..8_u8 {
            let pool = pool.clone();
            replica_writes.push(tokio::spawn(async move {
                let mut item = observation(Utc::now(), 1);
                item.bait = format!("replica-{offset}");
                item.source_hash = vec![offset.saturating_add(20); 32];
                let mut batch = HashMap::new();
                merge_observation(&mut batch, item);
                persist_batch_with_budget(
                    &pool,
                    batch.into_values().collect(),
                    5,
                    MAX_AGGREGATES_PER_BATCH as i64,
                )
                .await
                .unwrap();
            }));
        }
        for write in replica_writes {
            write.await.unwrap();
        }
        let rows_after_replicas: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*)::BIGINT FROM "HoneypotHits"
                WHERE game_id IS NULL AND participation_id IS NULL"#,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows_after_replicas, 5);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
