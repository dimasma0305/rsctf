use sea_orm::DatabaseConnection;

use crate::utils::error::{AppError, AppResult};

const VPN_INSTANCE_LOCK: i64 = 0x5253_4354_4656_504e;
static OWNS_INSTANCE_LEASE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static INSTANCE_LEASE_RELEASING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static INSTANCE_LEASE: std::sync::LazyLock<
    tokio::sync::Mutex<Option<sqlx::pool::PoolConnection<sqlx::Postgres>>>,
> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(None));

/// The in-process WireGuard interface, iptables policy, and BYOC registry are
/// singleton state. Hold a database session lock for the process lifetime so a
/// second network owner fails startup instead of serving divergent policy. BYOC
/// needs this ownership even when the optional WireGuard VPN is disabled.
pub async fn acquire_instance_lease(db: &DatabaseConnection) -> AppResult<()> {
    INSTANCE_LEASE_RELEASING.store(false, std::sync::atomic::Ordering::Release);
    let mut slot = INSTANCE_LEASE.lock().await;
    if slot.is_some() {
        return Ok(());
    }
    let mut connection = db
        .get_postgres_connection_pool()
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // From this point onward the server may grant a session advisory lock. If
    // this future is cancelled, or the query reaches PostgreSQL but its reply
    // is lost, returning the socket to the pool could strand an invisible lock
    // in an otherwise healthy pooled session. This dedicated lifetime lease is
    // cheap to close on every exit (including a clean explicit unlock).
    connection.close_on_drop();
    let acquired: bool = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(VPN_INSTANCE_LOCK)
            .fetch_one(&mut *connection),
    )
    .await
    .map_err(|_| AppError::unavailable("network singleton lease attempt timed out"))?
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !acquired {
        return Err(AppError::internal(
            "another rsctf replica already owns the singleton network/BYOC lease",
        ));
    }
    *slot = Some(connection);
    OWNS_INSTANCE_LEASE.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// Release the process-wide network ownership session after network workers
/// have stopped. The dedicated checked-out connection closes on every exit;
/// it is never returned to the pool with ambiguous advisory-lock state.
pub async fn release_instance_lease() -> AppResult<()> {
    // Set this before waiting on the slot lock. If the monitor is concurrently
    // probing a connection which shutdown has made unhealthy, it must return
    // normally instead of racing intentional release with process::exit(1).
    INSTANCE_LEASE_RELEASING.store(true, std::sync::atomic::Ordering::Release);
    let mut connection = INSTANCE_LEASE.lock().await.take();
    OWNS_INSTANCE_LEASE.store(false, std::sync::atomic::Ordering::Release);
    let Some(mut connection) = connection.take() else {
        return Ok(());
    };

    let unlocked = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(VPN_INSTANCE_LOCK)
            .fetch_one(&mut *connection),
    )
    .await;
    match unlocked {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => {
            connection.close_on_drop();
            Err(AppError::internal(
                "network singleton lease was not held by its owner session",
            ))
        }
        Ok(Err(error)) => {
            connection.close_on_drop();
            Err(AppError::internal(format!(
                "network singleton lease release failed: {error}"
            )))
        }
        Err(_) => {
            connection.close_on_drop();
            Err(AppError::internal(
                "network singleton lease release timed out",
            ))
        }
    }
}

/// Whether this process owns the kernel-local VPN/BYOC capability.
///
/// HTTP and engine-only replicas may share the same deployment configuration
/// without being allowed to touch another process's WireGuard namespace.
pub fn owns_instance_lease() -> bool {
    OWNS_INSTANCE_LEASE.load(std::sync::atomic::Ordering::Acquire)
}

pub fn start_instance_lease_monitor() {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            let mut slot = INSTANCE_LEASE.lock().await;
            let healthy = if let Some(connection) = slot.as_mut() {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&mut **connection),
                )
                .await
                .is_ok_and(|result| result.is_ok())
            } else {
                false
            };
            if !healthy {
                OWNS_INSTANCE_LEASE.store(false, std::sync::atomic::Ordering::Release);
                if INSTANCE_LEASE_RELEASING.load(std::sync::atomic::Ordering::Acquire) {
                    tracing::info!("network singleton lease monitor stopped after release");
                    return;
                }
                tracing::error!("network singleton lease was lost; terminating fail-closed");
                std::process::exit(1);
            }
        }
    });
}
