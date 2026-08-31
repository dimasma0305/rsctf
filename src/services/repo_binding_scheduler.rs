//! Durable repository scan scheduling and lease recovery.
//!
//! PostgreSQL time and row leases are authoritative. Every control replica may
//! poll, while `SKIP LOCKED`, the lease token, the checkout fence, and the
//! upstream-host advisory lock keep work bounded and non-duplicating.

use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::app_state::SharedState;
use crate::controllers::admin::{RepoBindingScanExecution, RepoBindingScanResultModel};
use crate::utils::error::{AppError, AppResult};

const SCAN_BATCH: i64 = 4;
const MANUAL_SCAN_LIMIT: usize = 4;
const SCAN_LEASE_SECONDS: i32 = 300;
const SCAN_TICK_SECONDS: u64 = 5;
const HEARTBEAT_TIMEOUT_SECONDS: u64 = 10;
const SHUTDOWN_DRAIN_SECONDS: u64 = 30;
const HISTORY_RETENTION: i64 = 200;
const HISTORY_PURGE_BATCH: i64 = 64;
static MANUAL_SCAN_GATE: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| {
        std::sync::Arc::new(tokio::sync::Semaphore::new(MANUAL_SCAN_LIMIT))
    });

const CLAIM_DUE_SQL: &str = r#"
WITH candidates AS MATERIALIZED (
    SELECT binding.id, binding.repo_url, binding.next_scan_utc
      FROM "RepoBindings" binding
     WHERE binding.status = 0
       AND binding.next_scan_utc <= clock_timestamp()
       AND (binding.scan_lease_expires_at_utc IS NULL
            OR binding.scan_lease_expires_at_utc <= clock_timestamp())
     ORDER BY binding.next_scan_utc, binding.id
     LIMIT 64
), ranked AS MATERIALIZED (
    SELECT candidate.id, candidate.next_scan_utc,
           row_number() OVER (
               PARTITION BY lower(split_part(split_part(candidate.repo_url, '://', 2), '/', 1))
               ORDER BY candidate.next_scan_utc, candidate.id
           ) AS host_rank
      FROM candidates candidate
     WHERE NOT EXISTS (
             SELECT 1
               FROM "RepoBindings" active
              WHERE active.scan_lease_expires_at_utc > clock_timestamp()
                AND lower(split_part(split_part(active.repo_url, '://', 2), '/', 1))
                    = lower(split_part(split_part(candidate.repo_url, '://', 2), '/', 1))
       )
), due AS MATERIALIZED (
    SELECT binding.id
      FROM "RepoBindings" binding
      JOIN ranked ON ranked.id = binding.id AND ranked.host_rank = 1
     ORDER BY ranked.next_scan_utc, binding.id
     LIMIT $1
     FOR UPDATE OF binding SKIP LOCKED
)
UPDATE "RepoBindings" binding
   SET scan_lease_owner = $2,
       scan_lease_expires_at_utc = clock_timestamp() + make_interval(secs => $3),
       scan_attempt = LEAST(9007199254740991, binding.scan_attempt + 1),
       current_activity = 'Scanning repository'
  FROM due
 WHERE binding.id = due.id
RETURNING binding.id, binding.repo_url, binding.scan_attempt, clock_timestamp()
"#;

#[derive(Clone, Debug)]
pub(crate) struct RepoScanClaim {
    pub id: i32,
    pub repo_url: String,
    pub owner: Uuid,
    pub attempt: i64,
    pub claimed_at: DateTime<Utc>,
}

fn database_error(error: sqlx::Error) -> AppError {
    AppError::internal(error.to_string())
}

pub(crate) fn validate_interval(interval: i32) -> AppResult<i32> {
    if !(60..=86_400).contains(&interval) {
        return Err(AppError::bad_request(
            "intervalSeconds must be between 60 and 86400",
        ));
    }
    Ok(interval)
}

fn claim_from_row(owner: Uuid, row: (i32, String, i64, DateTime<Utc>)) -> RepoScanClaim {
    RepoScanClaim {
        id: row.0,
        repo_url: row.1,
        attempt: row.2,
        claimed_at: row.3,
        owner,
    }
}

