//! Durable request/ack coordination between API replicas and the network owner.
//!
//! Notifications only reduce latency. The singleton PostgreSQL cursor remains
//! authoritative across listener disconnects, process crashes, and lost
//! `NOTIFY` messages.

use std::time::Duration;

use sea_orm::DatabaseConnection;
use sqlx::postgres::PgListener;

use crate::utils::error::{AppError, AppResult};

pub(super) const NOTIFY_CHANNEL: &str = "rsctf_ad_network_reconcile";
const APPLY_TIMEOUT: Duration = Duration::from_secs(15);
const APPLY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RECOVERY_INTERVAL: Duration = Duration::from_secs(5);
const SAFETY_AUDIT_INTERVAL: Duration = Duration::from_secs(30);
const LISTENER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LISTENER_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Generation(i64);

impl Generation {
    fn value(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, sqlx::FromRow)]
struct Cursor {
    requested_generation: i64,
    applied_generation: i64,
}

impl Cursor {
    fn pending_snapshot(self) -> Option<Generation> {
        (self.requested_generation > self.applied_generation)
            .then_some(Generation(self.requested_generation))
    }

    fn has_applied(self, generation: Generation) -> bool {
        self.applied_generation >= generation.value()
    }
}

async fn load_cursor(pool: &sqlx::PgPool) -> AppResult<Cursor> {
    sqlx::query_as::<_, Cursor>(
        r#"SELECT requested_generation, applied_generation
             FROM "AdNetworkReconcileState"
            WHERE id = 1"#,
    )
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::internal("A&D network reconcile cursor is missing"))
}

/// Commit a new durable request before publishing its best-effort wake-up.
///
/// Callers invoke this only after their authoritative policy mutation has
/// committed. The update and `pg_notify` deliberately use separate autocommit
/// statements, so an owner can never observe the notification before the
/// generation row is visible. A crash between them is recovered by polling.
pub(super) async fn request(db: &DatabaseConnection) -> AppResult<Generation> {
    let pool = db.get_postgres_connection_pool();
    let generation: i64 = sqlx::query_scalar(
        r#"INSERT INTO "AdNetworkReconcileState"
               (id, requested_generation, applied_generation, requested_at, applied_at)
           VALUES (1, 1, 0, clock_timestamp(), NULL)
           ON CONFLICT (id) DO UPDATE
             SET requested_generation =
                     "AdNetworkReconcileState".requested_generation + 1,
                 requested_at = clock_timestamp()
           RETURNING requested_generation"#,
    )
    .fetch_one(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let generation = Generation(generation);

    if let Err(error) = sqlx::query("SELECT pg_notify($1, $2)")
        .bind(NOTIFY_CHANNEL)
        .bind(generation.value().to_string())
        .execute(pool)
        .await
    {
        tracing::warn!(
            %error,
            generation = generation.value(),
            "A&D network reconcile wake-up failed; durable polling will recover"
        );
    }
    Ok(generation)
}

/// Snapshot the highest generation visible before an owner starts kernel work.
/// An acknowledgement must use this exact value, never a post-apply reload.
pub(super) async fn pending_snapshot(db: &DatabaseConnection) -> AppResult<Option<Generation>> {
    Ok(load_cursor(db.get_postgres_connection_pool())
        .await?
        .pending_snapshot())
}

/// Acknowledge only the generation captured before kernel activation.
///
/// `requested_generation` may have advanced while the owner was applying the
/// snapshot. In that case `applied_generation` advances only to `generation`,
/// leaving the newer request pending for the next pass.
pub(super) async fn acknowledge(db: &DatabaseConnection, generation: Generation) -> AppResult<()> {
    let updated = sqlx::query(
        r#"UPDATE "AdNetworkReconcileState"
              SET applied_generation = GREATEST(applied_generation, $1),
                  applied_at = clock_timestamp()
            WHERE id = 1
              AND requested_generation >= $1"#,
    )
    .bind(generation.value())
    .execute(db.get_postgres_connection_pool())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::internal(format!(
            "A&D network reconcile generation {} could not be acknowledged",
            generation.value()
        )));
    }
    Ok(())
}

/// Wait until the owner acknowledges this caller's committed mutation.
/// Non-owners fail closed rather than claiming the kernel policy is current.
pub(super) async fn wait_until_applied(
    db: &DatabaseConnection,
    generation: Generation,
) -> AppResult<()> {
    wait_until_applied_with(db, generation, APPLY_TIMEOUT, APPLY_POLL_INTERVAL).await
}

