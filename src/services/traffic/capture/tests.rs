use super::*;

fn spec(id: &str, host: &str, port: u16, challenge: i32, participation: i32) -> CaptureSpec {
    CaptureSpec {
        service_id: challenge * 100 + participation,
        container_id: id.to_string(),
        host_text: host.to_string(),
        host: host.parse().unwrap(),
        port,
        challenge_id: challenge,
        participation_id: participation,
    }
}

#[tokio::test]
async fn post_start_exit_is_returned_for_durable_fail_closed_handling() {
    let capture = spec("failed", "10.13.40.21", 8080, 4, 8);
    let mut registry = CaptureRegistry::default();
    registry.active.insert(
        capture.container_id.clone(),
        ActiveCapture {
            spec: capture.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            thread: std::thread::spawn(|| Err("pcap flush failed".to_string())),
        },
    );
    while !registry
        .active
        .get(&capture.container_id)
        .unwrap()
        .thread
        .is_finished()
    {
        tokio::task::yield_now().await;
    }

    let failures = registry.reap_finished().await;
    assert_eq!(
        failures,
        vec![CaptureFailure {
            spec: capture,
            error: "pcap flush failed".to_string(),
        }]
    );
    assert!(registry.active.is_empty());
}

#[test]
fn clean_unexpected_exit_is_still_a_capture_failure() {
    assert_eq!(
        unexpected_exit_error(Ok(Ok(17))),
        "capture exited unexpectedly after 17 packets"
    );
    assert_eq!(
        unexpected_exit_error(Err("capture thread panicked".to_string())),
        "capture thread panicked"
    );
}

#[test]
fn reconciliation_stops_changed_identity_before_starting_replacement() {
    let old = spec("same-id", "10.13.40.7", 80, 5, 9);
    let replacement = spec("same-id", "10.13.40.8", 80, 5, 9);
    let stale = spec("old-id", "10.13.40.9", 80, 5, 10);
    let fresh = spec("new-id", "10.13.40.10", 8080, 6, 11);
    let current = HashMap::from([
        (old.container_id.clone(), old),
        (stale.container_id.clone(), stale),
    ]);
    let desired = HashMap::from([
        (replacement.container_id.clone(), replacement.clone()),
        (fresh.container_id.clone(), fresh.clone()),
    ]);

    let plan = reconciliation_plan(&current, &desired);
    assert_eq!(plan.stop, vec!["old-id", "same-id"]);
    assert_eq!(
        plan.start
            .iter()
            .map(|capture| capture.container_id.as_str())
            .collect::<Vec<_>>(),
        vec!["new-id", "same-id"]
    );
    assert!(plan.start.contains(&replacement));
    assert!(plan.start.contains(&fresh));
}

#[test]
fn unchanged_capture_has_no_work() {
    let capture = spec("stable", "10.13.40.2", 443, 1, 2);
    let current = HashMap::from([(capture.container_id.clone(), capture.clone())]);
    let desired = current.clone();
    assert_eq!(
        reconciliation_plan(&current, &desired),
        ReconciliationPlan::default()
    );
}

#[test]
fn filter_is_scoped_to_service_ip_and_port() {
    assert_eq!(
        spec("c", "10.13.40.12", 8080, 1, 1).bpf_filter(),
        "host 10.13.40.12 and tcp port 8080"
    );
    assert_eq!(
        spec("v6", "2001:db8::7", 443, 1, 1).bpf_filter(),
        "host 2001:db8::7 and tcp port 443"
    );
}

#[test]
fn invalid_desired_endpoint_fails_closed() {
    let row = DesiredCaptureRow {
        service_id: 7,
        container_id: "container".to_string(),
        host: "service.internal".to_string(),
        port: 80,
        challenge_id: 1,
        participation_id: 2,
    };
    assert!(CaptureSpec::from_row(row).is_err());
}

#[test]
fn filenames_are_safe_and_distinguish_common_prefixes() {
    let first = capture_filename("abcdefghijkl-first/unsafe");
    let second = capture_filename("abcdefghijkl-second/unsafe");
    assert_ne!(first, second);
    for name in [first, second] {
        assert!(name.ends_with(".pcap"));
        assert!(!name.contains('/'));
        assert!(!name.contains(".."));
    }
}

#[test]
fn retained_backend_pointer_clears_only_after_endpoint_deactivation() {
    assert!(CLEAR_INACTIVE_BACKEND_SQL.contains("container_id = $1"));
    assert!(CLEAR_INACTIVE_BACKEND_SQL.contains("BTRIM(host)"));
    assert!(CLEAR_INACTIVE_BACKEND_SQL.contains("port = 0"));
}

