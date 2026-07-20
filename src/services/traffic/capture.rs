//! Durable desired-state reconciliation for live A&D traffic captures.
//!
//! PostgreSQL is the source of truth. Exactly one eligible runtime holds a
//! session advisory lock and owns the process-local libpcap threads. API calls
//! only wake this worker; they never create a capture thread themselves.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};

use crate::app_state::SharedState;
use crate::services::container::ContainerBackendKind;
use crate::utils::error::{AppError, AppResult};

use super::capture_live_with_startup;

mod config;
mod failures;
mod health;
mod owner;
mod shutdown;
mod spec;
mod teardown;

pub(crate) use teardown::destroy_container_after_capture_fence;
pub use teardown::stop_container_capture;

use config::{
    capture_device, capture_enabled, capture_filename, join_capture_thread, reconcile_interval,
    unexpected_exit_error,
};
use health::{OwnerHeartbeat, OwnerToken};
use owner::{release as release_owner, try_acquire as try_acquire_owner, OwnerLease};

const DEFAULT_RECONCILE_SECONDS: u64 = 2;
const CAPTURE_RETRY_DELAY: Duration = Duration::from_secs(30);
const APPLY_TIMEOUT: Duration = Duration::from_secs(15);
const APPLY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CAPTURE_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_RESULT_RETENTION_HOURS: i32 = 24;

const RECONCILE_EVENT: &str = "InternalTrafficCaptureReconcile";
const CAPTURE_IDENTITY_STATE_SQL: &str = r#"SELECT
       EXISTS (
           SELECT 1 FROM "AdTeamServices" service
            WHERE service.container_id = $1
       ) AS has_identity,
       EXISTS (
           SELECT 1
             FROM "AdTeamServices" service
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE service.container_id = $1
              AND challenge.enable_traffic_capture = TRUE
              AND challenge.ad_self_hosted = FALSE
              AND NULLIF(BTRIM(service.host), '') IS NOT NULL
              AND service.port BETWEEN 1 AND 65535
       ) AS is_desired"#;
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
struct DesiredCaptureRow {
    service_id: i32,
    container_id: String,
    host: String,
    port: i32,
    challenge_id: i32,
    participation_id: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Generation(i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::FromRow)]
struct ReconcileCursor {
    requested_generation: i64,
    applied_generation: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::FromRow)]
struct CaptureIdentityState {
    has_identity: bool,
    is_desired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
struct PendingReconcileRequest {
    generation: i64,
    container_id: String,
    action: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReconcileAction {
    Start,
    Stop,
}

impl ReconcileAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Stop => "Stop",
        }
    }
}

