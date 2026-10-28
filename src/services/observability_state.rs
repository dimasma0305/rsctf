//! Periodic durable gauges. Scrapes never run database queries.
use crate::app_state::SharedState;
use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};
use tokio::sync::watch;

pub(super) const JOB_KINDS: [&str; 8] = [
    "AdReset",
    "AdReconcile",
    "ChallengeBuild",
    "BuildBatch",
    "WorkloadRollout",
    "ImagePreflight",
    "VariantGeneration",
    "SecurityDerivation",
];
pub(super) static QUEUED: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub(super) static RUNNING: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub(super) static EXPIRED: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub(super) static OLDEST: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
/// Process counters, unlike the sampled deployment gauges above. Count committed
/// expired-lease claims only; ordinary queued claims and failed polls are excluded.
pub(super) static RECLAIMED: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
pub(super) fn record_control_job_reclaim(kind: &str) {
    if let Some(index) = JOB_KINDS.iter().position(|known| *known == kind) {
        RECLAIMED[index].fetch_add(1, Relaxed);
    }
}
pub(super) static WORKER_CERT_EXPIRED: AtomicU64 = AtomicU64::new(0);
pub(super) static WORKER_CERT_7_DAYS: AtomicU64 = AtomicU64::new(0);
pub(super) static WORKER_CERT_30_DAYS: AtomicU64 = AtomicU64::new(0);
pub(super) static SAMPLED_AT: AtomicU64 = AtomicU64::new(0);
pub(super) static SAMPLE_ERRORS: AtomicU64 = AtomicU64::new(0);

#[derive(sqlx::FromRow)]
struct JobCounts {
    kind: String,
    queued: i64,
    running: i64,
    expired: i64,
    oldest: i64,
}

async fn sample(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    let jobs = sqlx::query_as::<_, JobCounts>(r#"
        SELECT kind,
               COUNT(*) FILTER (WHERE status = 0) AS queued,
               COUNT(*) FILTER (WHERE status = 1 AND lease_expires_at_utc > clock_timestamp()) AS running,
               COUNT(*) FILTER (WHERE status = 1 AND lease_expires_at_utc <= clock_timestamp()) AS expired,
               COALESCE(MAX(GREATEST(0, EXTRACT(EPOCH FROM clock_timestamp() - created_at_utc)))
                   FILTER (WHERE status = 0), 0)::bigint AS oldest
          FROM "ControlPlaneJobs" WHERE status IN (0, 1) AND kind = ANY($1)
         GROUP BY kind"#).bind(JOB_KINDS.as_slice()).fetch_all(pool).await?;
    let certificates = sqlx::query_as::<_, (i64, i64, i64)>(
        r#"
        SELECT COUNT(*) FILTER (WHERE certificate_expires_at <= clock_timestamp()),
               COUNT(*) FILTER (WHERE certificate_expires_at > clock_timestamp()
                   AND certificate_expires_at <= clock_timestamp() + interval '7 days'),
               COUNT(*) FILTER (WHERE certificate_expires_at > clock_timestamp()
                   AND certificate_expires_at <= clock_timestamp() + interval '30 days')
          FROM "WorkerNodes" WHERE administrative_state <> 'Disabled'"#,
    )
    .fetch_one(pool)
    .await?;
    // Publish only after both queries succeed; failed samples keep their original
    // timestamp so a monitoring system can distinguish zero backlog from stale data.
    for (index, kind) in JOB_KINDS.iter().enumerate() {
        let row = jobs.iter().find(|row| row.kind == *kind);
        QUEUED[index].store(row.map_or(0, |row| row.queued.max(0) as u64), Relaxed);
        RUNNING[index].store(row.map_or(0, |row| row.running.max(0) as u64), Relaxed);
        EXPIRED[index].store(row.map_or(0, |row| row.expired.max(0) as u64), Relaxed);
        OLDEST[index].store(row.map_or(0, |row| row.oldest.max(0) as u64), Relaxed);
    }
    WORKER_CERT_EXPIRED.store(certificates.0 as u64, Relaxed);
    WORKER_CERT_7_DAYS.store(certificates.1 as u64, Relaxed);
    WORKER_CERT_30_DAYS.store(certificates.2 as u64, Relaxed);
    SAMPLED_AT.store(chrono::Utc::now().timestamp().max(0) as u64, Relaxed);
    Ok(())
}

pub fn start(
    state: SharedState,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *shutdown.borrow() {
                return;
            }
            tokio::select! {
                _ = shutdown.changed() => return,
                _ = interval.tick() => {
                    if !tokio::time::timeout(Duration::from_secs(3), sample(state.pg())).await.is_ok_and(|result| result.is_ok()) {
                        SAMPLE_ERRORS.fetch_add(1, Relaxed);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn durable_gauges_exclude_disabled_certificates_and_reset_empty_kinds() {
        let db = crate::utils::test_database::TestDatabase::new("metrics_state").await;
        sqlx::raw_sql(r#"
            CREATE TABLE "ControlPlaneJobs" (kind text, status smallint, created_at_utc timestamptz, lease_expires_at_utc timestamptz);
            CREATE TABLE "WorkerNodes" (administrative_state text, certificate_expires_at timestamptz);
            INSERT INTO "ControlPlaneJobs" VALUES
              ('AdReset', 0, clock_timestamp() - interval '120 seconds', NULL),
              ('AdReset', 1, clock_timestamp(), clock_timestamp() - interval '1 second'),
              ('BuildBatch', 1, clock_timestamp(), clock_timestamp() + interval '1 minute'),
              ('arbitrary-secret-label', 0, clock_timestamp(), NULL);
            INSERT INTO "WorkerNodes" VALUES
              ('Active', clock_timestamp() - interval '1 day'),
              ('Active', clock_timestamp() + interval '1 day'),
              ('Active', clock_timestamp() + interval '15 days'),
              ('Disabled', clock_timestamp() - interval '1 day');
        "#).execute(&db.pool).await.unwrap();
        sample(&db.pool).await.unwrap();
        assert_eq!(QUEUED[0].load(Relaxed), 1);
        assert_eq!(EXPIRED[0].load(Relaxed), 1);
        assert!(OLDEST[0].load(Relaxed) >= 120);
        assert_eq!(RUNNING[3].load(Relaxed), 1);
        assert_eq!(WORKER_CERT_EXPIRED.load(Relaxed), 1);
        assert_eq!(WORKER_CERT_7_DAYS.load(Relaxed), 1);
        assert_eq!(WORKER_CERT_30_DAYS.load(Relaxed), 2);
        sqlx::query(r#"DELETE FROM "ControlPlaneJobs""#)
            .execute(&db.pool)
            .await
            .unwrap();
        sample(&db.pool).await.unwrap();
        assert_eq!(QUEUED[0].load(Relaxed), 0);
        assert_eq!(EXPIRED[0].load(Relaxed), 0);
        assert_eq!(OLDEST[0].load(Relaxed), 0);
        db.close().await;
    }
}
