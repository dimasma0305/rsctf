//! middlewares/user_activity.rs — ported from RSCTF's per-request user activity
//! update (`UserInfo.UpdateByHttpContext`, invoked from `PrivilegeAuthentication`
//! on every authenticated request).
//!
//! RSCTF stamps the acting user's row with their current client IP and a
//! `LastVisitedUtc` timestamp on each authenticated hit, **throttled** to at most
//! once per 5 seconds (`DateTimeOffset.UtcNow - user.LastVisitedUtc >
//! TimeSpan.FromSeconds(5)`). That freshness is what feeds the anti-cheat IP gate
//! and the `SharedIp` / `CrossTeamIp` / `IpChurn` suspicion detectors — without
//! it `user.ip` stays frozen at its registration value and `last_visited_utc`
//! goes stale.
//!
//! Design constraints (mirroring RSCTF's "activity tracking must never break a
//! request"):
//!
//! * **Non-blocking.** Authentication is reused from the handler extractor and
//!   the request only attempts to enqueue an observation into a bounded channel,
//!   so activity never adds a second user lookup, a pool checkout, or database
//!   latency to the response.
//! * **Fail-safe.** Any failure (token invalid, user gone, DB error) is swallowed
//!   — it can never turn a successful request into an error.
//! * **Coalesced.** A sharded in-process gate admits at most one observation per
//!   user every five seconds (or immediately when their IP changes). One worker
//!   batches stable-IP refreshes while the SQL predicate remains the
//!   cross-replica backstop.
//! * **Cheap on the anonymous path.** Non-`/api` traffic and requests without a
//!   handler-authenticated principal perform no activity database work.

use axum::extract::{ConnectInfo, State};
use axum::middleware::Next;
use axum::response::Response;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::app_state::SharedState;

/// RSCTF's activity-update throttle: at most one write per user per 5 seconds.
const THROTTLE: Duration = Duration::from_secs(5);
const SHARDS: usize = 256;
const MAX_GATE_ENTRIES_PER_SHARD: usize = 512;
// The channel and batch are independently bounded: a stalled database cannot
// turn best-effort telemetry into unbounded application memory. At the normal
// five-second cadence, 4,096 slots also absorb a synchronized first poll from a
// large event without making request handlers wait.
const ACTIVITY_QUEUE_CAPACITY: usize = 4_096;
const MAX_ACTIVITY_BATCH_USERS: usize = 512;
// Adds at most 250 ms to the historical five-second stable-IP freshness. An IP
// change bypasses this wait and flushes the current batch immediately.
const ACTIVITY_BATCH_WINDOW: Duration = Duration::from_millis(250);
const ACTIVITY_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityReason {
    Periodic,
    IpChanged,
}

#[derive(Debug)]
struct ActivityObservation {
    user_id: Uuid,
    ip: Option<String>,
    observed_at: chrono::DateTime<chrono::Utc>,
    gate_at: Instant,
    reason: ActivityReason,
}

/// State-owned handoff from request middleware to the single activity writer
/// in this API process. Keeping the receiver in application state avoids a
/// process-global runtime task and lets the composition root drain it cleanly.
pub(crate) struct ActivityQueue {
    sender: mpsc::Sender<ActivityObservation>,
    receiver: Mutex<Option<mpsc::Receiver<ActivityObservation>>>,
}

impl ActivityQueue {
    pub(crate) fn new() -> Self {
        Self::with_capacity(ACTIVITY_QUEUE_CAPACITY)
    }

