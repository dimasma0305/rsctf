//! Bounded, supervised flag-egress observation writer.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::app_state::SharedState;

const QUEUE_CAPACITY: usize = 2_048;
const MAX_AGGREGATES: usize = 4_096;
const MAX_FLUSH: usize = 256;
static QUEUE_DROPS: AtomicU64 = AtomicU64::new(0);
static AGGREGATE_DROPS: AtomicU64 = AtomicU64::new(0);

fn record_overflow(counter: &AtomicU64, boundary: &'static str) {
    record_overflow_count(counter, boundary, 1);
}

fn record_overflow_count(counter: &AtomicU64, boundary: &'static str, count: u64) {
    let dropped = counter
        .fetch_add(count, Ordering::Relaxed)
        .saturating_add(count);
    if dropped.is_power_of_two() {
        tracing::warn!(
            dropped,
            boundary,
            "flag-egress telemetry was shed at a bounded boundary"
        );
    }
}

#[derive(Debug, thiserror::Error)]
enum FlushError {
    #[error("participation evidence is sealed")]
    Sealed,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

pub(crate) fn record_queue_drop() {
    record_overflow(&QUEUE_DROPS, "queue");
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ObservationKey {
    pub game_id: i32,
    pub participation_id: i32,
    pub challenge_id: i32,
    pub container_id: Uuid,
    pub remote_ip: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Observation {
    pub key: ObservationKey,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone)]
pub(crate) struct Queue {
    inner: Arc<QueueInner>,
}

struct QueueInner {
    sender: mpsc::Sender<Observation>,
    receiver: Mutex<Option<mpsc::Receiver<Observation>>>,
}

impl Queue {
    pub(crate) fn new() -> Self {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        Self {
            inner: Arc::new(QueueInner {
                sender,
                receiver: Mutex::new(Some(receiver)),
            }),
        }
    }

    pub(crate) fn enqueue(&self, observation: Observation) -> bool {
        self.inner.sender.try_send(observation).is_ok()
    }

    fn take_receiver(&self) -> Option<mpsc::Receiver<Observation>> {
        self.inner
            .receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
struct Aggregate {
    key: ObservationKey,
    count: i64,
    first_seen: chrono::DateTime<chrono::Utc>,
    last_seen: chrono::DateTime<chrono::Utc>,
}

fn aggregate(map: &mut HashMap<ObservationKey, Aggregate>, observation: Observation) {
    if let Some(current) = map.get_mut(&observation.key) {
        current.count = current.count.saturating_add(1);
        current.first_seen = current.first_seen.min(observation.observed_at);
        current.last_seen = current.last_seen.max(observation.observed_at);
    } else if map.len() < MAX_AGGREGATES {
        map.insert(
            observation.key.clone(),
            Aggregate {
                key: observation.key,
                count: 1,
                first_seen: observation.observed_at,
                last_seen: observation.observed_at,
            },
        );
    } else {
        record_overflow(&AGGREGATE_DROPS, "aggregate");
    }
}

async fn flush(state: &SharedState, map: &mut HashMap<ObservationKey, Aggregate>) -> bool {
    let Some(game_id) = map.keys().next().map(|key| key.game_id) else {
        return true;
    };
    let keys = map
        .keys()
        .filter(|key| key.game_id == game_id)
        .take(MAX_FLUSH)
        .cloned()
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return true;
    }
    let rows = keys
        .iter()
        .filter_map(|key| map.get(key))
        .map(|row| {
            serde_json::json!({
                "gameId": row.key.game_id,
                "participationId": row.key.participation_id,
                "challengeId": row.key.challenge_id,
                "containerId": row.key.container_id,
                "remoteIp": row.key.remote_ip,
                "count": row.count,
                "firstSeen": row.first_seen,
                "lastSeen": row.last_seen,
            })
        })
        .collect::<Vec<_>>();
    let write = async {
        let mut transaction = state.pg().begin().await.map_err(FlushError::Database)?;
        let scopes = keys
            .iter()
            .filter_map(|key| map.get(key))
            .map(|row| {
                (
                    row.key.game_id,
                    row.key.challenge_id,
                    row.key.participation_id,
                )
            })
            .collect::<BTreeSet<_>>();
        for (game_id, challenge_id, participation_id) in scopes {
            let locked = crate::services::participation_evidence::lock_audit_insert_scope(
                &mut transaction,
                game_id,
                Some(challenge_id),
                &[participation_id],
            )
            .await
            .map_err(|error| FlushError::Database(sqlx::Error::Protocol(error.to_string())))?;
            if !locked {
                return Err(FlushError::Sealed);
            }
        }
        let ids = sqlx::query_scalar::<_, i32>(
            r#"WITH input AS (
                   SELECT * FROM jsonb_to_recordset($1::jsonb) AS row(
                       "gameId" integer, "participationId" integer,
                       "challengeId" integer, "containerId" uuid,
                       "remoteIp" text, "count" bigint,
                       "firstSeen" timestamptz, "lastSeen" timestamptz)
               )
               INSERT INTO "FlagEgressEvents"
                   (game_id, participation_id, challenge_id, container_id,
                    remote_ip, remote_port, hit_count, first_seen_utc, last_seen_utc)
               SELECT row."gameId", row."participationId", row."challengeId",
                      row."containerId", row."remoteIp", 0,
                      LEAST(row."count", 2147483647)::integer,
                      row."firstSeen", row."lastSeen" FROM input row
               ON CONFLICT
                   (game_id, participation_id, challenge_id,
                    (COALESCE(container_id::text, ''::text)), remote_ip, remote_port)
               DO UPDATE SET
                   hit_count = LEAST("FlagEgressEvents".hit_count::bigint + EXCLUDED.hit_count, 2147483647)::integer,
                   first_seen_utc = LEAST("FlagEgressEvents".first_seen_utc, EXCLUDED.first_seen_utc),
                   last_seen_utc = GREATEST("FlagEgressEvents".last_seen_utc, EXCLUDED.last_seen_utc)
               RETURNING id"#,
        )
        .bind(serde_json::Value::Array(rows))
        .fetch_all(&mut *transaction)
        .await
        .map_err(FlushError::Database)?;
        transaction.commit().await.map_err(FlushError::Database)?;
        Ok::<_, FlushError>(ids)
    };
    match tokio::time::timeout(Duration::from_secs(2), write).await {
        Ok(Ok(ids)) => {
            for key in keys {
                map.remove(&key);
            }
            match tokio::time::timeout(
                Duration::from_secs(2),
                crate::services::flag_egress_feed::publish_committed_batch(
                    state.pg(),
                    &state.events,
                    game_id,
                    &ids,
                ),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::debug!(game_id, %error, "flag-egress batch publication failed; HTTP backfill remains authoritative")
                }
                Err(_) => tracing::debug!(
                    game_id,
                    "flag-egress batch publication timed out; HTTP backfill remains authoritative"
                ),
            }
            true
        }
        Ok(Err(FlushError::Sealed)) => {
            let dropped = keys
                .iter()
                .filter_map(|key| map.remove(key))
                .map(|row| u64::try_from(row.count).unwrap_or(u64::MAX))
                .fold(0_u64, u64::saturating_add);
            record_overflow_count(&AGGREGATE_DROPS, "sealed", dropped);
            true
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "flag-egress observation flush failed");
            false
        }
        Err(_) => {
            tracing::warn!("flag-egress observation flush timed out");
            false
        }
    }
}

