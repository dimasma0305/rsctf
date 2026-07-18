//! services/cron/mod.rs — ported from RSCTF `Services/CronJob/*`.
//!
//! RSCTF runs its scheduled maintenance as an `IHostedService`
//! (`CronJobService`) that owns a one-minute `Timer`, elects a single leader
//! across replicas through a Redis/`IDistributedCache` lock, and — on each tick
//! — fires any `[CronJob]`-attributed job in `RuntimeCronJobs` whose `Cronos`
//! expression is due. The concrete jobs there are the container reaper
//! (`ContainerChecker`), the scoreboard-cache maintenance
//! (`BootstrapCache` / `FlushRecentGames`), and assorted pruners.
//!
//! Here we reproduce that shape with Tokio: a single supervisor task driven by a
//! `tokio::time::interval`, a best-effort Redis `SET NX` leader lock, and a
//! fixed set of DB-backed jobs run every tick:
//!
//!   * [`reap_expired_containers`] — destroy container rows whose
//!     `expect_stop_at` has passed (mirrors `RuntimeCronJobs.ContainerChecker`
//!     + `ContainerRepository.DestroyContainer`).
//!   * [`flush_stale_scoreboards`] — evict the live scoreboard cache keys for
//!     recently-ended games plus the recent-games list (mirrors
//!     `CacheHelper.FlushScoreboardCache` / `FlushRecentGamesCache`).
//!   * the round scheduler — for every running Attack-Defense game whose
//!     current round has ended, advance it: finalize the round, open round
//!     `N+1` sized from `ad_tick_seconds`, and plant a fresh rotating `ad_flag`
//!     for every `ad_team_service` (mirrors `AdRoundService.AdvanceAsync`).
//!     This automatic checker pipeline is the only path allowed to create rounds.
//!
//! NOTE: wiring this in is a one-liner — call `crate::services::cron::start(state.clone())`
//! once after `AppState` is built in `main.rs` / `server.rs`. `main.rs` is not
//! required to call it for the crate to build.

use std::time::Duration as StdDuration;

use chrono::{Duration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::app_state::SharedState;
use crate::models::data::{ad_team_service, container, game, koth_target};
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
use crate::utils::error::AppResult;

mod round_finish;
mod scheduler;

/// Cache key for cross-replica maintenance ownership.
///
/// Round ownership deliberately does not use a deployment-wide Redis leader:
/// every engine replica may look for due games and the durable per-game/round
/// PostgreSQL locks and leases remain the final arbiter. That lets adding an
/// engine replica increase useful concurrency instead of merely creating a hot
/// standby.
const CRON_JOB_LOCK: &str = "_CronJobLock";

/// Leader-lock TTL in seconds. A keepalive renews it every third of this window
/// while jobs run; a dead leader still lapses within a couple of ticks.
const LOCK_TTL_SECS: i64 = 90;

/// Redis must confirm a renewal well before the lease can expire. A wedged
/// connection is ownership loss, not permission to keep mutating shared state.
const LOCK_IO_TIMEOUT_SECS: u64 = 10;

/// Maintenance stays on a 30-second cadence; the latency-sensitive round driver
/// has its own five-second scheduler so reaping/Docker work cannot delay scoring.
const MAINTENANCE_TICK_SECONDS: u64 = 30;
const ROUND_TICK_SECONDS: u64 = 5;

/// Hard cap on ONE game's advance (finalize + open + plant flags + run the checker +
/// KotH). A game with many hung/offline services can make its checker pass take
/// minutes; this stops it blocking every other game, the reaper, and the next tick (#5).
pub(super) const ADVANCE_BUDGET_SECS: u64 = 240;
const ORPHAN_GRACE_SECS: u64 = 60;
static ORPHAN_FIRST_SEEN: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// How far back a game counts as "recently ended" for the stale-scoreboard
/// sweep. Bounds the flush work so it doesn't re-touch every game forever.
const RECENT_ENDED_HOURS: i64 = 6;

/// Which games one round-scheduler replica is eligible to drive.
///
/// BYOC tunnels and their yamux control streams are process-local to the active
/// network owner. A standalone engine replica must therefore leave any game
/// containing a self-hosted A&D service to that network owner. A combined
/// `all`/`control` process uses [`All`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundSchedulerScope {
    /// Combined single-process/control deployment: drive every engine game.
    All,
    /// Horizontally-scaled engine worker: drive games with managed services only.
    ManagedOnly,
    /// Active network owner: drive only games that contain a BYOC service.
    NetworkBoundOnly,
}