async fn wait_until_applied_with(
    db: &DatabaseConnection,
    generation: Generation,
    timeout: Duration,
    poll_interval: Duration,
) -> AppResult<()> {
    let started = tokio::time::Instant::now();
    loop {
        if load_cursor(db.get_postgres_connection_pool())
            .await?
            .has_applied(generation)
        {
            return Ok(());
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Err(AppError::unavailable(format!(
                "timed out waiting for A&D network reconcile generation {}",
                generation.value()
            )));
        }
        tokio::time::sleep(poll_interval.min(timeout.saturating_sub(elapsed))).await;
    }
}

async fn connect_listener(pool: &sqlx::PgPool) -> Result<PgListener, sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen(NOTIFY_CHANNEL).await?;
    Ok(listener)
}

struct AbortTaskOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Run the network owner's immediate LISTEN wake-up plus durable recovery poll.
/// The public lifecycle entry remains `cron::start_network_reconcile`.
pub(crate) fn start_owner_listener(
    state: crate::app_state::SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !super::enabled() {
            return;
        }
        let mut capture_watchdog = AbortTaskOnDrop(tokio::spawn(run_capture_watchdog(
            state.clone(),
            shutdown.clone(),
        )));
        let mut recovery = tokio::time::interval(RECOVERY_INTERVAL);
        recovery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut safety_audit = tokio::time::interval(SAFETY_AUDIT_INTERVAL);
        safety_audit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        safety_audit.reset();
        tracing::info!(
            recovery_seconds = RECOVERY_INTERVAL.as_secs(),
            safety_audit_seconds = SAFETY_AUDIT_INTERVAL.as_secs(),
            "cron: A&D network reconcile listener started"
        );

        loop {
            let listener = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                    continue;
                }
                result = &mut capture_watchdog.0 => {
                    if let Err(error) = result {
                        tracing::error!(%error, "capture route watchdog task failed");
                    }
                    return;
                }
                _ = safety_audit.tick() => {
                    run_owner_audit(&state, "periodic safety audit").await;
                    continue;
                }
                listener = tokio::time::timeout(
                    LISTENER_CONNECT_TIMEOUT,
                    connect_listener(state.pg()),
                ) => listener,
            };
            let mut listener = match listener {
                Ok(Ok(listener)) => listener,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "cron: A&D network reconcile listener connect failed");
                    run_owner_pass(&state, "listener unavailable recovery").await;
                    if wait_for_listener_retry(&state, &mut shutdown, &mut safety_audit).await {
                        return;
                    }
                    continue;
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_seconds = LISTENER_CONNECT_TIMEOUT.as_secs(),
                        "cron: A&D network reconcile listener connect timed out"
                    );
                    run_owner_pass(&state, "listener timeout recovery").await;
                    if wait_for_listener_retry(&state, &mut shutdown, &mut safety_audit).await {
                        return;
                    }
                    continue;
                }
            };

            // LISTEN is active before this recovery read. A request committed
            // before subscription is found here; one committed afterward also
            // queues a notification, closing the startup/reconnect race.
            run_owner_pass(&state, "listener startup/reconnect").await;
            recovery.reset();

            loop {
                let wake = tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return;
                        }
                        continue;
                    }
                    result = &mut capture_watchdog.0 => {
                        if let Err(error) = result {
                            tracing::error!(%error, "capture route watchdog task failed");
                        }
                        return;
                    }
                    _ = recovery.tick() => Some(OwnerWake::Recovery),
                    _ = safety_audit.tick() => Some(OwnerWake::SafetyAudit),
                    notification = listener.try_recv() => match notification {
                        Ok(Some(_)) => Some(OwnerWake::Notification),
                        // PgListener eagerly reconnects before returning None.
                        // Notifications lost during the gap are recovered from
                        // the durable requested/applied cursor immediately.
                        Ok(None) => Some(OwnerWake::Reconnected),
                        Err(error) => {
                            tracing::warn!(%error, "cron: A&D network reconcile listener failed");
                            None
                        }
                    },
                };
                let Some(wake) = wake else {
                    break;
                };
                match wake {
                    OwnerWake::Recovery => run_owner_pass(&state, "periodic recovery").await,
                    OwnerWake::SafetyAudit => {
                        run_owner_audit(&state, "periodic safety audit").await
                    }
                    OwnerWake::Notification => {
                        run_owner_pass(&state, "database notification").await
                    }
                    OwnerWake::Reconnected => {
                        tracing::warn!("cron: A&D network reconcile listener reconnected");
                        run_owner_pass(&state, "listener reconnect").await;
                    }
                }
            }

            if wait_for_listener_retry(&state, &mut shutdown, &mut safety_audit).await {
                return;
            }
        }
    })
}

