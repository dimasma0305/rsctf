//! Bounded container-row and runtime-orphan maintenance.

#[cfg(test)]
use std::collections::HashSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::StreamExt;
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::controllers::game::ManagedContainerCandidate;
use crate::utils::error::{AppError, AppResult};

use super::orphan_identity::load_runtime_ownership;
use super::orphan_tracking::{
    advance_inventory_cursor, inventory_cursor, OrphanSweepPolicy, ORPHAN_DESTROY_CONCURRENCY,
    ORPHAN_FIRST_SEEN,
};

#[cfg(test)]
use super::orphan_identity::{RuntimeOwnership, DOCKER_SHORT_ID_LEN};
#[cfg(test)]
use super::orphan_tracking::reset_scan_cursor;

const EXPIRED_REAP_BATCH: usize = 64;
const EXPIRED_REAP_CONCURRENCY: usize = 4;
const EXPIRED_REAP_BUDGET: Duration = Duration::from_secs(18);
const EXPIRED_REAP_CLAIM_LEASE: Duration = Duration::from_secs(45);
const EXPIRED_REAP_RETRY_DELAY: Duration = Duration::from_secs(30);
const CLAIM_RELEASE_BUDGET: Duration = Duration::from_secs(2);
const BACKLOG_SAMPLE_CAP: usize = 1_024;

const MAX_TRACKED_ORPHANS: usize = 8_192;
const ORPHAN_TRACKING_RETENTION: Duration = Duration::from_secs(60 * 60);

const CLAIM_EXPIRED_SQL: &str = r#"
WITH candidate AS (
    SELECT id
      FROM "Containers"
     WHERE expect_stop_at < $1
       AND (reap_after IS NULL OR reap_after <= $1)
     ORDER BY expect_stop_at, id
     LIMIT $2
     FOR UPDATE SKIP LOCKED
)
UPDATE "Containers" container
   SET reap_claim_token = $3,
       reap_after = $4
  FROM candidate
 WHERE container.id = candidate.id
RETURNING container.id,
          container.container_id AS backend_id,
          container.game_instance_id,
          container.exercise_instance_id
"#;

const BACKLOG_SQL: &str = r#"
SELECT COUNT(*)::bigint
  FROM (
        SELECT id
          FROM "Containers"
         WHERE expect_stop_at < $1
           AND (reap_after IS NULL OR reap_after <= $1)
         ORDER BY expect_stop_at, id
         LIMIT $2
       ) backlog
"#;

#[derive(Clone, Copy)]
struct ExpiredReapPolicy {
    batch: usize,
    concurrency: usize,
    budget: Duration,
    claim_lease: Duration,
    retry_delay: Duration,
}