    fn with_capacity(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
        }
    }

    fn enqueue(&self, observation: ActivityObservation) -> bool {
        match self.sender.try_send(observation) {
            Ok(()) => true,
            Err(error) => {
                let observation = error.into_inner();
                // Do not consume a five-second window for work that was never
                // accepted. The exact-reservation check cannot erase a newer
                // IP observation racing on another request.
                release_activity_window(observation.user_id, &observation.ip, observation.gate_at);
                false
            }
        }
    }

    fn take_receiver(&self) -> Option<mpsc::Receiver<ActivityObservation>> {
        self.receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

#[derive(Clone, Default)]
pub(crate) struct RequestActivityContext(Arc<OnceLock<Uuid>>);

impl RequestActivityContext {
    pub(crate) fn mark_authenticated(&self, user_id: Uuid) {
        let _ = self.0.set(user_id);
    }

    fn user_id(&self) -> Option<Uuid> {
        self.0.get().copied()
    }
}

type ActivityShard = Mutex<HashMap<Uuid, (Instant, Option<String>)>>;
static ACTIVITY_GATES: LazyLock<Box<[ActivityShard]>> =
    LazyLock::new(|| (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect());

fn activity_shard(user_id: Uuid) -> &'static ActivityShard {
    let mut hash = DefaultHasher::new();
    user_id.hash(&mut hash);
    &ACTIVITY_GATES[(hash.finish() as usize) % SHARDS]
}

fn should_persist(user_id: Uuid, ip: &Option<String>, now: Instant) -> Option<ActivityReason> {
    let mut gates = activity_shard(user_id)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if gates.len() >= MAX_GATE_ENTRIES_PER_SHARD && !gates.contains_key(&user_id) {
        gates.retain(|_, (last, _)| {
            now.saturating_duration_since(*last) < Duration::from_secs(3_600)
        });
        if gates.len() >= MAX_GATE_ENTRIES_PER_SHARD {
            if let Some(oldest) = gates
                .iter()
                .min_by_key(|(_, (last, _))| *last)
                .map(|(user_id, _)| *user_id)
            {
                gates.remove(&oldest);
            }
        }
    }
    match gates.get_mut(&user_id) {
        // Two requests can capture their timestamps before taking this shard.
        // Never let the older observation regress the coalescing state.
        Some((last, _)) if now <= *last => None,
        Some((last, previous_ip))
            if now.saturating_duration_since(*last) < THROTTLE && previous_ip == ip =>
        {
            None
        }
        Some((last, previous_ip)) => {
            let reason = if previous_ip == ip {
                ActivityReason::Periodic
            } else {
                ActivityReason::IpChanged
            };
            *last = now;
            previous_ip.clone_from(ip);
            Some(reason)
        }
        None => {
            gates.insert(user_id, (now, ip.clone()));
            Some(ActivityReason::Periodic)
        }
    }
}

/// Undo a just-consumed local window when an observation could not be queued or
/// no pool connection was available to flush its batch.
/// Remove only the exact reservation: a newer request may already have replaced
/// it after observing an IP change.
fn release_activity_window(user_id: Uuid, ip: &Option<String>, now: Instant) {
    let mut gates = activity_shard(user_id)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if gates
        .get(&user_id)
        .is_some_and(|(reserved_at, reserved_ip)| *reserved_at == now && reserved_ip == ip)
    {
        gates.remove(&user_id);
    }
}

fn reserve_activity_observation(
    user_id: Uuid,
    ip: Option<String>,
    observed_at: chrono::DateTime<chrono::Utc>,
    now: Instant,
) -> Option<ActivityObservation> {
    let reason = should_persist(user_id, &ip, now)?;
    Some(ActivityObservation {
        user_id,
        ip,
        observed_at,
        gate_at: now,
        reason,
    })
}

/// Per-request user-activity stamp. Layered over the `/api` router in `server.rs`
/// via [`axum::middleware::from_fn_with_state`] so it can reach the DB and the
/// token service through [`SharedState`].
///
/// Resolves the acting user from the verified session (bearer JWT or the rsctf
/// session cookie); if present, attempts to enqueue a throttled refresh of that
/// user's `ip` and `last_visited_utc`. Anonymous and non-`/api` requests return
/// before any token verify or DB work.
pub async fn middleware(
    State(st): State<SharedState>,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    // Cheap gate first: only authenticated `/api` traffic is tracked.
    if !req.uri().path().starts_with("/api") {
        return next.run(req).await;
    }

    let ip = client_ip(&req);
    let activity = RequestActivityContext::default();
    req.extensions_mut().insert(activity.clone());
    let response = next.run(req).await;

    if let Some(user_id) = activity.user_id() {
        let observed_at = chrono::Utc::now();
        let gate_at = Instant::now();
        if let Some(observation) = reserve_activity_observation(user_id, ip, observed_at, gate_at) {
            // `try_send` is the only request-path work after the sharded gate.
            // A full/closed queue releases this exact local reservation so a
            // later request can retry with a fresher timestamp and IP.
            let _ = st.user_activity.enqueue(observation);
        }
    }

    response
}

/// Start the one best-effort activity writer owned by an API process. The
/// composition root supervises this as an optional worker: telemetry failure
/// must never make the replica unhealthy or stop gameplay.
pub fn start_writer(
    state: &SharedState,
    shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let receiver = state.user_activity.take_receiver();
    let pool = state.pg().clone();
    tokio::spawn(async move {
        let Some(receiver) = receiver else {
            tracing::warn!("user activity writer was started more than once");
            return;
        };
        run_writer(pool, receiver, shutdown).await;
    })
}

fn merge_observation(
    pending: &mut HashMap<Uuid, ActivityObservation>,
    observation: ActivityObservation,
) -> bool {
    let replace = match pending.get(&observation.user_id) {
        Some(current) => observation.gate_at > current.gate_at,
        None => true,
    };
    let flush_immediately = replace && observation.reason == ActivityReason::IpChanged;
    if replace {
        pending.insert(observation.user_id, observation);
    }
    flush_immediately
}

async fn execute_activity_batch(
    connection: &mut sqlx::PgConnection,
    mut batch: Vec<ActivityObservation>,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    // Every API replica must present overlapping rows in the same order. At
    // larger user counts PostgreSQL commonly drives this UPDATE from the
    // UNNEST input into the users PK; randomized HashMap drain order can then
    // make two replicas lock the same rows in opposite orders and deadlock.
    sort_activity_batch(&mut batch);

    let mut user_ids = Vec::with_capacity(batch.len());
    let mut ips = Vec::with_capacity(batch.len());
    let mut observed_at = Vec::with_capacity(batch.len());
    for observation in batch {
        user_ids.push(observation.user_id);
        ips.push(observation.ip);
        observed_at.push(observation.observed_at);
    }

    // One fixed-shape raw-sqlx statement replaces up to 512 individual UPDATE
    // executions. The row predicate keeps the historical semantics and remains
    // the cross-replica/out-of-order backstop.
    sqlx::query(
        r#"WITH activity AS (
               SELECT id, ip, observed_at
                 FROM UNNEST($1::uuid[], $2::text[], $3::timestamptz[])
                      AS observed(id, ip, observed_at)
           )
           UPDATE "AspNetUsers" AS users
              SET last_visited_utc = activity.observed_at,
                  ip = COALESCE(activity.ip, users.ip)
             FROM activity
            WHERE users.id = activity.id
              AND users.last_visited_utc < activity.observed_at
              AND (
                    users.last_visited_utc < activity.observed_at - interval '5 seconds'
                    OR (
                        activity.ip IS NOT NULL
                        AND users.ip IS DISTINCT FROM activity.ip
                    )
                  )"#,
    )
    .bind(user_ids)
    .bind(ips)
    .bind(observed_at)
    .execute(connection)
    .await
}