impl ReconcileCursor {
    fn pending_snapshot(self) -> Option<Generation> {
        (self.requested_generation > self.applied_generation)
            .then_some(Generation(self.requested_generation))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CaptureSpec {
    service_id: i32,
    container_id: String,
    host_text: String,
    host: IpAddr,
    port: u16,
    challenge_id: i32,
    participation_id: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CaptureFailure {
    spec: CaptureSpec,
    error: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ReconciliationPlan {
    stop: Vec<String>,
    start: Vec<CaptureSpec>,
}

fn reconciliation_plan(
    current: &HashMap<String, CaptureSpec>,
    desired: &HashMap<String, CaptureSpec>,
) -> ReconciliationPlan {
    let mut stop: Vec<String> = current
        .iter()
        .filter(|(id, spec)| desired.get(*id) != Some(*spec))
        .map(|(id, _)| id.clone())
        .collect();
    let mut start: Vec<CaptureSpec> = desired
        .iter()
        .filter(|(id, spec)| current.get(*id) != Some(*spec))
        .map(|(_, spec)| spec.clone())
        .collect();
    stop.sort_unstable();
    start.sort_unstable_by(|left, right| left.container_id.cmp(&right.container_id));
    ReconciliationPlan { stop, start }
}

struct ActiveCapture {
    spec: CaptureSpec,
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<Result<u64, String>>,
}

#[derive(Default)]
struct CaptureRegistry {
    active: HashMap<String, ActiveCapture>,
    retry_after: HashMap<String, Instant>,
}

impl Drop for CaptureRegistry {
    fn drop(&mut self) {
        for capture in self.active.values() {
            capture.stop.store(true, Ordering::Release);
        }
    }
}

impl CaptureRegistry {
    async fn reconcile(
        &mut self,
        state: &SharedState,
        failure_wakeup: &Arc<tokio::sync::Notify>,
        desired: &HashMap<String, CaptureSpec>,
        storage_root: &Path,
    ) -> Vec<CaptureFailure> {
        let mut failures = self.reap_finished().await;
        self.retry_after.retain(|id, _| desired.contains_key(id));

        let current = self
            .active
            .iter()
            .map(|(id, capture)| (id.clone(), capture.spec.clone()))
            .collect();
        let plan = reconciliation_plan(&current, desired);

        // Signal every obsolete capture first, then join all of them before a
        // replacement starts. This prevents an old filter from recording a new
        // container that reuses the same IP/port.
        self.stop_ids(&plan.stop).await;
        for spec in plan.start {
            if self
                .retry_after
                .get(&spec.container_id)
                .is_some_and(|retry_at| *retry_at > Instant::now())
            {
                continue;
            }
            if let Some(failure) = self.start(state, failure_wakeup, spec, storage_root).await {
                failures.push(failure);
            }
        }
        failures
    }

    async fn start(
        &mut self,
        state: &SharedState,
        failure_wakeup: &Arc<tokio::sync::Notify>,
        spec: CaptureSpec,
        storage_root: &Path,
    ) -> Option<CaptureFailure> {
        let out_dir = spec.output_dir(storage_root);
        if let Err(error) = std::fs::create_dir_all(&out_dir) {
            tracing::warn!(
                container = %spec.container_id,
                path = %out_dir.display(),
                %error,
                "traffic capture directory creation failed"
            );
            self.retry_after.insert(
                spec.container_id.clone(),
                Instant::now() + CAPTURE_RETRY_DELAY,
            );
            return Some(CaptureFailure {
                spec,
                error: format!("create capture directory: {error}"),
            });
        }

        let out = out_dir.join(capture_filename(&spec.container_id));
        let device = capture_device();
        let filter = spec.bpf_filter();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_state = state.clone();
        let thread_failure_wakeup = failure_wakeup.clone();
        let (startup_tx, startup_rx) = tokio::sync::oneshot::channel();
        let container_id = spec.container_id.clone();
        let thread_name = format!(
            "cap-{}",
            &crate::utils::codec::sha256_str(&container_id)[..8]
        );
        tracing::info!(
            container = %container_id,
            device = %device,
            %filter,
            path = %out.display(),
            "starting reconciled traffic capture"
        );
        let thread = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let capture_stop = thread_stop.clone();
                let result = capture_live_with_startup(
                    &device,
                    Some(&filter),
                    &out,
                    capture_stop,
                    startup_tx,
                )
                .map_err(|error| {
                    tracing::warn!(
                        container = %container_id,
                        path = %out.display(),
                        %error,
                        "traffic capture stopped with an error"
                    );
                    error.to_string()
                });
                if !thread_stop.load(Ordering::Acquire) {
                    thread_state.readiness.begin_capture_restore();
                    thread_failure_wakeup.notify_one();
                }
                result
            });
        match thread {
            Ok(thread) => match tokio::time::timeout(CAPTURE_STARTUP_TIMEOUT, startup_rx).await {
                Ok(Ok(Ok(()))) => {
                    self.retry_after.remove(&spec.container_id);
                    self.active.insert(
                        spec.container_id.clone(),
                        ActiveCapture { spec, stop, thread },
                    );
                    None
                }
                startup => {
                    stop.store(true, Ordering::Release);
                    let error = match startup {
                        Ok(Ok(Err(error))) => error,
                        Ok(Err(_)) => {
                            "capture thread exited before startup acknowledgement".to_string()
                        }
                        Err(_) => "capture startup acknowledgement timed out".to_string(),
                        Ok(Ok(Ok(()))) => unreachable!("successful startup handled above"),
                    };
                    tracing::warn!(container = %spec.container_id, %error, "traffic capture startup failed");
                    let _ = join_capture_thread(thread).await;
                    self.retry_after.insert(
                        spec.container_id.clone(),
                        Instant::now() + CAPTURE_RETRY_DELAY,
                    );
                    Some(CaptureFailure { spec, error })
                }
            },
            Err(error) => {
                tracing::warn!(container = %spec.container_id, %error, "traffic capture thread spawn failed");
                self.retry_after.insert(
                    spec.container_id.clone(),
                    Instant::now() + CAPTURE_RETRY_DELAY,
                );
                Some(CaptureFailure {
                    spec,
                    error: format!("spawn capture thread: {error}"),
                })
            }
        }
    }

    async fn reap_finished(&mut self) -> Vec<CaptureFailure> {
        let finished: Vec<String> = self
            .active
            .iter()
            .filter(|(_, capture)| capture.thread.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        let mut failures = Vec::with_capacity(finished.len());
        for id in finished {
            let Some(capture) = self.active.remove(&id) else {
                continue;
            };
            let result = join_capture_thread(capture.thread).await;
            let error = unexpected_exit_error(result);
            self.retry_after
                .insert(id, Instant::now() + CAPTURE_RETRY_DELAY);
            failures.push(CaptureFailure {
                spec: capture.spec,
                error,
            });
        }
        failures
    }

    async fn stop_ids(&mut self, ids: &[String]) {
        let mut stopped = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(capture) = self.active.remove(id) {
                capture.stop.store(true, Ordering::Release);
                stopped.push((id.clone(), capture.thread));
            }
            self.retry_after.remove(id);
        }
        for (id, thread) in stopped {
            match join_capture_thread(thread).await {
                Ok(Ok(packets)) => {
                    tracing::info!(container = %id, packets, "traffic capture stopped")
                }
                Ok(Err(error)) => {
                    tracing::debug!(container = %id, %error, "traffic capture stopped after error")
                }
                Err(error) => {
                    tracing::warn!(container = %id, %error, "traffic capture thread panicked")
                }
            }
        }
    }

    async fn stop_all(&mut self) {
        let ids: Vec<String> = self.active.keys().cloned().collect();
        self.stop_ids(&ids).await;
        self.retry_after.clear();
    }

    fn has_active(&self, container_id: &str) -> bool {
        self.active
            .get(container_id)
            .is_some_and(|capture| !capture.thread.is_finished())
    }

    fn has_active_spec(&self, desired: &CaptureSpec) -> bool {
        self.active
            .get(&desired.container_id)
            .is_some_and(|active| active.spec == *desired && !active.thread.is_finished())
    }

    fn active_specs(&self) -> Vec<CaptureSpec> {
        self.active
            .values()
            .filter(|capture| !capture.thread.is_finished())
            .map(|capture| capture.spec.clone())
            .collect()
    }
}

async fn request_reconciliation(
    state: &SharedState,
    container_id: &str,
    action: ReconcileAction,
) -> AppResult<Generation> {
    let mut transaction = state
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let generation: i64 = sqlx::query_scalar(
        r#"INSERT INTO "TrafficCaptureReconcileState"
               (id, requested_generation, applied_generation, requested_at, applied_at)
           VALUES (1, 1, 0, clock_timestamp(), NULL)
           ON CONFLICT (id) DO UPDATE
             SET requested_generation =
                     "TrafficCaptureReconcileState".requested_generation + 1,
                 requested_at = clock_timestamp()
           RETURNING requested_generation"#,
    )
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"INSERT INTO "TrafficCaptureReconcileRequests"
               (generation, container_id, action, requested_at,
                applied_at, succeeded, error)
           VALUES ($1, $2, $3, clock_timestamp(), NULL, NULL, NULL)"#,
    )
    .bind(generation)
    .bind(container_id)
    .bind(action.as_str())
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    state.publish_event(RECONCILE_EVENT, None, generation.to_string());
    Ok(Generation(generation))
}

async fn load_owner_cursor(
    connection: &mut PoolConnection<Postgres>,
) -> Result<ReconcileCursor, sqlx::Error> {
    sqlx::query_as::<_, ReconcileCursor>(
        r#"SELECT requested_generation, applied_generation
             FROM "TrafficCaptureReconcileState"
            WHERE id = 1"#,
    )
    .fetch_optional(&mut **connection)
    .await?
    .ok_or(sqlx::Error::RowNotFound)
}

async fn acknowledge_owner(
    connection: &mut PoolConnection<Postgres>,
    generation: Generation,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        r#"UPDATE "TrafficCaptureReconcileState"
              SET applied_generation = GREATEST(applied_generation, $1),
                  applied_at = clock_timestamp()
            WHERE id = 1
              AND requested_generation >= $1"#,
    )
    .bind(generation.0)
    .execute(&mut **connection)
    .await?;
    if updated.rows_affected() == 1 {
        Ok(())
    } else {
        Err(sqlx::Error::RowNotFound)
    }
}