async fn claim_due(pool: &sqlx::PgPool, count: i64) -> AppResult<Vec<RepoScanClaim>> {
    let count = count.clamp(1, SCAN_BATCH);
    let owner = Uuid::new_v4();
    let rows = sqlx::query_as::<_, (i32, String, i64, DateTime<Utc>)>(CLAIM_DUE_SQL)
        .bind(count)
        .bind(owner)
        .bind(SCAN_LEASE_SECONDS)
        .fetch_all(pool)
        .await
        .map_err(database_error)?;
    Ok(rows
        .into_iter()
        .map(|row| claim_from_row(owner, row))
        .collect())
}

async fn claim_manual(pool: &sqlx::PgPool, id: i32) -> AppResult<RepoScanClaim> {
    let owner = Uuid::new_v4();
    let row = sqlx::query_as::<_, (i32, String, i64, DateTime<Utc>)>(
        r#"UPDATE "RepoBindings"
              SET scan_lease_owner = $2,
                  scan_lease_expires_at_utc = clock_timestamp()
                      + make_interval(secs => $3),
                  scan_attempt = LEAST(9007199254740991, scan_attempt + 1),
                  current_activity = 'Scanning repository (manual)'
            WHERE id = $1
              AND (scan_lease_expires_at_utc IS NULL
                   OR scan_lease_expires_at_utc <= clock_timestamp())
        RETURNING id, repo_url, scan_attempt, clock_timestamp()"#,
    )
    .bind(id)
    .bind(owner)
    .bind(SCAN_LEASE_SECONDS)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?;
    if let Some(row) = row {
        return Ok(claim_from_row(owner, row));
    }
    let exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM "RepoBindings" WHERE id = $1)"#,
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(database_error)?;
    if exists {
        Err(AppError::conflict(
            "Repository binding scan is already active",
        ))
    } else {
        Err(AppError::not_found("Repo binding not found"))
    }
}

async fn renew(pool: &sqlx::PgPool, claim: &RepoScanClaim) -> AppResult<bool> {
    let affected = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET scan_lease_expires_at_utc = clock_timestamp()
                  + make_interval(secs => $4)
            WHERE id = $1 AND scan_lease_owner = $2 AND scan_attempt = $3"#,
    )
    .bind(claim.id)
    .bind(claim.owner)
    .bind(claim.attempt)
    .bind(SCAN_LEASE_SECONDS)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(affected == 1)
}

pub(crate) async fn set_activity(
    pool: &sqlx::PgPool,
    claim: &RepoScanClaim,
    activity: &str,
) -> AppResult<bool> {
    let activity = activity.chars().take(240).collect::<String>();
    let affected = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET current_activity = $4
            WHERE id = $1 AND scan_lease_owner = $2 AND scan_attempt = $3"#,
    )
    .bind(claim.id)
    .bind(claim.owner)
    .bind(claim.attempt)
    .bind(activity)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(affected == 1)
}

async fn finish_success(
    pool: &sqlx::PgPool,
    claim: &RepoScanClaim,
    execution: &RepoBindingScanExecution,
) -> AppResult<bool> {
    let failed = execution.result.failures > 0;
    let affected = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET last_scan_utc = $4,
                  last_commit_sha = COALESCE($5, last_commit_sha),
                  last_scan_message = $6,
                  next_scan_utc = CASE WHEN $7
                       THEN clock_timestamp() + make_interval(secs => LEAST(
                            3600,
                            30 * power(2, LEAST(scan_failures + 1, 7))::integer
                                + ((id::bigint + scan_attempt) % 31)::integer
                       ))
                       ELSE clock_timestamp() + make_interval(secs => interval_seconds)
                  END,
                  scan_failures = CASE WHEN $7 THEN LEAST(1000000, scan_failures + 1) ELSE 0 END,
                  scan_lease_owner = NULL,
                  scan_lease_expires_at_utc = NULL,
                  current_activity = NULL
            WHERE id = $1 AND scan_lease_owner = $2 AND scan_attempt = $3"#,
    )
    .bind(claim.id)
    .bind(claim.owner)
    .bind(claim.attempt)
    .bind(execution.ran_at)
    .bind(&execution.commit_sha)
    .bind(&execution.message)
    .bind(failed)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(affected == 1)
}