fn sort_activity_batch(batch: &mut [ActivityObservation]) {
    batch.sort_unstable_by_key(|observation| observation.user_id);
}

async fn flush_pending(pool: &sqlx::PgPool, pending: &mut HashMap<Uuid, ActivityObservation>) {
    if pending.is_empty() {
        return;
    }

    // Draining retains the map allocation for the next 250 ms window while the
    // owned vector keeps this batch stable as new observations enter the queue.
    let batch: Vec<_> = pending
        .drain()
        .map(|(_, observation)| observation)
        .collect();
    let batch_size = batch.len();
    let Some(mut connection) = pool.try_acquire() else {
        // Telemetry never queues behind a saturated gameplay pool. Undo only
        // reservations still matching this dropped batch so the next request
        // can retry instead of losing a full five-second window.
        for observation in &batch {
            release_activity_window(observation.user_id, &observation.ip, observation.gate_at);
        }
        return;
    };

    let update = execute_activity_batch(&mut connection, batch);
    match tokio::time::timeout(ACTIVITY_WRITE_TIMEOUT, update).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, batch_size, "user activity batch update failed")
        }
        Err(_) => tracing::warn!(batch_size, "user activity batch update timed out"),
    }
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

async fn run_writer(
    pool: sqlx::PgPool,
    mut receiver: mpsc::Receiver<ActivityObservation>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut pending = HashMap::with_capacity(MAX_ACTIVITY_BATCH_USERS);
    let mut flush_interval = tokio::time::interval(ACTIVITY_BATCH_WINDOW);
    flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Tokio intervals tick immediately once. Consume that tick so the first
    // stable observation receives the intended coalescing window.
    flush_interval.tick().await;

    loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => {
                receiver.close();
                while let Some(observation) = receiver.recv().await {
                    let urgent = merge_observation(&mut pending, observation);
                    if urgent || pending.len() >= MAX_ACTIVITY_BATCH_USERS {
                        flush_pending(&pool, &mut pending).await;
                    }
                }
                flush_pending(&pool, &mut pending).await;
                break;
            }
            _ = flush_interval.tick() => {
                flush_pending(&pool, &mut pending).await;
            }
            observation = receiver.recv() => {
                let Some(observation) = observation else {
                    flush_pending(&pool, &mut pending).await;
                    break;
                };
                let urgent = merge_observation(&mut pending, observation);
                if urgent || pending.len() >= MAX_ACTIVITY_BATCH_USERS {
                    flush_pending(&pool, &mut pending).await;
                }
            }
        }
    }
}

