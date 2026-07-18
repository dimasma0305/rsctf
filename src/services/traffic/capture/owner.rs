//! Cancellation-safe PostgreSQL advisory ownership for libpcap threads.

use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};

/// `RSCTFCAP` as an i64. This lock is independent from the optional VPN lease,
/// because capture ownership is required even when the integrated VPN is off.
const CAPTURE_OWNER_LOCK: i64 = 0x5253_4354_4643_4150;
const LEASE_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) async fn try_acquire(pool: &PgPool) -> Result<Option<OwnerLease>, String> {
    let connection = tokio::time::timeout(LEASE_ACQUIRE_TIMEOUT, pool.acquire())
        .await
        .map_err(|_| "capture owner connection acquisition timed out".to_string())?
        .map_err(|error| error.to_string())?;
    let mut lease = OwnerLease::new(connection);
    let acquired = tokio::time::timeout(
        LEASE_ACQUIRE_TIMEOUT,
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
            .bind(CAPTURE_OWNER_LOCK)
            .fetch_one(&mut **lease.connection_mut()),
    )
    .await;
    match acquired {
        Ok(Ok(true)) => Ok(Some(lease)),
        Ok(Ok(false)) => {
            lease.return_to_pool();
            Ok(None)
        }
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("capture owner advisory-lock attempt timed out".to_string()),
    }
}

pub(super) async fn release(mut lease: OwnerLease) -> Result<(), String> {
    let released = tokio::time::timeout(
        LEASE_ACQUIRE_TIMEOUT,
        sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(CAPTURE_OWNER_LOCK)
            .fetch_one(&mut **lease.connection_mut()),
    )
    .await;
    match released {
        Ok(Ok(true)) => {
            lease.return_to_pool();
            Ok(())
        }
        Ok(Ok(false)) => Err("capture owner session did not hold its advisory lock".to_string()),
        Ok(Err(error)) => Err(format!("capture owner unlock failed: {error}")),
        Err(_) => Err("capture owner unlock timed out".to_string()),
    }
}

/// Until an explicit successful unlock, dropping the lease closes the physical
/// connection so a session lock can never return to sqlx's pool by cancellation.
pub(super) struct OwnerLease {
    connection: Option<PoolConnection<Postgres>>,
}

impl OwnerLease {
    fn new(connection: PoolConnection<Postgres>) -> Self {
        Self {
            connection: Some(connection),
        }
    }

    pub(super) fn connection_mut(&mut self) -> &mut PoolConnection<Postgres> {
        self.connection
            .as_mut()
            .expect("capture owner connection exists until explicit release")
    }

    fn return_to_pool(mut self) {
        drop(self.connection.take());
    }
}

impl Drop for OwnerLease {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_mut() {
            connection.close_on_drop();
        }
    }
}