/// Launch kernel-local VPN reconciliation. Only the process that is eligible to
/// own the VPN/BYOC network capability should call this function.
pub fn start_network_reconcile(
    state: SharedState,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    crate::services::ad_vpn::coordination::start_owner_listener(state, shutdown)
}

/// Launch singleton deployment maintenance. Multiple engine replicas may call
/// this; the Redis lease elects at most one active maintenance pass.
pub fn start_maintenance(
    state: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let maintenance = state.clone();
    tokio::spawn(async move {
        let mut lock = LeaderLock::connect(CRON_JOB_LOCK, "maintenance").await;
        let mut ticker = tokio::time::interval(StdDuration::from_secs(MAINTENANCE_TICK_SECONDS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tracing::info!("cron: maintenance supervisor started (tick {MAINTENANCE_TICK_SECONDS}s)");

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                    continue;
                }
                _ = ticker.tick() => {}
            }
            if !lock.try_acquire().await {
                continue;
            }
            run_with_lease(&mut lock, run_jobs(&maintenance), "maintenance").await;
        }
    })
}

/// Launch the latency-sensitive round driver.
///
/// Every eligible engine replica runs the poller. PostgreSQL game locks, unique
/// round constraints, and durable pipeline leases decide ownership, so this is
/// active-active rather than a Redis-elected singleton.
pub fn start_round_scheduler(
    state: SharedState,
    scope: RoundSchedulerScope,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(StdDuration::from_secs(ROUND_TICK_SECONDS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tracing::info!(
            ?scope,
            "cron: A&D round supervisor started (tick {ROUND_TICK_SECONDS}s)"
        );
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                    continue;
                }
                _ = ticker.tick() => {}
            }
            run_round_jobs(&state, scope).await;
        }
    })
}

/// Backwards-compatible all-in-one startup used by the default role.
pub fn start(
    state: SharedState,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    vec![
        start_network_reconcile(state.clone(), shutdown.clone()),
        start_maintenance(state.clone(), shutdown.clone()),
        start_round_scheduler(state, RoundSchedulerScope::All, shutdown),
    ]
}

async fn run_with_lease(
    lock: &mut LeaderLock,
    work: impl std::future::Future<Output = ()>,
    label: &'static str,
) {
    let Some((stop, mut keepalive)) = lock.start_keepalive() else {
        work.await;
        return;
    };
    tokio::pin!(work);
    tokio::select! {
        _ = &mut work => {
            let _ = stop.send(());
            if !keepalive.await.unwrap_or(false) {
                lock.holds = false;
                lock.conn = None;
            }
        }
        ownership = &mut keepalive => {
            lock.holds = false;
            if !ownership.unwrap_or(false) {
                tracing::warn!(supervisor = label, "cron: leader lease lost; cancelling this pass");
            }
        }
    }
}