/// Client IP from sources a client cannot forge past a trusted reverse proxy:
/// `X-Real-IP`, else the **rightmost** `X-Forwarded-For` hop, else the
/// `ConnectInfo` peer. Inlined (the equivalents in `request_log`/`rate_limiter`
/// are private); returns `None` when no origin is resolvable so the existing IP
/// is left untouched rather than overwritten with a lie.
fn client_ip(req: &axum::extract::Request) -> Option<String> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    crate::services::anti_cheat::client_ip(req.headers(), peer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Connection, Row};

    fn observation(
        user_id: Uuid,
        ip: Option<&str>,
        observed_at: chrono::DateTime<chrono::Utc>,
        gate_at: Instant,
        reason: ActivityReason,
    ) -> ActivityObservation {
        ActivityObservation {
            user_id,
            ip: ip.map(str::to_owned),
            observed_at,
            gate_at,
            reason,
        }
    }

    #[test]
    fn repeated_activity_is_coalesced_but_ip_change_is_immediate() {
        let user = Uuid::new_v4();
        let now = Instant::now();
        let first = Some("192.0.2.1".to_string());
        assert_eq!(
            should_persist(user, &first, now),
            Some(ActivityReason::Periodic)
        );
        assert_eq!(
            should_persist(user, &first, now + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            should_persist(
                user,
                &Some("192.0.2.2".to_string()),
                now + Duration::from_secs(1)
            ),
            Some(ActivityReason::IpChanged)
        );
        assert_eq!(
            should_persist(
                user,
                &Some("192.0.2.2".to_string()),
                now + THROTTLE + Duration::from_secs(1)
            ),
            Some(ActivityReason::Periodic)
        );
    }

    #[test]
    fn request_context_records_only_the_authenticated_identity() {
        let context = RequestActivityContext::default();
        let first = Uuid::new_v4();
        context.mark_authenticated(first);
        context.mark_authenticated(Uuid::new_v4());
        assert_eq!(context.user_id(), Some(first));
    }

    #[test]
    fn a_full_queue_does_not_consume_the_activity_window() {
        let queue = ActivityQueue::with_capacity(1);
        let now = Instant::now();
        let observed_at = chrono::Utc::now();
        let first_user = Uuid::new_v4();
        let second_user = Uuid::new_v4();

        let first = reserve_activity_observation(
            first_user,
            Some("192.0.2.43".to_string()),
            observed_at,
            now,
        )
        .unwrap();
        assert!(queue.enqueue(first));

        let second_ip = Some("192.0.2.44".to_string());
        let second =
            reserve_activity_observation(second_user, second_ip.clone(), observed_at, now).unwrap();
        assert!(!queue.enqueue(second));
        assert_eq!(
            should_persist(second_user, &second_ip, now + Duration::from_millis(1)),
            Some(ActivityReason::Periodic)
        );
    }

    #[test]
    fn a_dropped_batch_releases_only_its_own_activity_window() {
        let user = Uuid::new_v4();
        let now = Instant::now();
        let first_ip = Some("192.0.2.45".to_string());
        assert!(should_persist(user, &first_ip, now).is_some());
        release_activity_window(user, &first_ip, now);
        assert!(should_persist(user, &first_ip, now).is_some());

        let newer = now + Duration::from_millis(1);
        let newer_ip = Some("192.0.2.46".to_string());
        assert!(should_persist(user, &newer_ip, newer).is_some());
        release_activity_window(user, &first_ip, now);
        assert_eq!(
            should_persist(user, &newer_ip, newer + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn an_older_concurrent_observation_cannot_replace_the_latest_ip() {
        let user = Uuid::new_v4();
        let earlier = Instant::now();
        let later = earlier + Duration::from_millis(1);
        let newest_ip = Some("192.0.2.80".to_string());

        assert!(should_persist(user, &newest_ip, later).is_some());
        assert_eq!(
            should_persist(user, &Some("192.0.2.79".to_string()), earlier),
            None
        );
        assert_eq!(
            should_persist(user, &newest_ip, later + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn batch_merge_keeps_the_newest_observation_and_marks_ip_changes_urgent() {
        let user = Uuid::new_v4();
        let wall = chrono::Utc::now();
        let earlier = Instant::now();
        let later = earlier + Duration::from_millis(1);
        let mut pending = HashMap::new();

        assert!(!merge_observation(
            &mut pending,
            observation(
                user,
                Some("192.0.2.50"),
                wall,
                earlier,
                ActivityReason::Periodic,
            ),
        ));
        assert!(merge_observation(
            &mut pending,
            observation(
                user,
                Some("192.0.2.51"),
                wall + chrono::Duration::milliseconds(1),
                later,
                ActivityReason::IpChanged,
            ),
        ));
        assert!(!merge_observation(
            &mut pending,
            observation(
                user,
                Some("192.0.2.49"),
                wall - chrono::Duration::milliseconds(1),
                earlier,
                ActivityReason::IpChanged,
            ),
        ));
        let retained = pending.get(&user).unwrap();
        assert_eq!(retained.ip.as_deref(), Some("192.0.2.51"));
        assert_eq!(retained.gate_at, later);
    }

    #[test]
    fn activity_batches_use_a_replica_stable_user_lock_order() {
        let lower = Uuid::from_u128(1);
        let middle = Uuid::from_u128(2);
        let upper = Uuid::from_u128(3);
        let wall = chrono::Utc::now();
        let gate = Instant::now();
        let mut batch = vec![
            observation(
                upper,
                Some("192.0.2.3"),
                wall,
                gate,
                ActivityReason::Periodic,
            ),
            observation(
                lower,
                Some("192.0.2.1"),
                wall,
                gate,
                ActivityReason::Periodic,
            ),
            observation(
                middle,
                Some("192.0.2.2"),
                wall,
                gate,
                ActivityReason::Periodic,
            ),
        ];

        sort_activity_batch(&mut batch);
        assert_eq!(
            batch
                .iter()
                .map(|observation| observation.user_id)
                .collect::<Vec<_>>(),
            vec![lower, middle, upper]
        );
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn batch_sql_preserves_throttle_ip_change_and_timestamp_ordering() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
        let mut connection = sqlx::PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "AspNetUsers" (
              id UUID PRIMARY KEY,
              ip TEXT,
              last_visited_utc TIMESTAMPTZ NOT NULL
            );
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let base: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        let due_at = base + chrono::Duration::seconds(6);
        let stable_due = Uuid::new_v4();
        let stable_throttled = Uuid::new_v4();
        let ip_changed = Uuid::new_v4();
        let stale_observation = Uuid::new_v4();
        let null_ip = Uuid::new_v4();

        sqlx::query(
            r#"INSERT INTO "AspNetUsers" (id, ip, last_visited_utc) VALUES
                 ($1, '192.0.2.1', $6),
                 ($2, '192.0.2.2', $7),
                 ($3, '192.0.2.3', $8),
                 ($4, '192.0.2.4', $8),
                 ($5, '192.0.2.5', $6)"#,
        )
        .bind(stable_due)
        .bind(stable_throttled)
        .bind(ip_changed)
        .bind(stale_observation)
        .bind(null_ip)
        .bind(base)
        .bind(base + chrono::Duration::seconds(4))
        .bind(base + chrono::Duration::seconds(5))
        .execute(&mut connection)
        .await
        .unwrap();

        let gate = Instant::now();
        let result = execute_activity_batch(
            &mut connection,
            vec![
                observation(
                    stable_due,
                    Some("192.0.2.1"),
                    due_at,
                    gate,
                    ActivityReason::Periodic,
                ),
                observation(
                    stable_throttled,
                    Some("192.0.2.2"),
                    due_at,
                    gate,
                    ActivityReason::Periodic,
                ),
                observation(
                    ip_changed,
                    Some("198.51.100.3"),
                    due_at,
                    gate,
                    ActivityReason::IpChanged,
                ),
                observation(
                    stale_observation,
                    Some("198.51.100.4"),
                    base + chrono::Duration::seconds(4),
                    gate,
                    ActivityReason::IpChanged,
                ),
                observation(null_ip, None, due_at, gate, ActivityReason::Periodic),
            ],
        )
        .await
        .unwrap();
        assert_eq!(result.rows_affected(), 3);

        let rows = sqlx::query(r#"SELECT id, ip, last_visited_utc FROM "AspNetUsers""#)
            .fetch_all(&mut connection)
            .await
            .unwrap();
        let state: HashMap<Uuid, (Option<String>, chrono::DateTime<chrono::Utc>)> = rows
            .into_iter()
            .map(|row| (row.get("id"), (row.get("ip"), row.get("last_visited_utc"))))
            .collect();
        assert_eq!(state[&stable_due], (Some("192.0.2.1".into()), due_at));
        assert_eq!(
            state[&stable_throttled],
            (
                Some("192.0.2.2".into()),
                base + chrono::Duration::seconds(4)
            )
        );
        assert_eq!(state[&ip_changed], (Some("198.51.100.3".into()), due_at));
        assert_eq!(
            state[&stale_observation],
            (
                Some("192.0.2.4".into()),
                base + chrono::Duration::seconds(5)
            )
        );
        assert_eq!(state[&null_ip], (Some("192.0.2.5".into()), due_at));

        let duplicate = execute_activity_batch(
            &mut connection,
            vec![observation(
                stable_due,
                Some("192.0.2.1"),
                due_at + chrono::Duration::seconds(1),
                gate + Duration::from_secs(1),
                ActivityReason::Periodic,
            )],
        )
        .await
        .unwrap();
        assert_eq!(duplicate.rows_affected(), 0);

        let changed = execute_activity_batch(
            &mut connection,
            vec![observation(
                stable_due,
                Some("203.0.113.9"),
                due_at + chrono::Duration::seconds(1),
                gate + Duration::from_secs(1),
                ActivityReason::IpChanged,
            )],
        )
        .await
        .unwrap();
        assert_eq!(changed.rows_affected(), 1);
    }
}