async fn wait_for_request_result(pool: &PgPool, generation: Generation) -> AppResult<()> {
    let started = tokio::time::Instant::now();
    loop {
        let result = sqlx::query_as::<_, (Option<bool>, Option<String>)>(
            r#"SELECT succeeded, error
                 FROM "TrafficCaptureReconcileRequests"
                WHERE generation = $1"#,
        )
        .bind(generation.0)
        .fetch_optional(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::internal("traffic capture reconcile request is missing"))?;
        match result {
            (Some(true), _) => return Ok(()),
            (Some(false), error) => {
                return Err(AppError::unavailable(error.unwrap_or_else(|| {
                    "traffic capture reconciliation failed".to_string()
                })))
            }
            (None, _) => {}
        }
        let elapsed = started.elapsed();
        if elapsed >= APPLY_TIMEOUT {
            return Err(AppError::unavailable(format!(
                "timed out waiting for traffic capture reconcile generation {}",
                generation.0
            )));
        }
        tokio::time::sleep(APPLY_POLL_INTERVAL.min(APPLY_TIMEOUT.saturating_sub(elapsed))).await;
    }
}

async fn capture_identity_state(
    pool: &PgPool,
    container_id: &str,
) -> AppResult<CaptureIdentityState> {
    sqlx::query_as::<_, CaptureIdentityState>(CAPTURE_IDENTITY_STATE_SQL)
        .bind(container_id)
        .fetch_one(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn desired_captures(
    connection: &mut PoolConnection<Postgres>,
) -> Result<HashMap<String, CaptureSpec>, sqlx::Error> {
    let rows = sqlx::query_as::<_, DesiredCaptureRow>(
        r#"SELECT service.id AS service_id,
                  BTRIM(service.container_id) AS container_id,
                  BTRIM(service.host) AS host,
                  service.port,
                  service.challenge_id,
                  service.participation_id
             FROM "AdTeamServices" service
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE challenge.enable_traffic_capture = TRUE
              AND challenge.ad_self_hosted = FALSE
              AND service.container_id IS NOT NULL
              AND NULLIF(BTRIM(service.container_id), '') IS NOT NULL
              AND NULLIF(BTRIM(service.host), '') IS NOT NULL
              AND service.port BETWEEN 1 AND 65535
            ORDER BY service.id"#,
    )
    .fetch_all(&mut **connection)
    .await?;

    let mut desired: HashMap<String, CaptureSpec> = HashMap::with_capacity(rows.len());
    for row in rows {
        let service_id = row.service_id;
        match CaptureSpec::from_row(row) {
            Ok(spec) => {
                if let Some(existing) = desired.get(&spec.container_id) {
                    tracing::warn!(
                        container = %spec.container_id,
                        first_challenge = existing.challenge_id,
                        duplicate_service = service_id,
                        "duplicate capture container id in desired state; keeping the first row"
                    );
                    continue;
                }
                desired.insert(spec.container_id.clone(), spec);
            }
            Err(error) => {
                tracing::warn!(service = service_id, %error, "ignoring invalid traffic capture desired state")
            }
        }
    }
    Ok(desired)
}

fn request_failure(
    action: &str,
    container_id: &str,
    captures: &CaptureRegistry,
    desired: &HashMap<String, CaptureSpec>,
    capture_supported: bool,
) -> Option<&'static str> {
    match action {
        "Start" if !capture_supported => {
            Some("live traffic capture is unavailable on this runtime or container backend")
        }
        "Start" => match desired.get(container_id) {
            None => Some("the container is no longer eligible for traffic capture"),
            Some(spec) if captures.has_active_spec(spec) => None,
            Some(_) => Some("libpcap capture startup failed; inspect the network-owner logs"),
        },
        "Stop" if captures.has_active(container_id) => {
            Some("the obsolete traffic capture is still active; teardown was not acknowledged")
        }
        "Stop" => None,
        _ => Some("invalid traffic capture reconciliation action"),
    }
}

async fn record_request_results(
    connection: &mut PoolConnection<Postgres>,
    generation: Generation,
    captures: &CaptureRegistry,
    desired: &HashMap<String, CaptureSpec>,
    capture_supported: bool,
) -> Result<(), sqlx::Error> {
    let requests = sqlx::query_as::<_, PendingReconcileRequest>(
        r#"SELECT generation, container_id, action
             FROM "TrafficCaptureReconcileRequests"
            WHERE applied_at IS NULL AND generation <= $1
            ORDER BY generation"#,
    )
    .bind(generation.0)
    .fetch_all(&mut **connection)
    .await?;

    for request in requests {
        let failure = request_failure(
            &request.action,
            &request.container_id,
            captures,
            desired,
            capture_supported,
        );
        sqlx::query(
            r#"UPDATE "TrafficCaptureReconcileRequests"
                  SET applied_at = clock_timestamp(),
                      succeeded = $2,
                      error = $3
                WHERE generation = $1 AND applied_at IS NULL"#,
        )
        .bind(request.generation)
        .bind(failure.is_none())
        .bind(failure)
        .execute(&mut **connection)
        .await?;
    }
    Ok(())
}

async fn prune_request_results(
    connection: &mut PoolConnection<Postgres>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"DELETE FROM "TrafficCaptureReconcileRequests"
            WHERE applied_at IS NOT NULL
              AND applied_at < clock_timestamp() - ($1 * interval '1 hour')"#,
    )
    .bind(REQUEST_RESULT_RETENTION_HOURS)
    .execute(&mut **connection)
    .await?;
    Ok(())
}