pub fn start_writer(
    state: &SharedState,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let receiver = state.flag_egress_observations.take_receiver();
    let state = state.clone();
    tokio::spawn(async move {
        let Some(mut receiver) = receiver else {
            tracing::warn!("flag-egress observation writer was started more than once");
            return;
        };
        let mut aggregates = HashMap::new();
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                observation = receiver.recv() => {
                    let Some(observation) = observation else { break; };
                    aggregate(&mut aggregates, observation);
                }
                _ = interval.tick() => { flush(&state, &mut aggregates).await; },
            }
        }
        while let Ok(observation) = receiver.try_recv() {
            aggregate(&mut aggregates, observation);
        }
        for _ in 0..16 {
            if aggregates.is_empty() || !flush(&state, &mut aggregates).await {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_and_aggregate_memory_are_strictly_bounded() {
        assert_eq!(QUEUE_CAPACITY, 2_048);
        assert_eq!(MAX_AGGREGATES, 4_096);
        assert_eq!(MAX_FLUSH, 256);
    }

    fn observation(index: u32) -> Observation {
        Observation {
            key: ObservationKey {
                game_id: 1,
                participation_id: 2,
                challenge_id: 3,
                container_id: Uuid::from_u128(u128::from(index)),
                remote_ip: "192.0.2.10".to_owned(),
            },
            observed_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn fixed_rate_reconnect_bursts_coalesce_and_never_expand_the_bound() {
        let mut map = HashMap::new();
        for _ in 0..10_000 {
            aggregate(&mut map, observation(1));
        }
        assert_eq!(map.len(), 1);
        assert_eq!(map.values().next().unwrap().count, 10_000);

        for index in 0..10_000 {
            aggregate(&mut map, observation(index));
        }
        assert_eq!(map.len(), MAX_AGGREGATES);
    }
}