impl Default for ExpiredReapPolicy {
    fn default() -> Self {
        Self {
            batch: EXPIRED_REAP_BATCH,
            concurrency: EXPIRED_REAP_CONCURRENCY,
            budget: EXPIRED_REAP_BUDGET,
            claim_lease: EXPIRED_REAP_CLAIM_LEASE,
            retry_delay: EXPIRED_REAP_RETRY_DELAY,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct MaintenanceReport {
    pub scanned: u64,
    pub claimed: u64,
    pub destroyed: u64,
    pub deferred: u64,
    pub failed: u64,
    pub backlog: u64,
    pub backlog_capped: bool,
    pub deadline_reached: bool,
    pub duration_ms: u64,
}

struct ExpiredClaim {
    token: Uuid,
    candidates: Vec<ManagedContainerCandidate>,
    backlog: u64,
    backlog_capped: bool,
}

#[derive(Clone, Copy)]
enum ReapOutcome {
    Destroyed,
    Refreshed,
    Failed,
    Deferred,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

fn chrono_after(now: DateTime<Utc>, duration: Duration) -> AppResult<DateTime<Utc>> {
    let duration = ChronoDuration::from_std(duration)
        .map_err(|error| AppError::internal(error.to_string()))?;
    now.checked_add_signed(duration)
        .ok_or_else(|| AppError::internal("container maintenance deadline overflow"))
}

async fn claim_expired_containers(
    pool: &sqlx::PgPool,
    now: DateTime<Utc>,
    policy: ExpiredReapPolicy,
) -> AppResult<ExpiredClaim> {
    let token = Uuid::new_v4();
    let lease_until = chrono_after(now, policy.claim_lease)?;
    let limit = i64::try_from(policy.batch.clamp(1, 256)).unwrap_or(256);
    let candidates = sqlx::query_as::<_, ManagedContainerCandidate>(CLAIM_EXPIRED_SQL)
        .bind(now)
        .bind(limit)
        .bind(token)
        .bind(lease_until)
        .fetch_all(pool)
        .await
        .map_err(database_error)?;

    let backlog_limit = i64::try_from(BACKLOG_SAMPLE_CAP + 1).unwrap_or(i64::MAX);
    let sampled: i64 = sqlx::query_scalar(BACKLOG_SQL)
        .bind(now)
        .bind(backlog_limit)
        .fetch_one(pool)
        .await
        .map_err(database_error)?;
    let backlog_capped = sampled > i64::try_from(BACKLOG_SAMPLE_CAP).unwrap_or(i64::MAX);
    let backlog = sampled
        .min(i64::try_from(BACKLOG_SAMPLE_CAP).unwrap_or(i64::MAX))
        .max(0) as u64;
    Ok(ExpiredClaim {
        token,
        candidates,
        backlog,
        backlog_capped,
    })
}

async fn release_expired_claim(
    pool: &sqlx::PgPool,
    container_id: Uuid,
    token: Uuid,
    retry_at: Option<DateTime<Utc>>,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "Containers"
              SET reap_claim_token = NULL, reap_after = $3
            WHERE id = $1 AND reap_claim_token = $2"#,
    )
    .bind(container_id)
    .bind(token)
    .bind(retry_at)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

async fn release_expired_claim_bounded(
    pool: &sqlx::PgPool,
    container_id: Uuid,
    token: Uuid,
    retry_at: Option<DateTime<Utc>>,
    pass_deadline: tokio::time::Instant,
) {
    let now = tokio::time::Instant::now();
    if now >= pass_deadline {
        return;
    }
    let release_deadline = pass_deadline.min(now + CLAIM_RELEASE_BUDGET);
    match tokio::time::timeout_at(
        release_deadline,
        release_expired_claim(pool, container_id, token, retry_at),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(
            container = %container_id,
            %error,
            "cron: could not release expired-container claim; lease will recover it"
        ),
        Err(_) => tracing::warn!(
            container = %container_id,
            "cron: expired-container claim release timed out; lease will recover it"
        ),
    }
}

async fn reap_one_expired(
    state: SharedState,
    candidate: ManagedContainerCandidate,
    token: Uuid,
    deadline: tokio::time::Instant,
    retry_delay: Duration,
) -> ReapOutcome {
    if tokio::time::Instant::now() >= deadline {
        // The durable lease is the cancellation path: avoid turning a timed-out
        // pass into one release query per queued candidate.
        return ReapOutcome::Deferred;
    }
    let result = tokio::time::timeout_at(
        deadline,
        crate::controllers::game::destroy_managed_container_candidate(&state, &candidate, true),
    )
    .await;
    match result {
        Ok(Ok(true)) => ReapOutcome::Destroyed,
        Ok(Ok(false)) => {
            release_expired_claim_bounded(state.pg(), candidate.id, token, None, deadline).await;
            ReapOutcome::Refreshed
        }
        Ok(Err(error)) => {
            let retry_at = chrono_after(Utc::now(), retry_delay).ok();
            release_expired_claim_bounded(state.pg(), candidate.id, token, retry_at, deadline)
                .await;
            tracing::warn!(
                container = %candidate.id,
                backend_id = %candidate.backend_id,
                %error,
                "cron: expired-container teardown failed; retry deferred"
            );
            ReapOutcome::Failed
        }
        Err(_) => {
            tracing::warn!(
                container = %candidate.id,
                backend_id = %candidate.backend_id,
                "cron: expired-container teardown reached the pass deadline; lease will recover it"
            );
            ReapOutcome::Deferred
        }
    }
}

pub(super) async fn reap_expired_containers(state: &SharedState) -> AppResult<MaintenanceReport> {
    reap_expired_containers_with(state, ExpiredReapPolicy::default()).await
}

async fn reap_expired_containers_with(
    state: &SharedState,
    policy: ExpiredReapPolicy,
) -> AppResult<MaintenanceReport> {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + policy.budget;
    let claim = match tokio::time::timeout_at(
        deadline,
        claim_expired_containers(state.pg(), Utc::now(), policy),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Ok(MaintenanceReport {
                deadline_reached: true,
                duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                ..Default::default()
            });
        }
    };
    let claimed = claim.candidates.len() as u64;
    let outcomes = futures::stream::iter(claim.candidates)
        .map(|candidate| {
            reap_one_expired(
                state.clone(),
                candidate,
                claim.token,
                deadline,
                policy.retry_delay,
            )
        })
        .buffer_unordered(policy.concurrency.clamp(1, EXPIRED_REAP_CONCURRENCY))
        .collect::<Vec<_>>()
        .await;

    let mut report = MaintenanceReport {
        scanned: claimed.saturating_add(claim.backlog),
        claimed,
        backlog: claim.backlog,
        backlog_capped: claim.backlog_capped,
        ..Default::default()
    };
    for outcome in outcomes {
        match outcome {
            ReapOutcome::Destroyed => report.destroyed += 1,
            ReapOutcome::Refreshed => {}
            ReapOutcome::Failed => {
                report.failed += 1;
                report.backlog += 1;
            }
            ReapOutcome::Deferred => {
                report.deferred += 1;
                report.backlog += 1;
                report.deadline_reached = true;
            }
        }
    }
    report.duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Ok(report)
}

async fn destroy_orphan(state: SharedState, id: String) -> AppResult<()> {
    crate::services::ad_vpn::deactivate_backend_endpoint(&state.db, &id).await?;
    state.containers.destroy(&id).await
}

pub(super) async fn sweep_orphan_containers(state: &SharedState) -> AppResult<MaintenanceReport> {
    sweep_orphan_containers_with(state, OrphanSweepPolicy::default()).await
}

async fn sweep_orphan_containers_with(
    state: &SharedState,
    policy: OrphanSweepPolicy,
) -> AppResult<MaintenanceReport> {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + policy.budget;
    let cursor = inventory_cursor();
    let page = match tokio::time::timeout_at(
        deadline,
        state
            .containers
            .list_managed_page(cursor.as_deref(), policy.scan_batch),
    )
    .await
    {
        Ok(page) => page,
        Err(_) => {
            return Ok(MaintenanceReport {
                deadline_reached: true,
                duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                ..Default::default()
            });
        }
    };
    let has_more = page.next_cursor.is_some();
    let scanned = page.ids;
    advance_inventory_cursor(page.next_cursor);
    let managed_count = scanned.len() + usize::from(has_more);
    if scanned.is_empty() {
        ORPHAN_FIRST_SEEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        return Ok(MaintenanceReport {
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            ..Default::default()
        });
    }
    let ownership =
        match tokio::time::timeout_at(deadline, load_runtime_ownership(state.pg(), &scanned)).await
        {
            Ok(result) => result?,
            Err(_) => {
                return Ok(MaintenanceReport {
                    scanned: scanned.len() as u64,
                    backlog: managed_count.saturating_sub(scanned.len()) as u64,
                    deadline_reached: true,
                    duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    ..Default::default()
                });
            }
        };

    let now = Instant::now();
    let mut ready = Vec::new();
    let mut grace_pending = 0_u64;
    let mut untracked = 0_u64;
    {
        let mut first_seen = ORPHAN_FIRST_SEEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A disappeared runtime is no longer present in a bounded inventory
        // page, so age stale observations out without needing a full inventory
        // HashSet. This also prevents failed/vanished IDs from permanently
        // consuming the fixed tracking capacity.
        first_seen.retain(|_, seen| now.duration_since(*seen) <= ORPHAN_TRACKING_RETENTION);
        for id in &scanned {
            if ownership.contains(id) {
                first_seen.remove(id);
                continue;
            }
            if !first_seen.contains_key(id) && first_seen.len() >= MAX_TRACKED_ORPHANS {
                untracked += 1;
                continue;
            }
            let seen = first_seen.entry(id.clone()).or_insert(now);
            if now.duration_since(*seen) >= policy.grace {
                ready.push(id.clone());
            } else {
                grace_pending += 1;
            }
        }
    }

    // A second ownership snapshot closes the ordinary create-then-persist
    // window immediately before destructive work. The grace period remains the
    // fail-safe for a transaction that was in flight during both snapshots.
    if !ready.is_empty() {
        let refreshed =
            match tokio::time::timeout_at(deadline, load_runtime_ownership(state.pg(), &ready))
                .await
            {
                Ok(result) => result?,
                Err(_) => {
                    return Ok(MaintenanceReport {
                        scanned: scanned.len() as u64,
                        backlog: managed_count.saturating_sub(scanned.len()) as u64
                            + ready.len() as u64
                            + grace_pending
                            + untracked,
                        deadline_reached: true,
                        duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                        ..Default::default()
                    });
                }
            };
        ready.retain(|id| {
            let owned = refreshed.contains(id);
            if owned {
                ORPHAN_FIRST_SEEN
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(id);
            }
            !owned
        });
    }

    let ready_backlog = ready.len().saturating_sub(policy.destroy_batch) as u64;
    ready.truncate(policy.destroy_batch);
    let claimed = ready.len() as u64;
    let outcomes = futures::stream::iter(ready)
        .map(|id| {
            let state = state.clone();
            async move {
                if tokio::time::Instant::now() >= deadline {
                    return (id, ReapOutcome::Deferred);
                }
                match tokio::time::timeout_at(deadline, destroy_orphan(state, id.clone())).await {
                    Ok(Ok(())) => (id, ReapOutcome::Destroyed),
                    Ok(Err(error)) => {
                        tracing::warn!(backend_id = %id, %error, "cron: orphan destroy failed");
                        (id, ReapOutcome::Failed)
                    }
                    Err(_) => (id, ReapOutcome::Deferred),
                }
            }
        })
        .buffer_unordered(policy.concurrency.clamp(1, ORPHAN_DESTROY_CONCURRENCY))
        .collect::<Vec<_>>()
        .await;

    let mut report = MaintenanceReport {
        scanned: scanned.len() as u64,
        claimed,
        backlog: managed_count.saturating_sub(scanned.len()) as u64
            + ready_backlog
            + grace_pending
            + untracked,
        backlog_capped: untracked > 0,
        ..Default::default()
    };
    for (id, outcome) in outcomes {
        match outcome {
            ReapOutcome::Destroyed => {
                ORPHAN_FIRST_SEEN
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&id);
                report.destroyed += 1;
            }
            ReapOutcome::Failed => {
                report.failed += 1;
                report.backlog += 1;
            }
            ReapOutcome::Deferred => {
                report.deferred += 1;
                report.backlog += 1;
                report.deadline_reached = true;
            }
            ReapOutcome::Refreshed => {}
        }
    }
    report.duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use sea_orm::SqlxPostgresConnector;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;
    use crate::services::container::{
        ContainerInfo, ContainerManager, ContainerSpec, ContainerStatus,
    };

    struct PgHarness {
        admin: sqlx::PgPool,
        pool: sqlx::PgPool,
        schema: String,
    }

    impl PgHarness {
        async fn new() -> Self {
            let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
                .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
            let admin = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await
                .unwrap();
            let schema = format!("container_reaper_{}", Uuid::new_v4().simple());
            sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
                .execute(&admin)
                .await
                .unwrap();
            let options = PgConnectOptions::from_str(&database_url)
                .unwrap()
                .options([("search_path", schema.as_str())]);
            let pool = PgPoolOptions::new()
                .max_connections(16)
                .connect_with(options)
                .await
                .unwrap();
            sqlx::raw_sql(
                r#"
                CREATE TABLE "Containers" (
                  id UUID PRIMARY KEY,
                  image TEXT NOT NULL DEFAULT 'image',
                  container_id TEXT NOT NULL,
                  status SMALLINT NOT NULL DEFAULT 0,
                  started_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                  expect_stop_at TIMESTAMPTZ NOT NULL,
                  is_proxy BOOLEAN NOT NULL DEFAULT FALSE,
                  ip TEXT NOT NULL DEFAULT '',
                  port INTEGER NOT NULL DEFAULT 0,
                  public_ip TEXT,
                  public_port INTEGER,
                  game_instance_id INTEGER,
                  exercise_instance_id INTEGER,
                  ad_team_service_id INTEGER,
                  reap_claim_token UUID,
                  reap_after TIMESTAMPTZ
                );
                CREATE INDEX ix_containers_expired_reap
                  ON "Containers" (expect_stop_at, id) INCLUDE (reap_after);
                CREATE TABLE "GameChallenges" (
                  id INTEGER PRIMARY KEY,
                  game_id INTEGER NOT NULL,
                  shared_container_id UUID,
                  test_container_id UUID,
                  enable_traffic_capture BOOLEAN NOT NULL DEFAULT FALSE,
                  ad_self_hosted BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE TABLE "GameInstances" (
                  id INTEGER PRIMARY KEY,
                  participation_id INTEGER NOT NULL,
                  container_id UUID,
                  is_loaded BOOLEAN NOT NULL DEFAULT FALSE,
                  last_container_operation TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
                );
                CREATE TABLE "ExerciseInstances" (
                  id INTEGER PRIMARY KEY,
                  exercise_id INTEGER NOT NULL,
                  user_id UUID NOT NULL,
                  container_id UUID,
                  is_loaded BOOLEAN NOT NULL DEFAULT FALSE,
                  last_container_operation TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
                );
                CREATE TABLE "AdTeamServices" (
                  id INTEGER PRIMARY KEY,
                  game_id INTEGER NOT NULL DEFAULT 1,
                  challenge_id INTEGER NOT NULL DEFAULT 1,
                  container_id TEXT,
                  host TEXT NOT NULL DEFAULT '',
                  port INTEGER NOT NULL DEFAULT 0,
                  status SMALLINT NOT NULL DEFAULT 2
                );
                CREATE TABLE "KothTargets" (
                  challenge_id INTEGER NOT NULL,
                  game_id INTEGER NOT NULL DEFAULT 1,
                  container_id TEXT,
                  host TEXT NOT NULL DEFAULT '',
                  port INTEGER NOT NULL DEFAULT 0,
                  holder_participation_id INTEGER,
                  held_since TIMESTAMPTZ
                );
                CREATE TABLE "KothCrownCycles" (
                  phase TEXT NOT NULL,
                  old_container_id TEXT,
                  replacement_container_id TEXT
                );
                "#,
            )
            .execute(&pool)
            .await
            .unwrap();
            Self {
                admin,
                pool,
                schema,
            }
        }

        async fn insert_expired(&self, count: usize) {
            sqlx::query(
                r#"INSERT INTO "Containers" (id, container_id, expect_stop_at)
                   SELECT gen_random_uuid(), 'runtime-' || value::text,
                          clock_timestamp() - interval '1 minute'
                     FROM generate_series(1, $1) value"#,
            )
            .bind(i32::try_from(count).unwrap())
            .execute(&self.pool)
            .await
            .unwrap();
        }

        async fn cleanup(self) {
            self.pool.close().await;
            sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
                .execute(&self.admin)
                .await
                .unwrap();
            self.admin.close().await;
        }
    }

    #[derive(Clone)]
    struct ControlledRuntime {
        delay: Duration,
        failures: Arc<HashSet<String>>,
        managed: Arc<Vec<String>>,
        attempts: Arc<Mutex<Vec<String>>>,
        destroyed: Arc<Mutex<Vec<String>>>,
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    struct InFlightGuard(Arc<AtomicUsize>);

    impl Drop for InFlightGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl ControlledRuntime {
        fn new(delay: Duration, failures: impl IntoIterator<Item = String>) -> Self {
            Self {
                delay,
                failures: Arc::new(failures.into_iter().collect()),
                managed: Default::default(),
                attempts: Default::default(),
                destroyed: Default::default(),
                in_flight: Default::default(),
                max_in_flight: Default::default(),
            }
        }

        fn max_in_flight(&self) -> usize {
            self.max_in_flight.load(Ordering::SeqCst)
        }

        fn with_managed(mut self, managed: Vec<String>) -> Self {
            self.managed = Arc::new(managed);
            self
        }
    }

    #[async_trait]
    impl ContainerManager for ControlledRuntime {
        async fn create(&self, _spec: ContainerSpec) -> AppResult<ContainerInfo> {
            Err(AppError::bad_request("not used by the reaper test"))
        }

        async fn destroy(&self, id: &str) -> AppResult<()> {
            self.attempts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(id.to_string());
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(current, Ordering::SeqCst);
            let _guard = InFlightGuard(self.in_flight.clone());
            tokio::time::sleep(self.delay).await;
            if self.failures.contains(id) {
                return Err(AppError::unavailable("injected runtime failure"));
            }
            self.destroyed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(id.to_string());
            Ok(())
        }

        async fn query(&self, _id: &str) -> AppResult<ContainerStatus> {
            Err(AppError::bad_request("not used by the reaper test"))
        }

        async fn list_managed(&self) -> Vec<String> {
            self.managed.as_ref().clone()
        }

        async fn list_managed_page(
            &self,
            _cursor: Option<&str>,
            limit: usize,
        ) -> crate::services::container::ManagedContainerPage {
            let mut ids = self.managed.as_ref().clone();
            ids.truncate(limit);
            crate::services::container::ManagedContainerPage {
                ids,
                next_cursor: None,
            }
        }
    }

    fn test_state(pool: &sqlx::PgPool, runtime: Arc<dyn ContainerManager>) -> SharedState {
        crate::app_state::AppState::new(
            SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone()),
            Arc::new(crate::models::internal::configs::AppConfig::default()),
            Arc::new(crate::services::cache::InMemoryCache::new()),
            Arc::new(crate::storage::LocalBlobStorage::new(
                std::env::temp_dir().join("rsctf-container-reaper-tests"),
            )),
            crate::services::token::TokenService::new("0123456789abcdef0123456789abcdef", 60),
            runtime,
        )
    }

    #[test]
    fn ownership_lookup_normalizes_docker_ids_but_keeps_names_exact() {
        let ownership = RuntimeOwnership::from_ids([
            "ABCDEF1234567890".to_string(),
            "rsctf-koth-cycle-17".to_string(),
        ]);
        assert!(ownership.contains("abcdef123456"));
        assert!(ownership.contains("abcdef1234567890ffff"));
        // A suffix conflict sharing the daemon-safe short identity is retained,
        // never swept as an orphan. This is the conservative collision case.
        assert!(ownership.contains("abcdef1234560000"));
        assert!(!ownership.contains("fedcba123456"));
        assert!(ownership.contains("rsctf-koth-cycle-17"));
        assert!(!ownership.contains("rsctf-koth-cycle"));
        assert!(!ownership.contains("rsctf-koth-cycle-17-extra"));
    }

    fn reset_orphan_test_state() {
        ORPHAN_FIRST_SEEN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        reset_scan_cursor();
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn concurrent_claims_are_disjoint_bounded_and_report_a_capped_backlog() {
        let harness = PgHarness::new().await;
        harness.insert_expired(2_000).await;
        let policy = ExpiredReapPolicy {
            batch: 64,
            ..ExpiredReapPolicy::default()
        };
        let now = Utc::now();
        let (first, second) = tokio::join!(
            claim_expired_containers(&harness.pool, now, policy),
            claim_expired_containers(&harness.pool, now, policy),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.candidates.len(), 64);
        assert_eq!(second.candidates.len(), 64);
        let first_ids = first
            .candidates
            .iter()
            .map(|candidate| candidate.id)
            .collect::<HashSet<_>>();
        assert!(second
            .candidates
            .iter()
            .all(|candidate| !first_ids.contains(&candidate.id)));
        assert!(first.backlog_capped || second.backlog_capped);
        assert_eq!(first.backlog.max(second.backlog), BACKLOG_SAMPLE_CAP as u64);

        let claimed_rows: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "Containers" WHERE reap_claim_token IS NOT NULL"#,
        )
        .fetch_one(&harness.pool)
        .await
        .unwrap();
        assert_eq!(claimed_rows, 128);
        harness.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn runtime_work_is_concurrent_but_bounded_while_fixed_rate_pg_reads_progress() {
        let harness = PgHarness::new().await;
        harness.insert_expired(12).await;
        let runtime = Arc::new(ControlledRuntime::new(
            Duration::from_millis(40),
            ["runtime-3".to_string(), "runtime-9".to_string()],
        ));
        let state = test_state(&harness.pool, runtime.clone());
        let policy = ExpiredReapPolicy {
            batch: 12,
            concurrency: 3,
            budget: Duration::from_secs(3),
            claim_lease: Duration::from_secs(2),
            retry_delay: Duration::from_millis(100),
        };
        let reaper_state = state.clone();
        let reaper = tokio::spawn(async move {
            reap_expired_containers_with(&reaper_state, policy)
                .await
                .unwrap()
        });

        let cadence = Duration::from_millis(10);
        let read_started = tokio::time::Instant::now();
        for tick in 0..20_u32 {
            tokio::time::sleep_until(read_started + cadence * tick).await;
            let value = tokio::time::timeout(
                Duration::from_millis(100),
                sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&harness.pool),
            )
            .await
            .expect("fixed-rate database probe remained responsive")
            .unwrap();
            assert_eq!(value, 1);
        }
        let report = reaper.await.unwrap();
        assert_eq!(report.claimed, 12);
        assert_eq!(report.destroyed, 10);
        assert_eq!(report.failed, 2);
        assert!(runtime.max_in_flight() > 1);
        assert!(runtime.max_in_flight() <= 3);
        let remaining: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Containers""#)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
        assert_eq!(
            remaining, 2,
            "only failed runtime destroys remain retryable"
        );
        harness.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn pass_deadline_cancels_runtime_backlog_and_leaves_rows_retryable() {
        let harness = PgHarness::new().await;
        harness.insert_expired(8).await;
        let runtime = Arc::new(ControlledRuntime::new(
            Duration::from_secs(5),
            std::iter::empty(),
        ));
        let state = test_state(&harness.pool, runtime.clone());
        let started = Instant::now();
        let policy = ExpiredReapPolicy {
            batch: 8,
            concurrency: 2,
            budget: Duration::from_millis(75),
            claim_lease: Duration::from_millis(200),
            retry_delay: Duration::from_millis(100),
        };
        let report = reap_expired_containers_with(&state, policy).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(report.deadline_reached);
        assert_eq!(report.destroyed, 0);
        assert_eq!(report.deferred, 8);
        assert!(runtime.max_in_flight() <= 2);
        let remaining: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "Containers""#)
            .fetch_one(&harness.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 8);
        let leased: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "Containers" WHERE reap_claim_token IS NOT NULL"#,
        )
        .fetch_one(&harness.pool)
        .await
        .unwrap();
        assert_eq!(
            leased, 8,
            "deadline cleanup uses the one durable batch lease"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        let reclaimed = claim_expired_containers(&harness.pool, Utc::now(), policy)
            .await
            .unwrap();
        assert_eq!(
            reclaimed.candidates.len(),
            8,
            "expired leases are retryable"
        );
        harness.cleanup().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn orphan_sweep_protects_short_id_owners_and_create_persistence_window() {
        reset_orphan_test_state();
        let harness = PgHarness::new().await;
        let full_owned = format!("abcdef123456{}", "7".repeat(52));
        sqlx::query(
            r#"INSERT INTO "Containers" (id, container_id, expect_stop_at)
               VALUES (gen_random_uuid(), $1, clock_timestamp() + interval '1 hour')"#,
        )
        .bind(&full_owned[..DOCKER_SHORT_ID_LEN])
        .execute(&harness.pool)
        .await
        .unwrap();
        let runtime = Arc::new(
            ControlledRuntime::new(Duration::from_millis(5), std::iter::empty()).with_managed(
                vec![
                    full_owned.clone(),
                    "late-persisted-runtime".to_string(),
                    "true-orphan-runtime".to_string(),
                ],
            ),
        );
        let state = test_state(&harness.pool, runtime.clone());
        let policy = OrphanSweepPolicy {
            scan_batch: 16,
            destroy_batch: 16,
            concurrency: 2,
            grace: Duration::from_millis(30),
            budget: Duration::from_secs(2),
        };

        let first = sweep_orphan_containers_with(&state, policy).await.unwrap();
        assert_eq!(first.destroyed, 0, "new runtimes receive the safety grace");
        sqlx::query(
            r#"INSERT INTO "AdTeamServices"
                 (id, container_id, host, port, status)
               VALUES (7, 'late-persisted-runtime', '10.0.0.7', 31337, 0)"#,
        )
        .execute(&harness.pool)
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let second = sweep_orphan_containers_with(&state, policy).await.unwrap();
        assert_eq!(second.destroyed, 1);
        let attempts = runtime
            .attempts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(attempts, ["true-orphan-runtime"]);
        assert!(!attempts.contains(&full_owned));
        assert!(!attempts.contains(&"late-persisted-runtime".to_string()));
        harness.cleanup().await;
        reset_orphan_test_state();
    }
}