#[test]
fn unrelated_container_teardown_bypasses_capture_owner() {
    assert!(CAPTURE_IDENTITY_STATE_SQL.contains("AS has_identity"));
    assert!(CAPTURE_IDENTITY_STATE_SQL.contains("service.container_id = $1"));
    assert!(CAPTURE_IDENTITY_STATE_SQL.contains("AS is_desired"));
}

#[tokio::test]
async fn per_request_result_requires_the_exact_live_capture() {
    let capture = spec("container", "10.13.40.20", 8080, 4, 7);
    let desired = HashMap::from([(capture.container_id.clone(), capture.clone())]);
    let mut registry = CaptureRegistry::default();

    assert_eq!(
        request_failure("Start", "container", &registry, &desired, false),
        Some("live traffic capture is unavailable on this runtime or container backend")
    );
    assert_eq!(
        request_failure("Start", "container", &registry, &desired, true),
        Some("libpcap capture startup failed; inspect the network-owner logs")
    );
    assert_eq!(
        request_failure("Stop", "container", &registry, &desired, true),
        None
    );

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        Ok(0)
    });
    registry.active.insert(
        capture.container_id.clone(),
        ActiveCapture {
            spec: capture,
            stop,
            thread,
        },
    );
    assert_eq!(
        request_failure("Start", "container", &registry, &desired, true),
        None
    );
    assert_eq!(
        request_failure("Stop", "container", &registry, &desired, true),
        Some("the obsolete traffic capture is still active; teardown was not acknowledged")
    );
    registry.stop_all().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn singleton_lease_hands_off_after_explicit_release() {
    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to PostgreSQL");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect test database");

    let first = try_acquire_owner(&pool)
        .await
        .expect("first lease query")
        .expect("first owner acquires lease");
    assert!(
        try_acquire_owner(&pool)
            .await
            .expect("contending lease query")
            .is_none(),
        "a second session must not acquire the owner lock"
    );

    release_owner(first).await.expect("release first owner");
    let second = try_acquire_owner(&pool)
        .await
        .expect("handoff lease query")
        .expect("second owner acquires after release");
    release_owner(second).await.expect("release second owner");
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn runtime_failure_deactivates_only_the_exact_observed_endpoint() {
    use sqlx::{Connection, PgConnection};

    let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
        .expect("RSCTF_TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let mut connection = PgConnection::connect(&database_url).await.unwrap();
    sqlx::raw_sql(
        r#"
        CREATE TEMP TABLE "AdTeamServices" (
            id INTEGER PRIMARY KEY,
            container_id TEXT,
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            status SMALLINT NOT NULL
        );
        CREATE TEMP TABLE "TrafficCaptureFailures" (
            id BIGSERIAL PRIMARY KEY,
            service_id INTEGER NOT NULL,
            container_id TEXT NOT NULL,
            host TEXT NOT NULL,
            port INTEGER NOT NULL,
            challenge_id INTEGER NOT NULL,
            participation_id INTEGER NOT NULL,
            detected_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
            error TEXT NOT NULL,
            endpoint_was_current BOOLEAN NOT NULL,
            endpoint_deactivated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
            network_revoked_at TIMESTAMPTZ,
            last_reconcile_error TEXT
        );
        CREATE UNIQUE INDEX test_capturefailure_pending
            ON "TrafficCaptureFailures" (service_id, container_id)
            WHERE network_revoked_at IS NULL;
        INSERT INTO "AdTeamServices" VALUES
            (1, 'runtime-current', '10.13.40.31', 8080, 0),
            (2, 'runtime-reused', '10.13.40.99', 9090, 0);
        "#,
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let current = CaptureSpec {
        service_id: 1,
        ..spec("runtime-current", "10.13.40.31", 8080, 7, 11)
    };
    let stale = CaptureSpec {
        service_id: 2,
        ..spec("runtime-reused", "10.13.40.32", 8080, 7, 12)
    };
    failures::persist_and_deactivate(
        &mut connection,
        &[
            CaptureFailure {
                spec: current,
                error: "read failed".to_string(),
            },
            CaptureFailure {
                spec: stale,
                error: "old thread failed".to_string(),
            },
        ],
    )
    .await
    .unwrap();

    let rows = sqlx::query_as::<_, (i32, String, i32, i16)>(
        r#"SELECT id, host, port, status FROM "AdTeamServices" ORDER BY id"#,
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(rows[0], (1, String::new(), 0, 2));
    assert_eq!(rows[1], (2, "10.13.40.99".to_string(), 9090, 0));

    let outcomes = sqlx::query_as::<_, (i32, bool)>(
        r#"SELECT service_id, endpoint_was_current
             FROM "TrafficCaptureFailures" ORDER BY service_id"#,
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();
    assert_eq!(outcomes, vec![(1, true), (2, false)]);
}
