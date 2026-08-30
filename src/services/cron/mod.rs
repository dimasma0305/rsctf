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
//!   * bounded expired/orphan container maintenance — destroy rows whose
//!     `expect_stop_at` has passed (mirrors `RuntimeCronJobs.ContainerChecker`
//!     + `ContainerRepository.DestroyContainer`).
//!   * refresh the time-relative recent-games list. Final scoreboard caches are
//!     immutable long-lived entries and are not churned by maintenance.
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

use crate::app_state::SharedState;
use crate::utils::enums::{ChallengeReviewStatus, ChallengeType};
use crate::utils::error::AppResult;
use chrono::Utc;

mod backend_reaper;
mod delivery_health;
mod maintenance;
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

/// Hard cap on ONE game's advance (finalize + open + plant flags + run the checker +
/// KotH). A game with many hung/offline services can make its checker pass take
/// minutes; this stops it blocking every other game, the reaper, and the next tick (#5).
pub(super) const ADVANCE_BUDGET_SECS: u64 = 240;

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
        let poll_seconds = crate::services::ad_engine::ROUND_SCHEDULER_POLL_SECONDS;
        let mut ticker = tokio::time::interval(StdDuration::from_secs(poll_seconds));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tracing::info!(
            ?scope,
            "cron: A&D round supervisor started (tick {poll_seconds}s)"
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
    // Slow operator work is durable in PostgreSQL. Wake a bounded lease owner;
    // this returns immediately and also recovers jobs whose previous owner died.
    crate::services::control_jobs::kick(state.clone());

    match crate::services::control_jobs::purge_terminal(state.pg(), 256).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: purged retained control-plane job(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: control-plane job retention sweep failed: {e}"),
    }

    match crate::services::traffic::purge_expired_captures(state, 128).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: purged expired traffic capture tree(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: traffic capture retention sweep failed: {e}"),
    }

    match crate::services::blob_refs::purge_expired_service_snapshots(
        state.pg(),
        state.storage.as_ref(),
        128,
    )
    .await
    {
        Ok(n) if n > 0 => tracing::info!(n, "cron: purged expired A&D service snapshot(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: A&D snapshot retention sweep failed: {e}"),
    }

    match crate::services::blob_refs::purge_expired_stages(state.pg(), state.storage.as_ref(), 128)
        .await
    {
        Ok(n) if n > 0 => tracing::info!(n, "cron: reclaimed expired blob stage(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: blob staging sweep failed: {e}"),
    }

    match crate::services::blob_refs::purge_pending(state.pg(), state.storage.as_ref(), 128).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: purged deferred blob tombstone(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: deferred blob purge failed: {e}"),
    }

    match crate::controllers::edit::process_configuration_effects(state).await {
        Ok(n) if n > 0 => tracing::debug!(n, "cron: applied game configuration effect(s)"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "cron: game configuration effects failed"),
    }

    match crate::controllers::team::process_profile_invalidations(state).await {
        Ok(n) if n > 0 => tracing::debug!(n, "cron: applied team profile invalidation(s)"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "cron: team profile invalidations failed"),
    }

    match crate::services::blob_refs::purge_expired_stages(state.pg(), state.storage.as_ref(), 128)
        .await
    {
        Ok(n) if n > 0 => tracing::info!(n, "cron: reclaimed expired blob stage(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: blob staging sweep failed: {e}"),
    }

    match crate::controllers::game::purge_terminal_operations(state.pg(), 128).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: purged terminal player container operation(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: player container operation sweep failed: {e}"),
    }

    match crate::services::git_sync::collect_stale_checker_revisions(state).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: collected stale checker revision(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: checker revision GC failed: {e}"),
    }

    match crate::services::image_storage::scheduled_cleanup(state).await {
        Ok(Some(report)) if report.images_removed > 0 || report.cache_bytes_reclaimed > 0 => {
            tracing::info!(
                images = report.images_removed,
                image_bytes = report.image_bytes_evicted,
                cache_bytes = report.cache_bytes_reclaimed,
                dangling_bytes = report.dangling_bytes_reclaimed,
                free_before = report.available_bytes_before,
                free_after = report.available_bytes_after,
                pressure = report.pressure_mode,
                "cron: completed bounded Docker storage cleanup"
            );
            for message in report.messages {
                tracing::warn!(%message, "cron: Docker storage cleanup note");
            }
        }
        Ok(Some(report)) => {
            for message in report.messages {
                tracing::warn!(%message, "cron: Docker storage cleanup note");
            }
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(%error, "cron: Docker storage cleanup failed"),
    }

    match maintenance::reap_expired_containers(state).await {
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

    match backend_reaper::reap_ended_backends(state).await {
        Ok(n) if n > 0 => tracing::info!("cron: reaped {n} ended-game A&D backend(s)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: ended-game A&D teardown failed: {e}"),
    }

    match crate::controllers::edit::recover_accepted_provisioning(state).await {
        Ok(n) if n > 0 => tracing::info!(n, "cron: recovered accepted-participation resources"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "cron: accepted-participation recovery failed"),
    }

    match maintenance::sweep_orphan_containers(state).await {
        Ok(n) if n > 0 => tracing::info!("cron: swept {n} orphan container(s) (no DB row)"),
        Ok(_) => {}
        Err(e) => tracing::warn!("cron: orphan sweep failed: {e}"),
    }

    refresh_recent_games_cache(state).await;

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
            .remove(&format!("_KothScoreBoardWireV2_{game_id}"))
            .await;
        state
            .cache
            .remove(&format!("_KothScoreBoardFrozen_{game_id}"))
            .await;
        state
            .cache
            .remove(&format!("_KothScoreBoardWireV2Frozen_{game_id}"))
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

// ─────────────────────────────────────────────────────────────────────────────
// Job 2 — discovery cache maintenance.
// ─────────────────────────────────────────────────────────────────────────────

/// Event discovery is time-relative, so its legacy cache remains short-lived.
/// Final scoreboards are immutable seven-day entries after close and must not be
/// deleted on every maintenance tick; mutation paths retain explicit repair
/// invalidation for exceptional corrections.
async fn refresh_recent_games_cache(state: &SharedState) {
    state.cache.remove("_RecentGames").await;
}