async fn finish_failure(
    pool: &sqlx::PgPool,
    claim: &RepoScanClaim,
    error: &str,
) -> AppResult<bool> {
    let error = error.chars().take(2_000).collect::<String>();
    let affected = sqlx::query(
        r#"UPDATE "RepoBindings"
              SET last_scan_utc = clock_timestamp(),
                  last_scan_message = $4,
                  next_scan_utc = clock_timestamp() + make_interval(secs => LEAST(
                       3600,
                       30 * power(2, LEAST(scan_failures + 1, 7))::integer
                           + ((id::bigint + scan_attempt) % 31)::integer
                  )),
                  scan_failures = LEAST(1000000, scan_failures + 1),
                  scan_lease_owner = NULL,
                  scan_lease_expires_at_utc = NULL,
                  current_activity = NULL
            WHERE id = $1 AND scan_lease_owner = $2 AND scan_attempt = $3"#,
    )
    .bind(claim.id)
    .bind(claim.owner)
    .bind(claim.attempt)
    .bind(error)
    .execute(pool)
    .await
    .map_err(database_error)?
    .rows_affected();
    Ok(affected == 1)
}

async fn finish_failure_bounded(
    pool: &sqlx::PgPool,
    claim: &RepoScanClaim,
    error: &str,
) -> AppResult<bool> {
    tokio::time::timeout(
        Duration::from_secs(HEARTBEAT_TIMEOUT_SECONDS),
        finish_failure(pool, claim, error),
    )
    .await
    .map_err(|_| AppError::internal("repository scan failure finalization timed out"))?
}

pub(crate) async fn retain_history(pool: &sqlx::PgPool, binding_id: i32) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "RepoBindingScans"
            WHERE id IN (
                SELECT id FROM "RepoBindingScans"
                 WHERE binding_id = $1
                 ORDER BY id DESC
                 OFFSET $2 LIMIT $3
            )"#,
    )
    .bind(binding_id)
    .bind(HISTORY_RETENTION)
    .bind(HISTORY_PURGE_BATCH)
    .execute(pool)
    .await
    .map_err(database_error)?;
    Ok(())
}

fn repo_host(repo_url: &str) -> AppResult<String> {
    reqwest::Url::parse(repo_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .ok_or_else(|| AppError::bad_request("repository URL requires a host"))
}

async fn run_claimed(
    st: SharedState,
    claim: RepoScanClaim,
) -> AppResult<RepoBindingScanResultModel> {
    let work = run_claimed_to_completion(st.clone(), claim.clone());
    tokio::pin!(work);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(60));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The first interval tick is immediate; consume it because the claim was
    // just persisted with a full lease.
    heartbeat.tick().await;
    loop {
        tokio::select! {
            result = &mut work => return result,
            _ = heartbeat.tick() => {
                let renewed = tokio::time::timeout(
                    Duration::from_secs(HEARTBEAT_TIMEOUT_SECONDS),
                    renew(st.pg(), &claim),
                ).await;
                match renewed {
                    Ok(Ok(true)) => {}
                    Ok(Ok(false)) => {
                        return Err(AppError::conflict("Repository binding scan lease was lost"));
                    }
                    Ok(Err(error)) => {
                        let message = format!("repository scan heartbeat failed: {error}");
                        let _ = finish_failure_bounded(st.pg(), &claim, &message).await;
                        return Err(error);
                    }
                    Err(_) => {
                        let message = "repository scan heartbeat timed out";
                        let _ = finish_failure_bounded(st.pg(), &claim, message).await;
                        return Err(AppError::internal(message));
                    }
                }
            }
        }
    }
}