async fn reconcile_owner_pass(
    state: &SharedState,
    failure_wakeup: &Arc<tokio::sync::Notify>,
    connection: &mut PoolConnection<Postgres>,
    captures: &mut CaptureRegistry,
    storage_root: &Path,
    capture_supported: bool,
) -> Result<bool, sqlx::Error> {
    // Snapshot before applying. A request that races this pass advances a newer
    // generation and remains pending; this pass never acknowledges unseen work.
    let pending = load_owner_cursor(connection).await?.pending_snapshot();
    let desired = desired_captures(connection).await?;
    let capture_desired = if capture_supported {
        desired.clone()
    } else {
        HashMap::new()
    };
    let mut capture_failures = captures
        .reconcile(state, failure_wakeup, &capture_desired, storage_root)
        .await;
    if !capture_supported {
        capture_failures.extend(desired.values().cloned().map(|spec| {
            CaptureFailure {
                spec,
                error: "live traffic capture is unavailable on this runtime or container backend"
                    .to_string(),
            }
        }));
    }
    if !capture_failures.is_empty() {
        state.readiness.begin_capture_restore();
        failures::persist_and_deactivate(connection, &capture_failures).await?;
    }
    if let Some(generation) = pending {
        record_request_results(
            connection,
            generation,
            captures,
            &desired,
            capture_supported,
        )
        .await?;
        acknowledge_owner(connection, generation).await?;
    }
    prune_request_results(connection).await?;

    // Endpoint rows are already fail-closed and request generations are
    // acknowledged. Retry the independent kernel-policy acknowledgement only
    // after that, so one broken capture cannot strand an unrelated teardown.
    let network_revoked = failures::reconcile_pending(state, connection, APPLY_TIMEOUT).await?;
    // Re-read desired state on the next pass after any failure. Even when an
    // exact deactivation matched no row (because a replacement raced it), this
    // owner cannot claim restored until it has observed and captured that new
    // endpoint identity.
    let all_desired_active = capture_failures.is_empty()
        && capture_desired
            .values()
            .all(|spec| captures.has_active_spec(spec));
    Ok(network_revoked && all_desired_active)
}

