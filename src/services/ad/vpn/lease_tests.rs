use super::*;

#[tokio::test]
#[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL; run database regressions serially"]
async fn exact_session_proof_detects_unlock_disconnect_and_preserves_graceful_drain() {
    let fixture = crate::utils::test_database::TestDatabase::new("network_proof").await;
    let db = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(fixture.pool.clone());
    acquire_instance_lease(&db).await.unwrap();
    assert!(owns_instance_lease());
    assert!(refresh_proof().await);
    let contender: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(VPN_INSTANCE_LOCK)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert!(!contender);
    // Leave the TCP session alive while dropping only its advisory lock.
    {
        let mut slot = INSTANCE_LEASE.lock().await;
        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(VPN_INSTANCE_LOCK)
            .fetch_one(&mut **slot.as_mut().unwrap())
            .await
            .unwrap();
        assert!(unlocked);
        let alive: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut **slot.as_mut().unwrap())
            .await
            .unwrap();
        assert_eq!(alive, 1);
    }
    assert!(
        !refresh_proof().await,
        "a healthy connection alone is not ownership"
    );
    assert!(!owns_instance_lease());
    assert!(release_instance_lease().await.is_err());

    acquire_instance_lease(&db).await.unwrap();
    let pid: i32 = {
        let mut slot = INSTANCE_LEASE.lock().await;
        sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut **slot.as_mut().unwrap())
            .await
            .unwrap()
    };
    let killed: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert!(killed);
    assert!(!refresh_proof().await);
    assert!(!owns_instance_lease());
    assert!(release_instance_lease().await.is_err());

    acquire_instance_lease(&db).await.unwrap();
    let monitor = start_instance_lease_monitor();
    // Draining ordinary workers must not stop the ownership monitor. It retains
    // fresh proof beyond the initial deadline until explicit session release.
    tokio::time::sleep(Duration::from_millis(proof::FRESHNESS_MS + 100)).await;
    assert!(owns_instance_lease());
    release_instance_lease().await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), monitor)
        .await
        .unwrap()
        .unwrap();
    assert!(!owns_instance_lease());
    let mut contender = fixture.pool.acquire().await.unwrap();
    contender.close_on_drop();
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(VPN_INSTANCE_LOCK)
        .fetch_one(&mut *contender)
        .await
        .unwrap();
    assert!(
        acquired,
        "intentional drain must release ownership for replacement"
    );
    drop(contender);
    drop(db);
    fixture.close().await;
}
