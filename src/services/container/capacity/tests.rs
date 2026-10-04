use super::store::{self, ManagedRuntime, Reservation};
use super::LocalCapacity;
use crate::utils::error::AppError;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const GIB: i64 = 1024 * 1024 * 1024;

struct Fixture {
    admin: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn new() -> Self {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("local_capacity_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let fixture = Self { admin, schema };
        let pool = fixture.replica().await;
        sqlx::raw_sql(crate::migrations::LOCAL_CONTAINER_CAPACITY_SQL)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        fixture
    }

    /// A separate pool models a separate control replica.
    async fn replica(&self) -> sqlx::PgPool {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL").unwrap();
        let options = crate::migrations::test_pg_connect_options(&database_url)
            .options([("search_path", self.schema.as_str())]);
        PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap()
    }

    async fn drop(self) {
        sqlx::query(&format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

fn reservation(key: &str, cpu_millis: i64, memory_bytes: i64) -> Reservation {
    Reservation {
        key: key.to_string(),
        cpu_millis,
        memory_bytes,
        slots: 1,
    }
}

fn is_retryable(error: &AppError) -> bool {
    matches!(error, AppError::RetryableUnavailable { retry_after, .. } if *retry_after >= 1)
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn two_replicas_cannot_both_admit_the_last_slot() {
    let fixture = Fixture::new().await;
    let host = "host-a";
    let capacity = LocalCapacity {
        cpu_millis: 4_000,
        memory_bytes: 8 * GIB,
        slots: 1,
    };
    let replica_a = fixture.replica().await;
    let replica_b = fixture.replica().await;

    for _ in 0..8 {
        sqlx::query(r#"DELETE FROM "LocalContainerReservations""#)
            .execute(&replica_a)
            .await
            .unwrap();
        let request_a = reservation("op-a", 1_000, GIB);
        let request_b = reservation("op-b", 1_000, GIB);
        let (first, second) = tokio::join!(
            store::reserve(&replica_a, host, &capacity, &request_a),
            store::reserve(&replica_b, host, &capacity, &request_b),
        );
        let outcomes = [first, second];
        let admitted = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
        assert_eq!(admitted, 1, "exactly one replica may win the last slot");
        let rejected = outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().err())
            .expect("the loser receives an error");
        assert!(is_retryable(rejected), "{rejected:?}");
        let (_, _, slots) = store::usage(&replica_a, host).await.unwrap();
        assert_eq!(slots, 1);
    }

    // A retry with the winning key re-reserves instead of double counting.
    let winner = if store::usage(&replica_a, host).await.unwrap().2 == 1 {
        sqlx::query_scalar::<_, String>(
            r#"SELECT reservation_key FROM "LocalContainerReservations" LIMIT 1"#,
        )
        .fetch_one(&replica_a)
        .await
        .unwrap()
    } else {
        unreachable!("one reservation must remain")
    };
    store::reserve(
        &replica_b,
        host,
        &capacity,
        &reservation(&winner, 1_000, GIB),
    )
    .await
    .unwrap();
    assert_eq!(store::usage(&replica_a, host).await.unwrap().2, 1);

    replica_a.close().await;
    replica_b.close().await;
    fixture.drop().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn cpu_and_memory_are_accounted_and_oversized_requests_are_unavailable() {
    let fixture = Fixture::new().await;
    let host = "host-b";
    let capacity = LocalCapacity {
        cpu_millis: 3_000,
        memory_bytes: 4 * GIB,
        slots: 16,
    };
    let pool = fixture.replica().await;

    // Usage after each admitted step: (2000m, 1 GiB) -> (2500m, 3 GiB).
    store::reserve(&pool, host, &capacity, &reservation("one", 2_000, GIB))
        .await
        .unwrap();
    let cpu_exhausted = store::reserve(&pool, host, &capacity, &reservation("two", 2_000, GIB))
        .await
        .unwrap_err();
    assert!(is_retryable(&cpu_exhausted), "{cpu_exhausted:?}");
    store::reserve(&pool, host, &capacity, &reservation("three", 500, 2 * GIB))
        .await
        .unwrap();
    let still_fits = store::reserve(&pool, host, &capacity, &reservation("four", 1, 1)).await;
    assert!(still_fits.is_ok(), "1 byte still fits: {still_fits:?}");
    let memory_exhausted = store::reserve(&pool, host, &capacity, &reservation("five", 1, GIB))
        .await
        .unwrap_err();
    assert!(is_retryable(&memory_exhausted), "{memory_exhausted:?}");

    let oversized = store::reserve(&pool, host, &capacity, &reservation("six", 4_000, GIB))
        .await
        .unwrap_err();
    assert!(
        matches!(oversized, AppError::ServiceUnavailable(_)),
        "{oversized:?}"
    );
    let other_host = store::reserve(
        &pool,
        "host-c",
        &capacity,
        &reservation("seven", 3_000, GIB),
    )
    .await;
    assert!(other_host.is_ok(), "hosts are accounted independently");

    pool.close().await;
    fixture.drop().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn failure_and_removal_release_their_reservation() {
    let fixture = Fixture::new().await;
    let host = "host-d";
    let capacity = LocalCapacity {
        cpu_millis: 1_000,
        memory_bytes: GIB,
        slots: 1,
    };
    let pool = fixture.replica().await;

    store::reserve(&pool, host, &capacity, &reservation("failed", 1_000, GIB))
        .await
        .unwrap();
    assert!(is_retryable(
        &store::reserve(&pool, host, &capacity, &reservation("next", 1_000, GIB))
            .await
            .unwrap_err()
    ));
    store::release_key(&pool, host, "failed").await.unwrap();
    store::reserve(&pool, host, &capacity, &reservation("next", 1_000, GIB))
        .await
        .unwrap();
    store::attach(&pool, host, "next", "container-1")
        .await
        .unwrap();
    assert!(is_retryable(
        &store::reserve(&pool, host, &capacity, &reservation("after", 1_000, GIB))
            .await
            .unwrap_err()
    ));
    store::release_backend(&pool, host, "container-1")
        .await
        .unwrap();
    assert_eq!(store::usage(&pool, host).await.unwrap(), (0, 0, 0));
    store::reserve(&pool, host, &capacity, &reservation("after", 1_000, GIB))
        .await
        .unwrap();

    pool.close().await;
    fixture.drop().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn stale_reservations_reconcile_against_the_labeled_inventory() {
    let fixture = Fixture::new().await;
    let host = "host-e";
    let capacity = LocalCapacity {
        cpu_millis: 64_000,
        memory_bytes: 64 * GIB,
        slots: 64,
    };
    let pool = fixture.replica().await;

    for key in [
        "live",
        "vanished",
        "adopted-op",
        "abandoned",
        "fresh",
        "young-vanished",
    ] {
        store::reserve(&pool, host, &capacity, &reservation(key, 1_000, GIB))
            .await
            .unwrap();
    }
    store::attach(&pool, host, "live", "container-live")
        .await
        .unwrap();
    store::attach(&pool, host, "vanished", "container-gone")
        .await
        .unwrap();
    store::attach(&pool, host, "young-vanished", "container-young")
        .await
        .unwrap();
    // Age the rows that must be past their grace windows.
    sqlx::query(
        r#"UPDATE "LocalContainerReservations"
              SET updated_at_utc = clock_timestamp() - interval '20 minutes'
            WHERE reservation_key IN ('live', 'vanished', 'abandoned', 'adopted-op')"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let inventory = vec![
        ManagedRuntime {
            backend_id: "container-live".into(),
            operation_id: None,
        },
        ManagedRuntime {
            backend_id: "container-adopted".into(),
            operation_id: Some("adopted-op".into()),
        },
    ];
    let released = store::reconcile(&pool, host, &inventory).await.unwrap();
    assert_eq!(
        released, 2,
        "the vanished launch and the abandoned pre-launch row"
    );

    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        r#"SELECT reservation_key, backend_id FROM "LocalContainerReservations"
            ORDER BY reservation_key"#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "adopted-op".to_string(),
                Some("container-adopted".to_string())
            ),
            ("fresh".to_string(), None),
            ("live".to_string(), Some("container-live".to_string())),
            (
                "young-vanished".to_string(),
                Some("container-young".to_string())
            ),
        ]
    );

    // An abandoned pre-launch row is also aged out by the next admission.
    sqlx::query(
        r#"UPDATE "LocalContainerReservations"
              SET updated_at_utc = clock_timestamp() - interval '20 minutes'
            WHERE reservation_key = 'fresh'"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    store::reserve(&pool, host, &capacity, &reservation("admitted", 1_000, GIB))
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*)::bigint FROM "LocalContainerReservations"
            WHERE reservation_key = 'fresh'"#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, 0);

    pool.close().await;
    fixture.drop().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn a_contended_host_row_returns_the_retryable_response() {
    let fixture = Fixture::new().await;
    let host = "host-f";
    let capacity = LocalCapacity {
        cpu_millis: 4_000,
        memory_bytes: 4 * GIB,
        slots: 4,
    };
    let holder = fixture.replica().await;
    let contender = fixture.replica().await;
    store::reserve(&holder, host, &capacity, &reservation("seed", 1_000, GIB))
        .await
        .unwrap();
    let mut blocker = holder.begin().await.unwrap();
    sqlx::query(r#"SELECT 1 FROM "LocalContainerCapacityHosts" WHERE host_key = $1 FOR UPDATE"#)
        .bind(host)
        .execute(&mut *blocker)
        .await
        .unwrap();

    let error = store::reserve(
        &contender,
        host,
        &capacity,
        &reservation("blocked", 1_000, GIB),
    )
    .await
    .unwrap_err();
    assert!(is_retryable(&error), "{error:?}");
    blocker.rollback().await.unwrap();
    store::reserve(
        &contender,
        host,
        &capacity,
        &reservation("blocked", 1_000, GIB),
    )
    .await
    .unwrap();

    holder.close().await;
    contender.close().await;
    fixture.drop().await;
}

#[test]
fn a_spec_reservation_uses_millicores_mebibytes_and_one_slot() {
    let spec = crate::services::container::ContainerSpec {
        game_kind: rsctf_worker_protocol::GameKind::Jeopardy,
        image: "sha256:test".into(),
        memory_limit: 256,
        cpu_count: 2,
        storage_limit: crate::services::container::DEFAULT_CONTAINER_STORAGE_MB,
        expose_port: 80,
        publish_port: true,
        proxy_only: false,
        env: Vec::new(),
        flag: None,
        ad_network: None,
        allow_egress: false,
        control_plane_callback_ports: Vec::new(),
        network_mode: crate::utils::enums::NetworkMode::Open,
        operation_id: Some("exercise-container:abc".into()),
    };
    let reservation = Reservation::for_spec(&spec).unwrap();
    assert_eq!(reservation.key, "exercise-container:abc");
    assert_eq!(reservation.cpu_millis, 2_000);
    assert_eq!(reservation.memory_bytes, 256 * 1024 * 1024);
    assert_eq!(reservation.slots, 1);
    let anonymous = Reservation::for_spec(&crate::services::container::ContainerSpec {
        operation_id: None,
        ..spec
    })
    .unwrap();
    assert!(Uuid::parse_str(&anonymous.key).is_ok());
}
