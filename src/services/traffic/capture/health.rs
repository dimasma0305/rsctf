//! Durable capture-owner lease and exact live-endpoint publication.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::{Acquire, PgConnection, PgPool, Postgres};
use uuid::Uuid;

use super::CaptureSpec;
use crate::services::capture_safety::OWNER_LEASE_SECONDS;

const OWNER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(3);
const OWNER_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OwnerToken {
    id: Uuid,
    epoch: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
struct LiveEndpoint {
    service_id: i32,
    container_id: String,
    host: String,
    port: i32,
    owner_id: Uuid,
    owner_epoch: i64,
}

impl LiveEndpoint {
    fn from_spec(spec: &CaptureSpec, owner: OwnerToken) -> Self {
        Self {
            service_id: spec.service_id,
            container_id: spec.container_id.clone(),
            host: spec.host_text.clone(),
            port: i32::from(spec.port),
            owner_id: owner.id,
            owner_epoch: owner.epoch,
        }
    }
}

pub(super) async fn claim(connection: &mut PgConnection) -> Result<OwnerToken, sqlx::Error> {
    let id = Uuid::new_v4();
    let epoch: i64 = sqlx::query_scalar(
        r#"UPDATE "TrafficCaptureOwnerState"
              SET owner_id = $1,
                  owner_epoch = owner_epoch + 1,
                  heartbeat_at = clock_timestamp(),
                  lease_expires_at = clock_timestamp()
                      + ($2 * interval '1 second'),
                  draining = TRUE
            WHERE id = 1
            RETURNING owner_epoch"#,
    )
    .bind(id)
    .bind(OWNER_LEASE_SECONDS)
    .fetch_one(connection)
    .await?;
    Ok(OwnerToken { id, epoch })
}

async fn set_draining(
    connection: &mut PgConnection,
    owner: OwnerToken,
    draining: bool,
) -> Result<(), sqlx::Error> {
    let updated = sqlx::query(
        r#"UPDATE "TrafficCaptureOwnerState"
              SET draining = $3,
                  heartbeat_at = clock_timestamp(),
                  lease_expires_at = clock_timestamp()
                      + ($4 * interval '1 second')
            WHERE id = 1 AND owner_id = $1 AND owner_epoch = $2
              AND lease_expires_at > clock_timestamp()"#,
    )
    .bind(owner.id)
    .bind(owner.epoch)
    .bind(draining)
    .bind(OWNER_LEASE_SECONDS)
    .execute(connection)
    .await?;
    if updated.rows_affected() == 1 {
        Ok(())
    } else {
        Err(sqlx::Error::RowNotFound)
    }
}

pub(super) async fn activate(
    connection: &mut PgConnection,
    owner: OwnerToken,
) -> Result<(), sqlx::Error> {
    set_draining(connection, owner, false).await
}

pub(super) async fn begin_drain(
    connection: &mut PgConnection,
    owner: OwnerToken,
) -> Result<(), sqlx::Error> {
    set_draining(connection, owner, true).await
}

pub(super) async fn fence_unowned(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    let mut transaction = connection.begin().await?;
    sqlx::query(
        r#"UPDATE "TrafficCaptureOwnerState"
              SET owner_id = NULL,
                  owner_epoch = owner_epoch + 1,
                  heartbeat_at = NULL,
                  lease_expires_at = NULL,
                  draining = TRUE
            WHERE id = 1"#,
    )
    .execute(&mut *transaction)
    .await?;
    sqlx::query(r#"DELETE FROM "TrafficCaptureLiveEndpoints""#)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await
}

pub(super) async fn release(
    connection: &mut PgConnection,
    owner: OwnerToken,
) -> Result<(), sqlx::Error> {
    let mut transaction = connection.begin().await?;
    sqlx::query(
        r#"DELETE FROM "TrafficCaptureLiveEndpoints"
            WHERE owner_id = $1 AND owner_epoch = $2"#,
    )
    .bind(owner.id)
    .bind(owner.epoch)
    .execute(&mut *transaction)
    .await?;
    let updated = sqlx::query(
        r#"UPDATE "TrafficCaptureOwnerState"
              SET owner_id = NULL, heartbeat_at = NULL,
                  lease_expires_at = NULL, draining = TRUE
            WHERE id = 1 AND owner_id = $1 AND owner_epoch = $2"#,
    )
    .bind(owner.id)
    .bind(owner.epoch)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    transaction.commit().await
}

pub(super) async fn publish_live(
    connection: &mut PgConnection,
    owner: OwnerToken,
    active: &[CaptureSpec],
) -> Result<bool, sqlx::Error> {
    let mut expected = active
        .iter()
        .map(|spec| LiveEndpoint::from_spec(spec, owner))
        .collect::<Vec<_>>();
    expected.sort_unstable_by_key(|endpoint| endpoint.service_id);
    let mut transaction = connection.begin().await?;
    let token_is_current = sqlx::query_scalar::<_, i64>(
        r#"SELECT owner_epoch FROM "TrafficCaptureOwnerState"
            WHERE id = 1 AND owner_id = $1 AND owner_epoch = $2
              AND lease_expires_at > clock_timestamp()
            FOR UPDATE"#,
    )
    .bind(owner.id)
    .bind(owner.epoch)
    .fetch_optional(&mut *transaction)
    .await?
    .is_some();
    if !token_is_current {
        return Err(sqlx::Error::RowNotFound);
    }
    let current = sqlx::query_as::<_, LiveEndpoint>(
        r#"SELECT service_id, container_id, host, port, owner_id, owner_epoch
             FROM "TrafficCaptureLiveEndpoints"
            ORDER BY service_id"#,
    )
    .fetch_all(&mut *transaction)
    .await?;
    if current == expected {
        transaction.commit().await?;
        return Ok(false);
    }
    sqlx::query(r#"DELETE FROM "TrafficCaptureLiveEndpoints""#)
        .execute(&mut *transaction)
        .await?;
    for endpoint in expected {
        sqlx::query(
            r#"INSERT INTO "TrafficCaptureLiveEndpoints"
                   (service_id, container_id, host, port, owner_id,
                    owner_epoch, acknowledged_at)
               VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp())"#,
        )
        .bind(endpoint.service_id)
        .bind(endpoint.container_id)
        .bind(endpoint.host)
        .bind(endpoint.port)
        .bind(endpoint.owner_id)
        .bind(endpoint.owner_epoch)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(true)
}

async fn renew(connection: &mut PgConnection, owner: OwnerToken) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        r#"UPDATE "TrafficCaptureOwnerState"
              SET heartbeat_at = clock_timestamp(),
                  lease_expires_at = clock_timestamp()
                      + ($3 * interval '1 second')
            WHERE id = 1 AND owner_id = $1 AND owner_epoch = $2
              AND lease_expires_at > clock_timestamp()"#,
    )
    .bind(owner.id)
    .bind(owner.epoch)
    .bind(OWNER_LEASE_SECONDS)
    .execute(connection)
    .await?
    .rows_affected()
        == 1)
}

