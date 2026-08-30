use super::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

#[test]
fn user_limit_releases_with_the_session() {
    let admission = ProxyAdmission::new();
    let user = Uuid::new_v4();
    let workload = Uuid::new_v4();
    let permits = (0..MAX_PER_USER)
        .map(|_| {
            admission
                .try_acquire(user, 1, 2, workload, "127.0.0.1".parse().unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(admission
        .try_acquire(user, 1, 2, workload, "127.0.0.1".parse().unwrap())
        .is_none());
    drop(permits);
    assert!(admission
        .try_acquire(user, 1, 2, workload, "127.0.0.1".parse().unwrap())
        .is_some());
}

#[test]
fn participation_limit_spans_users_and_workloads() {
    let admission = ProxyAdmission::new();
    let permits = (0..MAX_PER_PARTICIPATION)
        .map(|_| {
            admission
                .try_acquire(
                    Uuid::new_v4(),
                    7,
                    9,
                    Uuid::new_v4(),
                    "127.0.0.1".parse().unwrap(),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(admission
        .try_acquire(
            Uuid::new_v4(),
            7,
            9,
            Uuid::new_v4(),
            "127.0.0.2".parse().unwrap()
        )
        .is_none());
    drop(permits);
}

#[test]
fn exercise_and_participation_scopes_with_the_same_id_are_independent() {
    let admission = ProxyAdmission::new();
    let participation_permits = (0..MAX_PER_PARTICIPATION)
        .map(|_| {
            admission
                .try_acquire(
                    Uuid::new_v4(),
                    7,
                    9,
                    Uuid::new_v4(),
                    "127.0.0.1".parse().unwrap(),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(admission
        .try_acquire(
            Uuid::new_v4(),
            7,
            9,
            Uuid::new_v4(),
            "127.0.0.2".parse().unwrap()
        )
        .is_none());
    assert!(admission
        .try_acquire_exercise(
            Uuid::new_v4(),
            7,
            Uuid::new_v4(),
            "127.0.0.2".parse().unwrap()
        )
        .is_some());
    drop(participation_permits);
}

#[test]
fn preview_sessions_share_all_global_ceilings_and_release() {
    let admission = ProxyAdmission::new();
    let user = Uuid::new_v4();
    let container = Uuid::new_v4();
    let source = "192.0.2.7".parse().unwrap();
    let permits = (0..MAX_PER_USER)
        .map(|_| {
            admission
                .try_acquire_preview(user, container, source)
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(admission
        .try_acquire_preview(user, container, source)
        .is_none());
    drop(permits);
    assert!(admission
        .try_acquire_preview(user, container, source)
        .is_some());
}

#[tokio::test]
async fn traffic_budget_rejects_sustained_session_work() {
    let admission = ProxyAdmission::new();
    let permit = admission
        .try_acquire_preview(Uuid::new_v4(), Uuid::new_v4(), "192.0.2.8".parse().unwrap())
        .unwrap();
    let traffic = permit.traffic();
    assert!(traffic.reserve(1024).await);
    assert!(traffic.reserve_control(0).await);
    assert!(!traffic.reserve(SESSION_TOTAL_BYTES as usize).await);
    assert_eq!(
        admission.traffic_metrics(),
        ProxyTrafficMetrics {
            accepted_bytes: 1024,
            accepted_frames: 2,
            accepted_control_frames: 1,
            rejected_frames: 1,
        }
    );
}

#[test]
fn concurrent_window_rollover_cannot_erase_admitted_work() {
    let window = Arc::new(FixedWindow::default());
    let accepted = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for _ in 0..16 {
        let window = Arc::clone(&window);
        let accepted = Arc::clone(&accepted);
        workers.push(std::thread::spawn(move || {
            for _ in 0..256 {
                if window.try_reserve(42, 1, 1_024, 1_024) {
                    accepted.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(accepted.load(Ordering::Relaxed), 1_024);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn user_ceiling_is_shared_by_independent_replica_admission_owners() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("connect test database");
    let schema = format!("proxy_admission_{}", Uuid::new_v4().simple());
    sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
        .execute(&admin)
        .await
        .expect("create isolated schema");
    let options = PgConnectOptions::from_str(&database_url)
        .expect("parse test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .expect("connect isolated pool");
    sqlx::raw_sql(
        r#"CREATE TABLE "ProxyTunnelLeases" (
             lease_id UUID PRIMARY KEY, user_id UUID NOT NULL,
             scope_kind SMALLINT NOT NULL, scope_id TEXT NOT NULL,
             source_ip TEXT NOT NULL, event_id INTEGER,
             workload_id UUID NOT NULL, expires_at_utc TIMESTAMPTZ NOT NULL,
             created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
           );
           CREATE TABLE "ProxyOpenBudgets" (
             bucket_start_utc TIMESTAMPTZ NOT NULL, source_key TEXT NOT NULL,
             open_count INTEGER NOT NULL,
             PRIMARY KEY (bucket_start_utc, source_key)
           );"#,
    )
    .execute(&pool)
    .await
    .expect("create distributed admission fixture");

    let user = Uuid::new_v4();
    let first_replica = ProxyAdmission::new();
    let second_replica = ProxyAdmission::new();
    let mut permits = Vec::new();
    for participation in 1..=MAX_PER_USER {
        permits.push(
            first_replica
                .try_acquire_distributed(
                    &pool,
                    user,
                    participation as i32,
                    7,
                    Uuid::new_v4(),
                    "192.0.2.10".parse().unwrap(),
                )
                .await
                .expect("distributed user permit"),
        );
    }
    assert!(second_replica
        .try_acquire_distributed(
            &pool,
            user,
            99,
            7,
            Uuid::new_v4(),
            "192.0.2.11".parse().unwrap(),
        )
        .await
        .is_none());
    drop(permits);

    pool.close().await;
    sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
        .execute(&admin)
        .await
        .expect("drop isolated schema");
    admin.close().await;
}
