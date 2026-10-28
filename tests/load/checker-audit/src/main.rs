//! Fixed-arrival component comparison of the old and new publication ordering.
//! Real venv creation, filesystem publication, PostgreSQL guard/commit, and
//! process execution; excludes HTTP, repository parsing, and dependency downloads.
#![allow(dead_code, unused_imports)]
use serde_json::json;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::time::Instant;
mod support;
pub(crate) use support::{app_state, package, services};
#[path = "../../../../src/utils/archive.rs"]
pub(crate) mod archive;
#[path = "../../../../src/services/git_sync/checker.rs"]
mod checker;
#[path = "../../../../src/services/git_sync/checker_gc.rs"]
mod checker_gc;
#[path = "../../../../src/utils/database.rs"]
pub(crate) mod database;
#[path = "../../../../src/utils/single_flight.rs"]
pub(crate) mod single_flight;
mod utils {
    pub(crate) use crate::support::error;
    pub(crate) use crate::{archive, database, single_flight};
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    let mode = std::env::args().nth(1).expect("before or after");
    assert!(matches!(mode.as_str(), "before" | "after"));
    let operations: usize = std::env::var("CHECKER_AUDIT_OPERATIONS")
        .unwrap_or("12".into())
        .parse()?;
    let rate: f64 = std::env::var("CHECKER_AUDIT_RATE")
        .unwrap_or("1".into())
        .parse()?;
    assert!((1..=64).contains(&operations) && rate.is_finite() && rate > 0.0 && rate <= 8.0);
    let options: PgConnectOptions = std::env::var("RSCTF_TEST_DATABASE_URL")?.parse()?;
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;
    let schema = format!("checker_audit_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await?;
    let pool = PgPoolOptions::new()
        .min_connections(4)
        .max_connections(4)
        .connect_with(options.options([("search_path", schema.as_str())]))
        .await?;
    sqlx::query(
        r#"CREATE TABLE "GameChallenges" (id INTEGER PRIMARY KEY, ad_checker_image TEXT NOT NULL)"#,
    )
    .execute(&pool)
    .await?;
    let root = std::env::temp_dir().join(format!("rsctf-checker-audit-{}", uuid::Uuid::new_v4()));
    let source = root.join("source");
    tokio::fs::create_dir_all(&source).await?;
    tokio::fs::write(source.join("run.py"), b"print('verified checker')\n").await?;
    tokio::fs::write(source.join("fixture.txt"), vec![b'R'; 16 * 1024]).await?;
    let started = Instant::now();
    let usage_before = (
        support::usage(libc::RUSAGE_SELF).0,
        support::usage(libc::RUSAGE_CHILDREN).0,
    );
    let sample_pool = pool.clone();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let sampler = tokio::spawn(async move {
        let mut samples = Vec::new();
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                _ = interval.tick() => samples.push(sample_pool.size() as usize - sample_pool.num_idle()),
            }
        }
        samples
    });
    // Four admitted importers model bounded concurrent consumers on two replicas.
    let admission = Arc::new(tokio::sync::Semaphore::new(4));
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..operations {
        let due = started + Duration::from_secs_f64(index as f64 / rate);
        tokio::time::sleep_until(due).await;
        let pool = pool.clone();
        let mode = mode.clone();
        let source = source.clone();
        let dest = root
            .join("checkers/1/fixture/revisions")
            .join(uuid::Uuid::new_v4().simple().to_string());
        let admission = admission.clone();
        tasks.spawn(async move {
            let _permit = admission.acquire().await?;
            let admitted = Instant::now();
            let (mut guard, prepared, hold_start, lock_wait, preparation) = if mode == "before" {
                let lock_start = Instant::now();
                let guard = checker_gc::acquire_checker_artifact_guard(&pool).await?;
                let hold_start = Instant::now();
                let prepared = checker::stage_checker_venv(dest.to_str().unwrap(), &source).await?;
                let preparation = hold_start.elapsed().as_secs_f64() * 1000.0;
                (
                    guard,
                    prepared,
                    hold_start,
                    (hold_start - lock_start).as_secs_f64() * 1000.0,
                    preparation,
                )
            } else {
                let prep_start = Instant::now();
                let prepared = checker::stage_checker_venv(dest.to_str().unwrap(), &source).await?;
                let preparation = prep_start.elapsed().as_secs_f64() * 1000.0;
                let lock_start = Instant::now();
                let guard = checker_gc::acquire_checker_artifact_guard(&pool).await?;
                let hold_start = Instant::now();
                (
                    guard,
                    prepared,
                    hold_start,
                    (hold_start - lock_start).as_secs_f64() * 1000.0,
                    preparation,
                )
            };
            prepared.publish().await?;
            sqlx::query(r#"INSERT INTO "GameChallenges" VALUES ($1, $2)"#)
                .bind(index as i32)
                .bind(dest.to_str().unwrap())
                .execute(&mut **guard.transaction_mut())
                .await?;
            guard.release().await?;
            let hold = hold_start.elapsed().as_secs_f64() * 1000.0;
            let latency = due.elapsed().as_secs_f64() * 1000.0;
            // Readback runs outside the publication latency, but its CPU is
            // included equally in both samples. Require a working published venv.
            let output = tokio::process::Command::new(dest.join("venv/bin/python3"))
                .arg(dest.join("src/run.py"))
                .kill_on_drop(true)
                .output()
                .await?;
            anyhow::ensure!(
                output.status.success() && output.stdout == b"verified checker\n",
                "checker integrity"
            );
            anyhow::ensure!(
                tokio::fs::read(dest.join("src/fixture.txt")).await? == vec![b'R'; 16 * 1024],
                "source integrity"
            );
            Ok::<_, anyhow::Error>([
                latency,
                lock_wait,
                hold,
                preparation,
                (admitted - due).as_secs_f64() * 1000.0,
            ])
        });
    }
    let mut samples = Vec::new();
    while let Some(result) = tasks.join_next().await {
        samples.push(result??);
    }
    // Include the complete offered-load window, even if the last job is fast.
    tokio::time::sleep_until(started + Duration::from_secs_f64(operations as f64 / rate)).await;
    let elapsed = started.elapsed().as_secs_f64();
    let own_usage = support::usage(libc::RUSAGE_SELF);
    let child_usage = support::usage(libc::RUSAGE_CHILDREN);
    stop.send(true)?;
    let occupancy = sampler.await?;
    let count: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM "GameChallenges""#)
        .fetch_one(&pool)
        .await?;
    anyhow::ensure!(
        count == operations as i64 && samples.len() == operations,
        "publication integrity"
    );
    let metric =
        |index| support::distribution(samples.iter().map(|sample| sample[index]).collect());
    println!(
        "{}",
        json!({"mode": mode, "operations": operations, "rate": rate, "elapsed_seconds": elapsed,
        "arrival_latency": metric(0), "lock_wait": metric(1), "lock_hold": metric(2), "preparation": metric(3), "admission_wait": metric(4),
        "pool_occupied_avg": occupancy.iter().sum::<usize>() as f64 / occupancy.len() as f64,
        "pool_occupied_max": occupancy.iter().max(), "pool_samples": occupancy.len(),
        "cpu_percent_including_children": (own_usage.0 + child_usage.0 - usage_before.0 - usage_before.1) / elapsed * 100.0,
        "parent_peak_rss_mib": own_usage.1 as f64 / 1024.0,
        "largest_child_rss_mib": child_usage.1 as f64 / 1024.0,
        "integrity_errors": 0})
    );
    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await?;
    admin.close().await;
    tokio::fs::remove_dir_all(root).await?;
    Ok(())
}