async fn run_claimed_to_completion(
    st: SharedState,
    claim: RepoScanClaim,
) -> AppResult<RepoBindingScanResultModel> {
    let result = async {
        let host = repo_host(&claim.repo_url)?;
        let host_lock =
            crate::utils::single_flight::PgSessionAdvisoryLock::acquire_repo_host(st.pg(), &host)
                .await
                .map_err(|error| AppError::internal(format!("lock repository host: {error}")))?;
        let checkout_path = std::path::PathBuf::from(&st.config.storage_root)
            .join("repos")
            .join(claim.id.to_string());
        let checkout =
            match crate::services::git_sync::lock_checkout_distributed(st.pg(), &checkout_path)
                .await
            {
                Ok(checkout) => checkout,
                Err(error) => {
                    if let Err(unlock_error) = host_lock.release().await {
                        tracing::warn!(
                            %unlock_error,
                            "repository host unlock failed after checkout lock failure"
                        );
                    }
                    return Err(error);
                }
            };
        let scan = crate::controllers::admin::execute_repo_binding_scan(&st, &claim).await;
        let result = match scan {
            Ok(execution) => {
                if !finish_success(st.pg(), &claim, &execution).await? {
                    Err(AppError::conflict(
                        "Repository binding scan completion lost its lease",
                    ))
                } else {
                    Ok(execution.result)
                }
            }
            Err(error) => {
                if let Err(finalize_error) =
                    finish_failure_bounded(st.pg(), &claim, &error.to_string()).await
                {
                    tracing::error!(
                        %finalize_error,
                        binding_id = claim.id,
                        "repository scan failure could not be finalized while fenced"
                    );
                }
                Err(error)
            }
        };
        drop(checkout);
        if let Err(error) = host_lock.release().await {
            tracing::warn!(%error, "repository host unlock failed; connection is close-on-drop");
        }
        result
    }
    .await;

    if let Err(error) = &result {
        if let Err(finalize_error) =
            finish_failure_bounded(st.pg(), &claim, &error.to_string()).await
        {
            tracing::error!(
                %finalize_error,
                binding_id = claim.id,
                "repository scan failure could not be finalized"
            );
        }
    }
    result
}

async fn run_claimed_supervised(
    st: SharedState,
    claim: RepoScanClaim,
) -> AppResult<RepoBindingScanResultModel> {
    use futures::FutureExt;

    let panic_claim = claim.clone();
    match std::panic::AssertUnwindSafe(run_claimed(st.clone(), claim))
        .catch_unwind()
        .await
    {
        Ok(result) => result,
        Err(_) => {
            let message = "repository scan worker panicked";
            let _ = finish_failure_bounded(st.pg(), &panic_claim, message).await;
            Err(AppError::internal(message))
        }
    }
}

/// Claim a manual scan before detaching it from the HTTP request. Dropping a
/// disconnected client's JoinHandle does not cancel an in-flight import.
pub async fn run_manual(st: SharedState, id: i32) -> AppResult<RepoBindingScanResultModel> {
    let admission = MANUAL_SCAN_GATE
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::too_many_requests(2))?;
    let claim = claim_manual(st.pg(), id).await?;
    tokio::spawn(async move {
        let _admission = admission;
        run_claimed_supervised(st, claim).await
    })
    .await
    .map_err(|error| AppError::internal(format!("repository scan task failed: {error}")))?
}