enum OwnerWake {
    Notification,
    Recovery,
    Reconnected,
    SafetyAudit,
}

async fn run_owner_pass(state: &crate::app_state::SharedState, wake: &'static str) {
    match super::reconcile_pending_for_owner(&state.db).await {
        Ok(true) => tracing::debug!(wake, "cron: applied pending A&D network generation"),
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, wake, "cron: A&D VPN capability reconciliation failed")
        }
    }
}

async fn run_owner_audit(state: &crate::app_state::SharedState, wake: &'static str) {
    match super::audit_owner_state(&state.db).await {
        Ok(true) => tracing::debug!(wake, "cron: audited A&D network owner state"),
        Ok(false) => {}
        Err(error) => tracing::warn!(%error, wake, "cron: A&D VPN safety audit failed"),
    }
}

async fn run_capture_refresh(state: &crate::app_state::SharedState) {
    if let Err(error) = super::capture_policy::refresh(&state.db).await {
        // Never refresh stale membership after a failed authoritative read.
        // Existing live entries expire in the kernel on their own.
        tracing::warn!(%error, "cron: capture route lease refresh failed closed");
    }
}

async fn run_capture_watchdog(
    state: crate::app_state::SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut refresh = tokio::time::interval(super::capture_policy::REFRESH_INTERVAL);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = refresh.tick() => run_capture_refresh(&state).await,
        }
    }
}

/// Returns true when shutdown wins the reconnect backoff.
async fn wait_for_listener_retry(
    state: &crate::app_state::SharedState,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    safety_audit: &mut tokio::time::Interval,
) -> bool {
    tokio::select! {
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
        _ = tokio::time::sleep(LISTENER_RETRY_INTERVAL) => false,
        _ = safety_audit.tick() => {
            run_owner_audit(state, "listener retry safety audit").await;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_does_not_absorb_a_newer_request() {
        let first = Cursor {
            requested_generation: 7,
            applied_generation: 6,
        };
        let snapshot = first.pending_snapshot().unwrap();
        let after_new_request_and_snapshot_ack = Cursor {
            requested_generation: 8,
            applied_generation: snapshot.value(),
        };
        assert_eq!(snapshot, Generation(7));
        assert_eq!(
            after_new_request_and_snapshot_ack.pending_snapshot(),
            Some(Generation(8))
        );
    }

    #[test]
    fn safety_audit_interval_bounds_ticketless_commit_recovery() {
        assert!(SAFETY_AUDIT_INTERVAL <= Duration::from_secs(30));
    }

    #[tokio::test]
    #[ignore = "requires migrated PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_ticket_notify_and_snapshot_ack_are_durable() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to a disposable migrated database");
        let db = sea_orm::Database::connect(&database_url).await.unwrap();
        let pool = db.get_postgres_connection_pool();
        sqlx::query(
            r#"UPDATE "AdNetworkReconcileState"
                  SET requested_generation = 0,
                      applied_generation = 0,
                      requested_at = clock_timestamp(),
                      applied_at = NULL
                WHERE id = 1"#,
        )
        .execute(pool)
        .await
        .unwrap();

        let mut listener = connect_listener(pool).await.unwrap();
        let first = request(&db).await.unwrap();
        let notification = tokio::time::timeout(Duration::from_secs(2), listener.recv())
            .await
            .expect("request notification timed out")
            .unwrap();
        assert_eq!(notification.channel(), NOTIFY_CHANNEL);
        assert_eq!(notification.payload(), first.value().to_string());

        let snapshot = pending_snapshot(&db).await.unwrap().unwrap();
        let newer = request(&db).await.unwrap();
        acknowledge(&db, snapshot).await.unwrap();
        let cursor = load_cursor(pool).await.unwrap();
        assert!(cursor.has_applied(snapshot));
        assert_eq!(cursor.pending_snapshot(), Some(newer));

        acknowledge(&db, newer).await.unwrap();
        wait_until_applied_with(
            &db,
            newer,
            Duration::from_secs(1),
            Duration::from_millis(10),
        )
        .await
        .unwrap();
    }
}