/// Request capture startup only after the service's container identity is
/// durably published. Waiting for the owner acknowledgement makes the API
/// response accurately reflect that the desired-state pass has run.
pub async fn start_container_capture(state: &SharedState, container_id: &str) -> AppResult<()> {
    if !capture_identity_state(state.pg(), container_id)
        .await?
        .is_desired
    {
        return Err(AppError::internal(
            "traffic capture startup requested before container publication",
        ));
    }
    let generation = request_reconciliation(state, container_id, ReconcileAction::Start).await?;
    let result = wait_for_request_result(state.pg(), generation).await;
    if result.is_err() {
        // Capture is mandatory for a challenge that enables it. Revoke the
        // endpoint before returning the error so a failed/unsupported libpcap
        // setup can never leave an unrecorded service reachable. Keep the
        // backend identity until the stop fence succeeds; if the owner is down,
        // a later teardown/reconcile pass can recover it safely.
        if let Err(error) =
            crate::services::ad_vpn::deactivate_backend_endpoint(&state.db, container_id).await
        {
            tracing::warn!(container = %container_id, %error, "failed capture startup endpoint revocation");
        }
        match destroy_container_after_capture_fence(state, container_id).await {
            Ok(()) => {}
            Err(error) => tracing::warn!(
                container = %container_id,
                %error,
                "capture startup rollback remains fenced for retry"
            ),
        }
    }
    result
}