/// Active-active scheduler. Every control replica may run it; PostgreSQL owns
/// exact claim and recovery semantics while this JoinSet enforces the local cap.
pub fn start(
    st: SharedState,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut workers = tokio::task::JoinSet::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(SCAN_TICK_SECONDS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                }
                _ = ticker.tick() => {
                    let available = SCAN_BATCH.saturating_sub(workers.len() as i64);
                    if available > 0 {
                        match claim_due(st.pg(), available).await {
                            Ok(claims) => for claim in claims {
                                let worker_state = st.clone();
                                workers.spawn(async move {
                                    run_claimed_supervised(worker_state, claim).await
                                });
                            },
                            Err(error) => tracing::error!(%error, "repository scheduler claim failed"),
                        }
                    }
                }
                result = workers.join_next(), if !workers.is_empty() => {
                    match result {
                        Some(Ok(Ok(_))) => {}
                        Some(Ok(Err(error))) => tracing::warn!(%error, "scheduled repository scan failed"),
                        Some(Err(error)) => tracing::error!(%error, "scheduled repository scan task failed"),
                        None => {}
                    }
                }
            }
        }
        let drain = async {
            while let Some(result) = workers.join_next().await {
                if let Err(error) = result {
                    tracing::error!(%error, "repository scan task failed during shutdown drain");
                }
            }
        };
        if tokio::time::timeout(Duration::from_secs(SHUTDOWN_DRAIN_SECONDS), drain)
            .await
            .is_err()
        {
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            tracing::warn!(
                "repository scans exceeded the shutdown drain and were aborted; durable leases will recover"
            );
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_are_rejected_instead_of_becoming_a_hot_loop() {
        assert!(validate_interval(59).is_err());
        assert_eq!(validate_interval(60).unwrap(), 60);
        assert_eq!(validate_interval(86_400).unwrap(), 86_400);
        assert!(validate_interval(86_401).is_err());
    }

    #[test]
    fn due_claim_is_database_timed_cross_replica_and_per_host_bounded() {
        assert!(CLAIM_DUE_SQL.contains("clock_timestamp()"));
        assert!(CLAIM_DUE_SQL.contains("WITH candidates AS MATERIALIZED"));
        assert!(CLAIM_DUE_SQL.contains("LIMIT 64"));
        assert!(CLAIM_DUE_SQL.contains("FOR UPDATE OF binding SKIP LOCKED"));
        assert!(CLAIM_DUE_SQL.contains("row_number() OVER"));
        assert!(CLAIM_DUE_SQL.contains("scan_lease_expires_at_utc"));
        assert!(CLAIM_DUE_SQL.contains("scan_attempt = LEAST"));
        let source = include_str!("repo_binding_scheduler.rs");
        assert!(source.contains("power(2, LEAST(scan_failures + 1, 7))"));
        assert!(source.contains("(id::bigint + scan_attempt) % 31"));
        assert!(source.contains("clock_timestamp() + make_interval(secs => interval_seconds)"));
        assert!(source.contains("try_acquire_owned()"));
        assert!(source.contains("SHUTDOWN_DRAIN_SECONDS"));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_claims_are_cross_replica_host_bounded_and_lease_recoverable() {
        use std::str::FromStr;

        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("repo_scheduler_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            r#"CREATE TABLE "RepoBindings" (
                   id INTEGER PRIMARY KEY,
                   repo_url TEXT NOT NULL,
                   status SMALLINT NOT NULL,
                   next_scan_utc TIMESTAMPTZ NOT NULL,
                   interval_seconds INTEGER NOT NULL DEFAULT 60,
                   scan_lease_owner UUID,
                   scan_lease_expires_at_utc TIMESTAMPTZ,
                   scan_attempt BIGINT NOT NULL DEFAULT 0,
                   scan_failures INTEGER NOT NULL DEFAULT 0,
                   current_activity TEXT,
                   last_scan_utc TIMESTAMPTZ,
                   last_commit_sha TEXT,
                   last_scan_message TEXT
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"INSERT INTO "RepoBindings" (id, repo_url, status, next_scan_utc)
               VALUES (1, 'https://github.com/one/a', 0, clock_timestamp() - interval '1 minute'),
                      (2, 'https://github.com/one/b', 0, clock_timestamp() - interval '1 minute'),
                      (3, 'https://gitlab.com/two/c', 0, clock_timestamp() - interval '1 minute'),
                      (4, 'https://example.com/paused', 1, clock_timestamp() - interval '1 day')"#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let (left, right) = tokio::join!(claim_due(&pool, 4), claim_due(&pool, 4));
        let mut claimed = left.unwrap();
        claimed.extend(right.unwrap());
        assert_eq!(
            claimed.len(),
            2,
            "replicas must not duplicate a durable claim"
        );
        claimed.sort_by_key(|claim| claim.id);
        claimed.dedup_by_key(|claim| claim.id);
        assert_eq!(
            claimed.iter().map(|claim| claim.id).collect::<Vec<_>>(),
            vec![1, 3],
            "one binding per upstream host is claimed across replicas"
        );

        sqlx::query(
            r#"UPDATE "RepoBindings"
                  SET next_scan_utc = clock_timestamp() + interval '1 hour',
                      scan_lease_expires_at_utc = clock_timestamp() - interval '1 second'
                WHERE id = 1"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let recovered = claim_due(&pool, 4).await.unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].id, 2);

        let (manual_a, manual_b) = tokio::join!(claim_manual(&pool, 4), claim_manual(&pool, 4));
        assert_ne!(
            manual_a.is_ok(),
            manual_b.is_ok(),
            "manual race has one owner"
        );

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