/// Run every job once, logging outcomes. Jobs are independent: one failing does
/// not abort the others (mirrors RSCTF running each `CronJob` in its own scope
/// and swallowing per-job exceptions).
async fn run_jobs(state: &SharedState) {
    match crate::services::blob_refs::purge_pending(state.pg(), state.storage.as_ref(), 128).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: purged deferred blob tombstone(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: deferred blob purge failed: {e}"),
    }

    match crate::services::git_sync::collect_stale_checker_revisions(state).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: collected stale checker revision(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: checker revision GC failed: {e}"),
    }

    match reap_expired_containers(state).await {
        Ok(n) if n > 0 => tracing::info!("cron: reaped {n} expired container(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: container reaper failed: {e}"),
    }

    match complete_ended_ad_checks(state).await {
        Ok(n) if n > 0 => {
            tracing::info!("cron: sealed final checker evidence for {n} ended game(s)")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: ended-game checker completion failed: {e}"),
    }

    match crate::services::ad_engine::koth_cycle::recover_ended_cycle_transitions(state).await {
        Ok(n) if n > 0 => {
            tracing::info!("cron: recovered {n} ended KotH crown-cycle transition(s)")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: ended KotH crown-cycle recovery failed: {e}"),
    }

    match reap_ended_ad_backends(state).await {
        Ok(n) if n > 0 => tracing::info!("cron: reaped {n} ended-game A&D backend(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: ended-game A&D teardown failed: {e}"),
    }

    match sweep_orphan_containers(state).await {
        Ok(n) if n > 0 => tracing::info!("cron: swept {n} orphan container(s) (no DB row)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: orphan sweep failed: {e}"),
    }

    match flush_stale_scoreboards(state).await {
        Ok(n) if n > 0 => tracing::debug!("cron: flushed scoreboard cache for {n} game(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: scoreboard flush failed: {e}"),
    }

    // KotH accrual needs no dedicated job: the live holder snapshot on
    // `koth_target` (`holder_participation_id` + `held_since`) is authoritative,
    // and the scoreboard builder in `controllers::koth` credits the current
    // holder `(now - held_since)` seconds at render time, so the still-open hold
    // window is always accounted for without persisting anything per tick.
}

async fn run_round_jobs(state: &SharedState, scope: RoundSchedulerScope) {
    match scheduler::advance_ad_rounds(state, scope).await {
        Ok(n) if n > 0 => tracing::debug!("cron: advanced {n} A&D round(s)"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "cron: A&D round advance failed"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Leader lock — best-effort Redis SET NX (RSCTF `CronJobService.TryHoldLock`).
// ─────────────────────────────────────────────────────────────────────────────

/// Cross-replica leader election over Redis. When no `RSCTF_REDIS_URL` is set
/// (the in-memory cache path) there is nothing to coordinate — a single node is
/// always the leader — so the lock is skipped entirely.
struct LeaderLock {
    /// `None` means Redis was explicitly unconfigured and single-node mode applies.
    url: Option<String>,
    conn: Option<redis::aio::ConnectionManager>,
    token: String,
    /// Whether this node currently holds the lock (renew instead of re-acquire).
    holds: bool,
    key: &'static str,
    label: &'static str,
}

impl LeaderLock {
    /// Open the leader-lock Redis connection from `RSCTF_REDIS_URL`, degrading to
    /// a lock-free single-node loop only when the variable is explicitly unset.
    /// A configured but unavailable Redis fails closed until reconnection.
    async fn connect(key: &'static str, label: &'static str) -> Self {
        let Ok(url) = std::env::var("RSCTF_REDIS_URL") else {
            tracing::debug!(
                "cron: RSCTF_REDIS_URL unset; running without leader lock (single node)"
            );
            return Self {
                url: None,
                conn: None,
                token: crate::utils::codec::random_token(24),
                holds: false,
                key,
                label,
            };
        };
        let token = crate::utils::codec::random_token(24);
        match redis::Client::open(url.clone()) {
            Ok(client) => match tokio::time::timeout(
                StdDuration::from_secs(LOCK_IO_TIMEOUT_SECS),
                crate::utils::redis::connection_manager(&client),
            )
            .await
            {
                Ok(Ok(conn)) => {
                    tracing::debug!("cron: redis leader lock enabled");
                    Self {
                        url: Some(url),
                        conn: Some(conn),
                        token,
                        holds: false,
                        key,
                        label,
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!("cron: redis connect failed ({e}); scheduled jobs fail closed");
                    Self {
                        url: Some(url),
                        conn: None,
                        token,
                        holds: false,
                        key,
                        label,
                    }
                }
                Err(_) => {
                    tracing::warn!(
                        "cron: redis connect timed out after {LOCK_IO_TIMEOUT_SECS}s; scheduled jobs fail closed"
                    );
                    Self {
                        url: Some(url),
                        conn: None,
                        token,
                        holds: false,
                        key,
                        label,
                    }
                }
            },
            Err(e) => {
                tracing::warn!("cron: invalid RSCTF_REDIS_URL ({e}); scheduled jobs fail closed");
                Self {
                    url: Some(url),
                    conn: None,
                    token,
                    holds: false,
                    key,
                    label,
                }
            }
        }
    }

    /// (Re)take the leader lock for this tick. Fail-closed: if Redis is present
    /// but we cannot confirm ownership, return `false` so this node stands down
    /// rather than risk two leaders running the jobs at once.
    ///
    /// * no Redis        → always `true` (single node),
    /// * already leader   → renew the TTL via `EXPIRE` (re-acquire if it lapsed),
    /// * otherwise        → atomic `SET key 1 NX EX ttl`.
    async fn try_acquire(&mut self) -> bool {
        let Some(url) = self.url.as_deref() else {
            return true;
        };
        if self.conn.is_none() {
            let Ok(client) = redis::Client::open(url) else {
                return false;
            };
            match tokio::time::timeout(
                StdDuration::from_secs(LOCK_IO_TIMEOUT_SECS),
                crate::utils::redis::connection_manager(&client),
            )
            .await
            {
                Ok(Ok(conn)) => self.conn = Some(conn),
                Ok(Err(error)) => {
                    tracing::warn!(%error, "cron: redis reconnect failed; scheduled jobs standing down");
                    return false;
                }
                Err(_) => {
                    tracing::warn!(
                        "cron: redis reconnect timed out after {LOCK_IO_TIMEOUT_SECS}s; scheduled jobs standing down"
                    );
                    return false;
                }
            }
        }
        let Some(mut conn) = self.conn.clone() else {
            return false;
        };

        if self.holds {
            let renewed = tokio::time::timeout(
                StdDuration::from_secs(LOCK_IO_TIMEOUT_SECS),
                redis::Script::new(
                    r#"if redis.call('GET', KEYS[1]) == ARGV[1] then
                         return redis.call('EXPIRE', KEYS[1], ARGV[2])
                       end
                       return 0"#,
                )
                .key(self.key)
                .arg(&self.token)
                .arg(LOCK_TTL_SECS)
                .invoke_async::<i64>(&mut conn),
            )
            .await;
            let renewed = match renewed {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "cron: redis lease renewal failed; scheduled jobs standing down");
                    self.conn = None;
                    self.holds = false;
                    return false;
                }
                Err(_) => {
                    tracing::warn!(
                        "cron: redis lease renewal timed out after {LOCK_IO_TIMEOUT_SECS}s; scheduled jobs standing down"
                    );
                    self.conn = None;
                    self.holds = false;
                    return false;
                }
            };
            if renewed == 1 {
                return true;
            }
            // Our lease lapsed (or the key was evicted) — fall through and race
            // for it again like any other contender.
            self.holds = false;
        }

        let acquired = tokio::time::timeout(
            StdDuration::from_secs(LOCK_IO_TIMEOUT_SECS),
            redis::cmd("SET")
                .arg(self.key)
                .arg(&self.token)
                .arg("NX")
                .arg("EX")
                .arg(LOCK_TTL_SECS)
                .query_async::<Option<String>>(&mut conn),
        )
        .await;

        let acquired = match acquired {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                tracing::warn!(%error, "cron: redis lease acquisition failed; scheduled jobs standing down");
                self.conn = None;
                self.holds = false;
                return false;
            }
            Err(_) => {
                tracing::warn!(
                    "cron: redis lease acquisition timed out after {LOCK_IO_TIMEOUT_SECS}s; scheduled jobs standing down"
                );
                self.conn = None;
                self.holds = false;
                return false;
            }
        };

        self.holds = acquired.is_some();
        self.holds
    }

    fn start_keepalive(
        &self,
    ) -> Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<bool>,
    )> {
        let mut conn = self.conn.clone()?;
        let token = self.token.clone();
        let key = self.key;
        let label = self.label;
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(StdDuration::from_secs((LOCK_TTL_SECS as u64 / 3).max(1)));
            loop {
                tokio::select! {
                    _ = &mut stop_rx => return true,
                    _ = interval.tick() => {
                        let renewed = tokio::time::timeout(
                            StdDuration::from_secs(LOCK_IO_TIMEOUT_SECS),
                            redis::Script::new(
                                r#"if redis.call('GET', KEYS[1]) == ARGV[1] then
                                     return redis.call('EXPIRE', KEYS[1], ARGV[2])
                                   end
                                   return 0"#,
                            )
                            .key(key)
                            .arg(&token)
                            .arg(LOCK_TTL_SECS)
                            .invoke_async::<i64>(&mut conn),
                        )
                        .await;
                        match renewed {
                            Ok(Ok(1)) => {}
                            Ok(Ok(_)) => return false,
                            Ok(Err(error)) => {
                                tracing::warn!(supervisor = label, %error, "cron: leader keepalive failed");
                                return false;
                            }
                            Err(_) => {
                                tracing::warn!(
                                    "cron: leader keepalive timed out after {LOCK_IO_TIMEOUT_SECS}s"
                                );
                                return false;
                            }
                        }
                    }
                }
            }
        });
        Some((stop_tx, task))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Job 1 — container reaper (RSCTF `RuntimeCronJobs.ContainerChecker`).
// ─────────────────────────────────────────────────────────────────────────────

/// Destroy every container whose `expect_stop_at` has passed: clear the owning
/// `game_instance` link, tear down the backing runtime workload, then delete the
/// row. Returns the number of containers reaped.
///
/// This mirrors `admin.rs::destroy_instance` per row so the periodic reaper and
/// the manual admin teardown can't drift apart. A backend `destroy` failure is
/// best-effort (logged, not fatal): the row is still deleted so a vanished
/// daemon can't wedge the table.
async fn reap_expired_containers(state: &SharedState) -> AppResult<u64> {
    let now = Utc::now();

    let expired = container::Entity::find()
        .filter(container::Column::ExpectStopAt.lt(now))
        .all(&state.db)
        .await?;

    let mut reaped = 0u64;
    for c in expired {
        match crate::controllers::game::destroy_managed_container_row(state, &c, true).await {
            Ok(true) => reaped += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(
                container = %c.id,
                backend_id = %c.container_id,
                %error,
                "cron: endpoint revocation failed; retaining expired container"
            ),
        }
    }

    Ok(reaped)
}

/// Revoke and destroy A&D/KotH backends once their game window closes. These
/// workloads are not represented by expiring `Containers` rows in every path,
/// so they need an explicit end-of-game lifecycle sweep.
async fn reap_ended_ad_backends(state: &SharedState) -> AppResult<u64> {
    let services: Vec<(i32, i32, i32)> = sqlx::query_as(
        r#"SELECT service.id, service.participation_id, service.challenge_id
             FROM "AdTeamServices" service
             JOIN "Games" game ON game.id = service.game_id
            WHERE game.end_time_utc <= now() - ($1 * interval '1 second')
              AND (service.container_id IS NOT NULL OR service.host <> '')
            ORDER BY service.id"#,
    )
    .bind(ADVANCE_BUDGET_SECS as i64)
    .fetch_all(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let mut reaped = 0u64;
    for (service_id, participation_id, challenge_id) in services {
        let key = format!("ad-service:{participation_id}:{challenge_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(state.pg(), &key)
                .await?;
        let row = ad_team_service::Entity::find_by_id(service_id)
            .one(&state.db)
            .await?;
        if let Some(row) = row {
            let ended = game::Entity::find_by_id(row.game_id)
                .one(&state.db)
                .await?
                .is_none_or(|game| {
                    game.end_time_utc <= Utc::now() - Duration::seconds(ADVANCE_BUDGET_SECS as i64)
                });
            if ended && (row.container_id.is_some() || !row.host.is_empty()) {
                let backend_id = row.container_id.clone();
                crate::services::ad_vpn::deactivate_team_service(&state.db, row.id).await?;
                if let Some(backend_id) = backend_id {
                    crate::services::traffic::stop_container_capture(state, &backend_id).await?;
                    let _ = state.containers.destroy(&backend_id).await;
                }
                reaped += 1;
            }
        }
        distributed.release().await?;
    }

    let targets: Vec<(i32, i32)> = sqlx::query_as(
        r#"SELECT target.id, target.challenge_id
             FROM "KothTargets" target
             JOIN "Games" game ON game.id = target.game_id
            WHERE game.end_time_utc <= now() - ($1 * interval '1 second')
              AND (target.container_id IS NOT NULL OR target.host <> '')
            ORDER BY target.id"#,
    )
    .bind(ADVANCE_BUDGET_SECS as i64)
    .fetch_all(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    for (target_id, challenge_id) in targets {
        let key = format!("shared-container:{challenge_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(state.pg(), &key)
                .await?;
        let target = koth_target::Entity::find_by_id(target_id)
            .one(&state.db)
            .await?;
        if let Some(target) = target {
            let ended = game::Entity::find_by_id(target.game_id)
                .one(&state.db)
                .await?
                .is_none_or(|game| {
                    game.end_time_utc <= Utc::now() - Duration::seconds(ADVANCE_BUDGET_SECS as i64)
                });
            if ended && (target.container_id.is_some() || !target.host.is_empty()) {
                let backend_id = target.container_id.clone();
                let mut active: koth_target::ActiveModel = target.into();
                active.host = Set(String::new());
                active.port = Set(0);
                active.holder_participation_id = Set(None);
                active.held_since = Set(None);
                active.update(&state.db).await?;
                crate::services::ad_vpn::ensure_hub_and_sync(&state.db).await?;
                let mut cleaned = backend_id.is_none();
                if let Some(backend_id) = backend_id {
                    match state.containers.destroy(&backend_id).await {
                        Ok(()) => {
                            sqlx::query(
                                r#"UPDATE "KothTargets" SET container_id = NULL
                                    WHERE id = $1 AND container_id = $2"#,
                            )
                            .bind(target_id)
                            .bind(&backend_id)
                            .execute(state.pg())
                            .await
                            .map_err(|error| {
                                crate::utils::error::AppError::internal(error.to_string())
                            })?;
                            cleaned = true;
                        }
                        Err(error) => tracing::warn!(
                            target = target_id,
                            backend_id,
                            %error,
                            "cron: ended KotH backend destroy failed; retaining id for retry"
                        ),
                    }
                }
                reaped += u64::from(cleaned);
            }
        }
        distributed.release().await?;
    }
    Ok(reaped)
}

async fn complete_ended_ad_checks(state: &SharedState) -> AppResult<u64> {
    let game_ids: Vec<i32> = sqlx::query_scalar(
        r#"SELECT game.id
             FROM "Games" game
            WHERE game.end_time_utc <= now()
              AND EXISTS (
                    SELECT 1 FROM "AdRounds" round
                     WHERE round.game_id = game.id
                       AND round.finalized = FALSE
                       AND (
                            NOT EXISTS (
                                SELECT 1 FROM "AdCheckResults" pending
                                 WHERE pending.round_id = round.id
                                   AND pending.sla_credit IS NULL
                            )
                            AND NOT EXISTS (
                                SELECT 1
                                  FROM "KothTargets" target
                                  JOIN "GameChallenges" challenge
                                    ON challenge.id = target.challenge_id
                                   AND challenge.game_id = target.game_id
                                 WHERE target.game_id = game.id
                                   AND challenge.is_enabled = TRUE
                                   AND challenge.review_status = $2
                                   AND challenge."Type" = $3
                                   AND NOT EXISTS (
                                        SELECT 1 FROM "KothControlResults" result
                                         WHERE result.game_id = target.game_id
                                           AND result.challenge_id = target.challenge_id
                                           AND result.ad_round_id = round.id
                                   )
                            )
                            OR game.end_time_utc <=
                               now() - ($1 * interval '1 second')
                       )
              )
            ORDER BY game.id"#,
    )
    .bind(ADVANCE_BUDGET_SECS as i64)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_all(state.pg())
    .await
    .map_err(|error| crate::utils::error::AppError::internal(error.to_string()))?;
    let mut completed = 0u64;
    for game_id in game_ids {
        if !crate::services::ad_engine::finalize_ended_round_checks(
            &state.db,
            game_id,
            ADVANCE_BUDGET_SECS as i64,
        )
        .await?
        {
            continue;
        }
        // Final evidence is immutable now. Materialize both score families
        // before the post-event board is asked to repair them on a user request.
        if !round_finish::refresh_score_rollups(state, game_id).await {
            continue;
        }
        completed += 1;
        crate::controllers::game::ad::hard_invalidate_ad_scoreboard(state, game_id).await;
        state
            .cache
            .remove(&format!("_KothScoreBoard_{game_id}"))
            .await;
        state
            .cache
            .remove(&format!("_KothScoreBoardFrozen_{game_id}"))
            .await;
        state
            .cache
            .remove(&format!("_KothTimeline_{game_id}"))
            .await;
        state
            .cache
            .remove(&format!("_KothTimelineFrozen_{game_id}"))
            .await;
        if let Ok(challenge_ids) = sqlx::query_scalar::<_, i32>(
            r#"SELECT challenge_id FROM "KothTargets" WHERE game_id = $1"#,
        )
        .bind(game_id)
        .fetch_all(state.pg())
        .await
        {
            for challenge_id in challenge_ids {
                state
                    .cache
                    .remove(&format!("_KothHillState_{game_id}_{challenge_id}"))
                    .await;
            }
        }
    }
    Ok(completed)
}

/// Destroy every platform-managed container still running on the backend whose
/// owning `Containers` row has vanished — the leak the reaper above can't catch
/// because it only walks DB rows. Covers a create that started the container but
/// failed to persist its row, and a row deleted without a backend destroy. Match
/// is by id prefix (the DB stores the full 64-char id; the daemon may report
/// either), so a live tracked container is never swept.
async fn sweep_orphan_containers(state: &SharedState) -> AppResult<u64> {
    let managed = state.containers.list_managed().await;
    if managed.is_empty() {
        return Ok(0);
    }
    let mut known: Vec<String> = container::Entity::find()
        .filter(container::Column::ContainerId.ne(""))
        .all(&state.db)
        .await?
        .into_iter()
        .map(|c| c.container_id)
        .collect();
    // A&D / KotH per-team service containers live in `ad_team_service`, NOT the
    // `container` table — without this they look orphaned and the sweep destroys
    // them every tick, leaving every platform-hosted service permanently Offline.
    known.extend(
        ad_team_service::Entity::find()
            .filter(ad_team_service::Column::ContainerId.is_not_null())
            .filter(ad_team_service::Column::ContainerId.ne(""))
            .all(&state.db)
            .await?
            .into_iter()
            .filter_map(|s| s.container_id),
    );
    known.extend(
        koth_target::Entity::find()
            .filter(koth_target::Column::ContainerId.is_not_null())
            .filter(koth_target::Column::ContainerId.ne(""))
            .all(&state.db)
            .await?
            .into_iter()
            .filter_map(|target| target.container_id),
    );
    let is_known = |id: &str| {
        known
            .iter()
            .any(|k| k == id || id.starts_with(k.as_str()) || k.starts_with(id))
    };
    // A backend is visible just before its bookkeeping transaction commits.
    // Require it to remain unowned for a full grace window so the orphan sweep
    // cannot destroy an in-flight shared/A&D container between create and insert.
    let now = std::time::Instant::now();
    let managed_set: std::collections::HashSet<&str> = managed.iter().map(String::as_str).collect();
    let mut ready = Vec::new();
    {
        let mut first_seen = ORPHAN_FIRST_SEEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        first_seen.retain(|id, _| managed_set.contains(id.as_str()) && !is_known(id));
        for id in &managed {
            if is_known(id) {
                continue;
            }
            let seen = first_seen.entry(id.clone()).or_insert(now);
            if now.duration_since(*seen) >= StdDuration::from_secs(ORPHAN_GRACE_SECS) {
                ready.push(id.clone());
            }
        }
    }
    let mut swept = 0u64;
    for id in ready {
        if let Err(error) =
            crate::services::ad_vpn::deactivate_backend_endpoint(&state.db, &id).await
        {
            tracing::warn!(backend_id = %id, %error, "cron: orphan endpoint revocation failed");
            continue;
        }
        if let Err(e) = state.containers.destroy(&id).await {
            tracing::warn!(backend_id = %id, "cron: orphan destroy failed: {e}");
        } else {
            ORPHAN_FIRST_SEEN
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            swept += 1;
        }
    }
    Ok(swept)
}

// ─────────────────────────────────────────────────────────────────────────────
// Job 2 — scoreboard cache maintenance
//   (RSCTF `CacheHelper.FlushScoreboardCache` / `FlushRecentGamesCache`).
// ─────────────────────────────────────────────────────────────────────────────

/// Evict stale scoreboard cache so it recomputes on next read. Drops the global
/// recent-games list plus the LIVE scoreboard keys for games that ended within
/// the last [`RECENT_ENDED_HOURS`] hours.
///
/// Only the live boards are evicted, never the frozen variants — matching
/// RSCTF's `FlushAdScoreboardCache`, which leaves the frozen boards to rebuild
/// lazily on read. In-game boards are kept fresh by the event-driven flushes in
/// the controllers, so this sweep deliberately targets only recently-ended games
/// (whose final standings no longer receive those events). Returns the number of
/// games whose keys were flushed.
async fn flush_stale_scoreboards(state: &SharedState) -> AppResult<u64> {
    let now = Utc::now();
    let cutoff = now - Duration::hours(RECENT_ENDED_HOURS);

    // Recent-games list (RSCTF `CacheKey.RecentGames`) — cheap, always refreshed.
    state.cache.remove("_RecentGames").await;

    let ended = game::Entity::find()
        .filter(game::Column::EndTimeUtc.lt(now))
        .filter(game::Column::EndTimeUtc.gte(cutoff))
        .all(&state.db)
        .await?;

    let mut flushed = 0u64;
    for g in &ended {
        for key in scoreboard_cache_keys(g.id) {
            state.cache.remove(&key).await;
        }
        flushed += 1;
    }

    Ok(flushed)
}

/// Scoreboard cache entries whose time-dependent view changes at event close.
fn scoreboard_cache_keys(game_id: i32) -> [String; 10] {
    [
        format!("_ScoreBoard_{game_id}"),
        format!("_ScoreBoardFrozen_{game_id}"),
        format!("_AdScoreBoard_{game_id}"),
        format!("_AdScoreBoard_{game_id}:stale"),
        format!("_AdScoreBoardFrozen_{game_id}"),
        format!("_AdScoreBoardFrozen_{game_id}:stale"),
        format!("_KothScoreBoard_{game_id}"),
        format!("_KothScoreBoardFrozen_{game_id}"),
        format!("_KothTimeline_{game_id}"),
        format!("_KothTimelineFrozen_{game_id}"),
    ]
}
