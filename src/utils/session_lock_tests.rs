//! Run against direct PostgreSQL and session-mode PgBouncer using the same test.
use super::*;

async fn acquire_eventually(connection: &mut sqlx::PgConnection, key: i64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
                .bind(key)
                .fetch_one(&mut *connection)
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("closed owner must release its session lock");
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(key)
            .fetch_one(connection)
            .await
            .unwrap()
    );
}

#[tokio::test]
#[ignore = "requires direct or session-pooled PostgreSQL via RSCTF_TEST_DATABASE_URL"]
async fn session_ownership_excludes_rivals_and_survives_cancellation_unlock_and_disconnect() {
    let url = std::env::var("RSCTF_TEST_DATABASE_URL").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let name = format!("session-regression:{}", uuid::Uuid::new_v4());
    let key = advisory_lock_key(&name);
    let owner = PgSessionAdvisoryLock::acquire_with_permit(&pool, &name, None)
        .await
        .unwrap();
    let mut observer = pool.acquire().await.unwrap();
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut *observer)
            .await
            .unwrap()
    );
    let waiter_pool = pool.clone();
    let waiter_name = name.clone();
    let waiter = tokio::spawn(async move {
        PgSessionAdvisoryLock::acquire_with_permit(&waiter_pool, &waiter_name, None).await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiter.is_finished());
    waiter.abort();
    assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
    owner.release().await.unwrap();
    acquire_eventually(&mut observer, key).await;

    let owner = PgSessionAdvisoryLock::acquire_with_permit(&pool, &name, None)
        .await
        .unwrap();
    drop(owner); // Request cancellation / process-local owner drop.
    acquire_eventually(&mut observer, key).await;

    let mut owner = PgSessionAdvisoryLock::acquire_with_permit(&pool, &name, None)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(owner.connection_mut())
        .await
        .unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT pg_terminate_backend($1)")
            .bind(pid)
            .fetch_one(&mut *observer)
            .await
            .unwrap()
    );
    acquire_eventually(&mut observer, key).await;
    assert!(
        owner.release().await.is_err(),
        "terminated owner must fail closed"
    );
    drop(observer);
    pool.close().await;
}
