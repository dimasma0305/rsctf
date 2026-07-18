//! Durable capture-owner lease and exact live-endpoint publication.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::{Acquire, PgConnection, PgPool};
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

async fn renew(pool: &PgPool, owner: OwnerToken) -> Result<bool, sqlx::Error> {
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
    .execute(pool)
    .await?
    .rows_affected()
        == 1)
}

pub(super) struct OwnerHeartbeat {
    healthy: Arc<AtomicBool>,
    shutdown: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl OwnerHeartbeat {
    pub(super) fn start(pool: PgPool, owner: OwnerToken) -> Self {
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
                        renew(&pool, owner),
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
}