pub(super) struct OwnerHeartbeat {
    healthy: Arc<AtomicBool>,
    shutdown: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// A pool connection reserved before durable capture ownership is claimed.
///
/// The capture owner already reserves this connection in the role-aware pool
/// floor. Keeping it out of the general checkout queue prevents unrelated
/// repository, provisioning, and round work from starving the safety lease.
pub(super) struct ReservedOwnerHeartbeat {
    connection: PoolConnection<Postgres>,
}

impl OwnerHeartbeat {
    pub(super) async fn reserve(pool: &PgPool) -> Result<ReservedOwnerHeartbeat, String> {
        let mut connection = tokio::time::timeout(OWNER_HEARTBEAT_TIMEOUT, pool.acquire())
            .await
            .map_err(|_| "traffic capture heartbeat connection acquisition timed out".to_string())?
            .map_err(|error| error.to_string())?;
        // A timed-out or aborted UPDATE may have reached PostgreSQL without its
        // result being observed. This safety session is never reused, so close
        // it on every exit instead of returning ambiguous state to request work.
        connection.close_on_drop();
        Ok(ReservedOwnerHeartbeat { connection })
    }

    fn start(mut connection: PoolConnection<Postgres>, owner: OwnerToken) -> Self {
        let healthy = Arc::new(AtomicBool::new(true));
        let task_health = healthy.clone();
        let (shutdown, mut stop) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(OWNER_HEARTBEAT_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    changed = stop.changed() => {
                        if changed.is_err() || *stop.borrow() {
                            return;
                        }
                    }
                    _ = ticker.tick() => match tokio::time::timeout(
                        OWNER_HEARTBEAT_TIMEOUT,
                        renew(&mut connection, owner),
                    ).await {
                        Ok(Ok(true)) => {}
                        Ok(Ok(false)) => {
                            tracing::error!(epoch = owner.epoch, "traffic capture owner token was fenced");
                            task_health.store(false, Ordering::Release);
                            return;
                        }
                        Ok(Err(error)) => {
                            tracing::error!(%error, epoch = owner.epoch, "traffic capture owner heartbeat failed");
                            task_health.store(false, Ordering::Release);
                            return;
                        }
                        Err(_) => {
                            tracing::error!(epoch = owner.epoch, "traffic capture owner heartbeat timed out");
                            task_health.store(false, Ordering::Release);
                            return;
                        }
                    }
                }
            }
        });
        Self {
            healthy,
            shutdown,
            task: Some(task),
        }
    }

    pub(super) fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub(super) async fn stop(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl ReservedOwnerHeartbeat {
    pub(super) fn start(self, owner: OwnerToken) -> OwnerHeartbeat {
        OwnerHeartbeat::start(self.connection, owner)
    }
}

impl Drop for OwnerHeartbeat {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use sqlx::postgres::PgPoolOptions;

    async fn heartbeat_test_pools(max_connections: u32) -> (PgPool, PgPool, String) {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("capture_heartbeat_{}", Uuid::new_v4().simple());
        sqlx::raw_sql(&format!(
            r#"
            CREATE SCHEMA "{schema}";
            CREATE TABLE "{schema}"."TrafficCaptureOwnerState" (
              id SMALLINT PRIMARY KEY, owner_id UUID, owner_epoch BIGINT NOT NULL,
              heartbeat_at TIMESTAMPTZ, lease_expires_at TIMESTAMPTZ,
              draining BOOLEAN NOT NULL
            );
            CREATE TABLE "{schema}"."TrafficCaptureLiveEndpoints" (
              service_id INTEGER PRIMARY KEY, container_id TEXT NOT NULL,
              host TEXT NOT NULL, port INTEGER NOT NULL, owner_id UUID NOT NULL,
              owner_epoch BIGINT NOT NULL, acknowledged_at TIMESTAMPTZ NOT NULL
            );
            INSERT INTO "{schema}"."TrafficCaptureOwnerState"
              VALUES (1, NULL, 0, NULL, NULL, TRUE);
            "#
        ))
        .execute(&admin)
        .await
        .unwrap();

        let search_path = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .after_connect(move |connection, _| {
                let statement = format!(r#"SET search_path TO "{search_path}""#);
                Box::pin(async move {
                    sqlx::query(&statement).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap();
        (admin, pool, schema)
    }

    async fn observed_heartbeat(admin: &PgPool, schema: &str) -> DateTime<Utc> {
        sqlx::query_scalar(&format!(
            r#"SELECT heartbeat_at FROM "{schema}"."TrafficCaptureOwnerState" WHERE id = 1"#
        ))
        .fetch_one(admin)
        .await
        .unwrap()
    }

    async fn cleanup_heartbeat_test(admin: PgPool, pool: PgPool, schema: &str) {
        pool.close().await;
        sqlx::raw_sql(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }

    #[test]
    fn heartbeat_has_multiple_missed_ticks_before_expiry() {
        assert!(OWNER_HEARTBEAT_TIMEOUT < OWNER_HEARTBEAT_INTERVAL);
        assert!(OWNER_HEARTBEAT_INTERVAL * 3 < Duration::from_secs(OWNER_LEASE_SECONDS as u64));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn postgres_owner_epoch_fences_exact_live_publication() {
        use sqlx::{Connection, PgConnection};

        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let mut connection = PgConnection::connect(&database_url).await.unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TEMP TABLE "TrafficCaptureOwnerState" (
              id SMALLINT PRIMARY KEY, owner_id UUID, owner_epoch BIGINT NOT NULL,
              heartbeat_at TIMESTAMPTZ, lease_expires_at TIMESTAMPTZ,
              draining BOOLEAN NOT NULL
            );
            CREATE TEMP TABLE "TrafficCaptureLiveEndpoints" (
              service_id INTEGER PRIMARY KEY, container_id TEXT NOT NULL,
              host TEXT NOT NULL, port INTEGER NOT NULL, owner_id UUID NOT NULL,
              owner_epoch BIGINT NOT NULL, acknowledged_at TIMESTAMPTZ NOT NULL
            );
            INSERT INTO "TrafficCaptureOwnerState"
              VALUES (1, NULL, 0, NULL, NULL, TRUE);
            "#,
        )
        .execute(&mut connection)
        .await
        .unwrap();

        let owner = claim(&mut connection).await.unwrap();
        let spec = CaptureSpec {
            service_id: 7,
            container_id: "runtime-7".into(),
            host_text: "10.13.40.7".into(),
            host: "10.13.40.7".parse().unwrap(),
            port: 8080,
            challenge_id: 3,
            participation_id: 9,
        };
        assert!(
            publish_live(&mut connection, owner, std::slice::from_ref(&spec))
                .await
                .unwrap()
        );
        activate(&mut connection, owner).await.unwrap();
        let live: bool = sqlx::query_scalar(
            r#"SELECT EXISTS (
                 SELECT 1 FROM "TrafficCaptureLiveEndpoints" endpoint
                 JOIN "TrafficCaptureOwnerState" owner ON owner.id = 1
                WHERE endpoint.service_id = 7
                  AND endpoint.container_id = 'runtime-7'
                  AND endpoint.host = '10.13.40.7' AND endpoint.port = 8080
                  AND endpoint.owner_id = owner.owner_id
                  AND endpoint.owner_epoch = owner.owner_epoch
                  AND owner.draining = FALSE
                  AND owner.lease_expires_at > clock_timestamp()
               )"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap();
        assert!(live);

        sqlx::query(
            r#"UPDATE "TrafficCaptureOwnerState"
                  SET lease_expires_at = clock_timestamp() - interval '1 second'
                WHERE id = 1"#,
        )
        .execute(&mut connection)
        .await
        .unwrap();
        assert!(
            publish_live(&mut connection, owner, std::slice::from_ref(&spec))
                .await
                .is_err(),
            "an unchanged endpoint set must not let an expired owner publish"
        );

        fence_unowned(&mut connection).await.unwrap();
        assert!(!sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS (SELECT 1 FROM "TrafficCaptureLiveEndpoints")"#,
        )
        .fetch_one(&mut connection)
        .await
        .unwrap());
        assert!(publish_live(&mut connection, owner, &[spec]).await.is_err());
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn heartbeat_renews_on_its_reserved_connection_when_the_pool_is_full() {
        let (admin, pool, schema) = heartbeat_test_pools(3).await;
        let mut owner_connection = pool.acquire().await.unwrap();
        let heartbeat = OwnerHeartbeat::reserve(&pool).await.unwrap();
        let owner = claim(&mut owner_connection).await.unwrap();
        let blocker = pool.acquire().await.unwrap();
        assert_eq!(pool.size(), 3);
        assert_eq!(pool.num_idle(), 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), pool.acquire())
                .await
                .is_err(),
            "ordinary work must see the deliberately exhausted pool"
        );
        let contended_pool = pool.clone();
        let fixed_rate_load = tokio::spawn(async move {
            let mut arrivals = tokio::time::interval(Duration::from_millis(25));
            arrivals.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for _ in 0..160 {
                arrivals.tick().await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(20), contended_pool.acquire())
                        .await
                        .is_err(),
                    "fixed-rate ordinary checkout unexpectedly bypassed the saturated pool"
                );
            }
        });

        let heartbeat = heartbeat.start(owner);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let first = observed_heartbeat(&admin, &schema).await;
        let deadline = tokio::time::Instant::now() + OWNER_HEARTBEAT_INTERVAL * 3;
        let second = loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let observed = observed_heartbeat(&admin, &schema).await;
            if observed > first {
                break observed;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "reserved heartbeat did not advance while ordinary pool capacity was exhausted"
            );
        };
        assert!(second > first);
        assert!(heartbeat.is_healthy());
        let lease_live: bool = sqlx::query_scalar(&format!(
            r#"SELECT lease_expires_at > clock_timestamp()
                 FROM "{schema}"."TrafficCaptureOwnerState" WHERE id = 1"#
        ))
        .fetch_one(&admin)
        .await
        .unwrap();
        assert!(lease_live);

        fixed_rate_load.await.unwrap();
        heartbeat.stop().await;
        drop(blocker);
        release(&mut owner_connection, owner).await.unwrap();
        drop(owner_connection);
        cleanup_heartbeat_test(admin, pool, &schema).await;
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn heartbeat_fails_closed_and_discards_its_session_after_a_database_stall() {
        let (admin, pool, schema) = heartbeat_test_pools(2).await;
        let mut owner_connection = pool.acquire().await.unwrap();
        let mut reserved = OwnerHeartbeat::reserve(&pool).await.unwrap();
        let heartbeat_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *reserved.connection)
            .await
            .unwrap();
        let owner = claim(&mut owner_connection).await.unwrap();
        let heartbeat = reserved.start(owner);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let last_success = observed_heartbeat(&admin, &schema).await;

        let mut row_lock = admin.begin().await.unwrap();
        sqlx::query(&format!(
            r#"SELECT owner_epoch FROM "{schema}"."TrafficCaptureOwnerState"
                 WHERE id = 1 FOR UPDATE"#
        ))
        .fetch_one(&mut *row_lock)
        .await
        .unwrap();
        let wait_deadline =
            tokio::time::Instant::now() + OWNER_HEARTBEAT_INTERVAL + OWNER_HEARTBEAT_TIMEOUT;
        loop {
            let wait: Option<String> =
                sqlx::query_scalar("SELECT wait_event_type FROM pg_stat_activity WHERE pid = $1")
                    .bind(heartbeat_pid)
                    .fetch_optional(&admin)
                    .await
                    .unwrap()
                    .flatten();
            if wait.as_deref() == Some("Lock") {
                break;
            }
            assert!(
                tokio::time::Instant::now() < wait_deadline,
                "heartbeat never reached the injected PostgreSQL row lock"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let failure_deadline = tokio::time::Instant::now() + OWNER_HEARTBEAT_TIMEOUT * 2;
        while heartbeat.is_healthy() {
            assert!(
                tokio::time::Instant::now() < failure_deadline,
                "a real heartbeat database stall did not fail closed"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        heartbeat.stop().await;
        row_lock.rollback().await.unwrap();

        let backend_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)")
                    .bind(heartbeat_pid)
                    .fetch_one(&admin)
                    .await
                    .unwrap();
            if !exists {
                break;
            }
            assert!(
                tokio::time::Instant::now() < backend_deadline,
                "ambiguous heartbeat session was returned instead of closed"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let settled_heartbeat = observed_heartbeat(&admin, &schema).await;
        assert!(
            settled_heartbeat >= last_success,
            "PostgreSQL heartbeat time moved backwards"
        );
        tokio::time::sleep(OWNER_HEARTBEAT_INTERVAL + Duration::from_millis(200)).await;
        assert_eq!(
            observed_heartbeat(&admin, &schema).await,
            settled_heartbeat,
            "a failed heartbeat task must not retry and extend its lease"
        );
        let replacement = tokio::time::timeout(Duration::from_secs(2), pool.acquire())
            .await
            .expect("closed heartbeat session must replenish pool capacity")
            .unwrap();
        drop(replacement);

        release(&mut owner_connection, owner).await.unwrap();
        drop(owner_connection);
        cleanup_heartbeat_test(admin, pool, &schema).await;
    }
}