/// Fence stale durable capture health before a new network owner builds its
/// first VPN policy. Acquiring the capture advisory lock proves no live owner
/// can still be publishing packet-capture acknowledgements.
pub async fn fence_unowned_capture_owner(pool: &PgPool) -> AppResult<()> {
    let Some(mut owner) = try_acquire_owner(pool)
        .await
        .map_err(AppError::unavailable)?
    else {
        return Ok(());
    };
    let fenced = health::fence_unowned(owner.connection_mut()).await;
    let released = release_owner(owner).await;
    fenced.map_err(|error| AppError::internal(error.to_string()))?;
    released.map_err(AppError::internal)
}

/// Start the singleton, shutdown-aware capture reconciler. Call this only from
/// `all`, `control`, and `network` roles and supervise the returned handle as a
/// required background service.
pub fn start_capture_reconciler(
    state: SharedState,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    // A sole network/control replica must not enter load-balancer rotation
    // before its first ownership attempt has restored every desired capture.
    state.readiness.begin_capture_restore();
    tokio::spawn(run_capture_reconciler(state, shutdown))
}

async fn run_capture_reconciler(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut events = state.events.subscribe();
    let failure_wakeup = Arc::new(tokio::sync::Notify::new());

    let capture_supported =
        capture_enabled() && state.containers.backend_kind() == ContainerBackendKind::Docker;
    if !capture_enabled() {
        tracing::info!("traffic capture packet collection disabled; reconciliation remains active");
    } else if state.containers.backend_kind() != ContainerBackendKind::Docker {
        tracing::info!(
            backend = ?state.containers.backend_kind(),
            "traffic capture packet collection unavailable; reconciliation remains active"
        );
    }

    let storage_root = PathBuf::from(&state.config.storage_root);
    let mut ticker = tokio::time::interval(reconcile_interval());
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut health_tick = tokio::time::interval(Duration::from_secs(2));
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut lease: Option<OwnerLease> = None;
    let mut owner_token: Option<OwnerToken> = None;
    let mut heartbeat: Option<OwnerHeartbeat> = None;
    let mut owner_active = false;
    let mut captures = CaptureRegistry::default();
    tracing::info!("traffic capture reconciler started");

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
            _ = ticker.tick() => {}
            _ = health_tick.tick() => {}
            _ = failure_wakeup.notified() => {}
            event = events.recv() => {
                match event {
                    Ok(event) if event.target == RECONCILE_EVENT => {}
                    Ok(_) => continue,
                    // A lagged receiver may have lost the reconcile hint, so run
                    // an authoritative pass immediately. Periodic polling remains
                    // the durable recovery path when Redis is unavailable.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }

        if heartbeat.as_ref().is_some_and(|pulse| !pulse.is_healthy()) {
            tracing::error!("traffic capture owner heartbeat stopped; terminating owner");
            break;
        }

        if lease.is_none() {
            match try_acquire_owner(state.pg()).await {
                Ok(Some(mut connection)) => {
                    state.readiness.begin_capture_restore();
                    tracing::info!(
                        "traffic capture singleton ownership acquired; restoring desired captures"
                    );
                    let token = match health::claim(connection.connection_mut()).await {
                        Ok(token) => token,
                        Err(error) => {
                            tracing::warn!(%error, "traffic capture durable ownership claim failed");
                            continue;
                        }
                    };
                    let pulse = OwnerHeartbeat::start(state.pg().clone(), token);
                    if let Err(error) =
                        crate::services::ad_vpn::ensure_hub_and_sync(&state.db).await
                    {
                        tracing::warn!(%error, "traffic capture owner fence was not acknowledged");
                        pulse.stop().await;
                        let _ = health::release(connection.connection_mut(), token).await;
                        let _ = release_owner(connection).await;
                        continue;
                    }
                    owner_token = Some(token);
                    heartbeat = Some(pulse);
                    owner_active = false;
                    lease = Some(connection);
                }
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, "traffic capture ownership attempt failed");
                    continue;
                }
            }
        }

        let Some(owner) = lease.as_mut() else {
            continue;
        };
        let pass = reconcile_owner_pass(
            &state,
            &failure_wakeup,
            owner.connection_mut(),
            &mut captures,
            &storage_root,
            capture_supported,
        )
        .await;
        match pass {
            Ok(restored) => {
                let token = owner_token.expect("owned capture session has a durable token");
                let mut policy_changed = match health::publish_live(
                    owner.connection_mut(),
                    token,
                    &captures.active_specs(),
                )
                .await
                {
                    Ok(changed) => changed,
                    Err(error) => {
                        tracing::warn!(%error, "traffic capture live publication failed");
                        break;
                    }
                };
                if restored && !owner_active {
                    if let Err(error) = health::activate(owner.connection_mut(), token).await {
                        tracing::warn!(%error, "traffic capture owner activation failed");
                        break;
                    }
                    owner_active = true;
                    policy_changed = true;
                }
                if policy_changed {
                    if let Err(error) =
                        crate::services::ad_vpn::ensure_hub_and_sync(&state.db).await
                    {
                        tracing::warn!(%error, "traffic capture route publication failed");
                        break;
                    }
                }
                if restored && owner_active {
                    state.readiness.finish_capture_restore();
                }
            }
            Err(error) => {
                tracing::warn!(%error, "traffic capture owner query failed; fencing ownership");
                state.readiness.begin_capture_restore();
                break;
            }
        }
    }

    shutdown::drain_owner(&state, owner_token, heartbeat, lease, &mut captures).await;
    tracing::info!("traffic capture reconciler stopped");
}

#[cfg(test)]
mod tests;
