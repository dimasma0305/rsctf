//! Durable reservation rows for local Docker aggregate admission.
//!
//! Every admission runs in one PostgreSQL transaction that first upserts the
//! per-host capacity row. The upsert takes that row's lock, so replicas that
//! share one Docker daemon serialize on it and can never both admit the last
//! slot. Reservations are keyed by the durable container operation identity,
//! which makes an exact retry an idempotent re-reservation.

use std::time::Duration;

use super::config::LocalCapacity;
use crate::utils::error::{AppError, AppResult};

/// Pre-launch rows older than this are abandoned owners (the create deadline
/// is 120 seconds) and no longer hold capacity.
const UNLAUNCHED_GRACE: &str = "5 minutes";
/// Launched rows must be older than this before an inventory miss releases
/// them, so a container attached after the inventory was listed survives.
const INVENTORY_GRACE: &str = "5 minutes";
const STALE_RELEASE_BATCH: i64 = 64;
const LOCK_TIMEOUT_SQLSTATE: &str = "55P03";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub key: String,
    pub cpu_millis: i64,
    pub memory_bytes: i64,
    pub slots: i32,
}

/// One labeled container from the runtime inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedRuntime {
    pub backend_id: String,
    pub operation_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct Usage {
    cpu_millis: i64,
    memory_bytes: i64,
    slots: i64,
}

fn database_error(error: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(database) = &error {
        if database.code().as_deref() == Some(LOCK_TIMEOUT_SQLSTATE) {
            return AppError::overloaded("Container capacity admission is busy", 1);
        }
    }
    AppError::internal(error.to_string())
}

fn admission_busy() -> AppError {
    AppError::overloaded("Container capacity admission is busy", 1)
}

/// Atomically account `request` against `capacity` for `host_key`.
///
/// A request that can never fit the configured ceiling is rejected as
/// unavailable without a retry hint; an exhausted host returns the retryable
/// overload response the container controllers already surface.
pub async fn reserve(
    pool: &sqlx::PgPool,
    host_key: &str,
    capacity: &LocalCapacity,
    request: &Reservation,
) -> AppResult<()> {
    if request.cpu_millis <= 0 || request.memory_bytes <= 0 || request.slots <= 0 {
        return Err(AppError::internal(
            "container capacity request must be positive",
        ));
    }
    if request.key.is_empty() || request.key.len() > 512 {
        return Err(AppError::internal(
            "container reservation key exceeds its durable bound",
        ));
    }
    if request.cpu_millis > capacity.cpu_millis
        || request.memory_bytes > capacity.memory_bytes
        || request.slots > capacity.slots
    {
        return Err(AppError::unavailable(
            "The challenge requests more CPU or memory than this container host allows",
        ));
    }
    let mut tx = match tokio::time::timeout(
        Duration::from_millis(250),
        crate::utils::database::begin_sqlx_transaction(pool),
    )
    .await
    {
        Ok(result) => result.map_err(database_error)?,
        Err(_) => return Err(admission_busy()),
    };
    sqlx::query("SET LOCAL lock_timeout = '2000ms'")
        .execute(&mut *tx)
        .await
        .map_err(database_error)?;
    // Serialize every admitter for this host on its capacity row and record
    // the ceiling this replica enforces.
    sqlx::query(
        r#"INSERT INTO "LocalContainerCapacityHosts" (host_key, cpu_millis, memory_bytes, slots)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (host_key) DO UPDATE
               SET cpu_millis = EXCLUDED.cpu_millis,
                   memory_bytes = EXCLUDED.memory_bytes,
                   slots = EXCLUDED.slots,
                   updated_at_utc = clock_timestamp()"#,
    )
    .bind(host_key)
    .bind(capacity.cpu_millis)
    .bind(capacity.memory_bytes)
    .bind(capacity.slots)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    release_unlaunched_stale(&mut tx, host_key).await?;
    let used = sqlx::query_as::<_, Usage>(
        r#"SELECT COALESCE(SUM(cpu_millis), 0)::BIGINT AS cpu_millis,
                  COALESCE(SUM(memory_bytes), 0)::BIGINT AS memory_bytes,
                  COALESCE(SUM(slots), 0)::BIGINT AS slots
             FROM "LocalContainerReservations"
            WHERE host_key = $1 AND reservation_key <> $2"#,
    )
    .bind(host_key)
    .bind(&request.key)
    .fetch_one(&mut *tx)
    .await
    .map_err(database_error)?;
    if used.cpu_millis.saturating_add(request.cpu_millis) > capacity.cpu_millis
        || used.memory_bytes.saturating_add(request.memory_bytes) > capacity.memory_bytes
        || used.slots.saturating_add(i64::from(request.slots)) > i64::from(capacity.slots)
    {
        tx.rollback().await.map_err(database_error)?;
        return Err(AppError::overloaded("Container host capacity is busy", 5));
    }
    sqlx::query(
        r#"INSERT INTO "LocalContainerReservations"
               (reservation_key, host_key, cpu_millis, memory_bytes, slots)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (reservation_key) DO UPDATE
               SET host_key = EXCLUDED.host_key,
                   cpu_millis = EXCLUDED.cpu_millis,
                   memory_bytes = EXCLUDED.memory_bytes,
                   slots = EXCLUDED.slots,
                   updated_at_utc = clock_timestamp()"#,
    )
    .bind(&request.key)
    .bind(host_key)
    .bind(request.cpu_millis)
    .bind(request.memory_bytes)
    .bind(request.slots)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    tx.commit().await.map_err(database_error)?;
    Ok(())
}

async fn release_unlaunched_stale(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    host_key: &str,
) -> AppResult<u64> {
    let sql = format!(
        r#"WITH stale AS (
               SELECT reservation_key
                 FROM "LocalContainerReservations"
                WHERE host_key = $1 AND backend_id IS NULL
                  AND updated_at_utc < clock_timestamp() - interval '{UNLAUNCHED_GRACE}'
                ORDER BY updated_at_utc, reservation_key
                LIMIT {STALE_RELEASE_BATCH}
           )
           DELETE FROM "LocalContainerReservations" reservation
            USING stale WHERE reservation.reservation_key = stale.reservation_key"#
    );
    let result = sqlx::query(&sql)
        .bind(host_key)
        .execute(&mut **tx)
        .await
        .map_err(database_error)?;
    Ok(result.rows_affected())
}

/// Record the runtime identity a reservation now holds.
pub async fn attach(
    pool: &sqlx::PgPool,
    host_key: &str,
    key: &str,
    backend_id: &str,
) -> AppResult<()> {
    if backend_id.is_empty() || backend_id.len() > 512 {
        return Err(AppError::internal(
            "container backend identity exceeds its durable bound",
        ));
    }
    sqlx::query(
        r#"UPDATE "LocalContainerReservations"
              SET backend_id = $3, updated_at_utc = clock_timestamp()
            WHERE host_key = $1 AND reservation_key = $2"#,
    )
    .bind(host_key)
    .bind(key)
    .bind(backend_id)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(database_error)
}

/// Release a reservation whose launch failed before a runtime identity existed.
pub async fn release_key(pool: &sqlx::PgPool, host_key: &str, key: &str) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "LocalContainerReservations"
            WHERE host_key = $1 AND reservation_key = $2"#,
    )
    .bind(host_key)
    .bind(key)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(database_error)
}

/// Release every reservation attached to a removed runtime.
pub async fn release_backend(
    pool: &sqlx::PgPool,
    host_key: &str,
    backend_id: &str,
) -> AppResult<()> {
    sqlx::query(
        r#"DELETE FROM "LocalContainerReservations"
            WHERE host_key = $1 AND backend_id = $2"#,
    )
    .bind(host_key)
    .bind(backend_id)
    .execute(pool)
    .await
    .map(|_| ())
    .map_err(database_error)
}

/// Reconcile reservations against the complete labeled runtime inventory.
///
/// A pre-launch row whose operation label is live adopts that runtime (the
/// owner crashed between create and attach). A launched row whose runtime is
/// absent after the grace window is released. Abandoned pre-launch rows age
/// out. Returns the number of rows released.
pub async fn reconcile(
    pool: &sqlx::PgPool,
    host_key: &str,
    inventory: &[ManagedRuntime],
) -> AppResult<u64> {
    let live_ids: Vec<&str> = inventory
        .iter()
        .map(|runtime| runtime.backend_id.as_str())
        .collect();
    let (operation_ids, operation_backends): (Vec<&str>, Vec<&str>) = inventory
        .iter()
        .filter_map(|runtime| {
            runtime
                .operation_id
                .as_deref()
                .map(|operation| (operation, runtime.backend_id.as_str()))
        })
        .unzip();
    let mut tx = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(database_error)?;
    sqlx::query(
        r#"UPDATE "LocalContainerReservations" reservation
              SET backend_id = live.backend_id, updated_at_utc = clock_timestamp()
             FROM unnest($2::TEXT[], $3::TEXT[]) AS live(operation_id, backend_id)
            WHERE reservation.host_key = $1 AND reservation.backend_id IS NULL
              AND reservation.reservation_key = live.operation_id"#,
    )
    .bind(host_key)
    .bind(&operation_ids)
    .bind(&operation_backends)
    .execute(&mut *tx)
    .await
    .map_err(database_error)?;
    let vanished_sql = format!(
        r#"DELETE FROM "LocalContainerReservations"
            WHERE host_key = $1 AND backend_id IS NOT NULL
              AND updated_at_utc < clock_timestamp() - interval '{INVENTORY_GRACE}'
              AND backend_id <> ALL($2::TEXT[])"#
    );
    let vanished = sqlx::query(&vanished_sql)
        .bind(host_key)
        .bind(&live_ids)
        .execute(&mut *tx)
        .await
        .map_err(database_error)?
        .rows_affected();
    let abandoned = release_unlaunched_stale(&mut tx, host_key).await?;
    tx.commit().await.map_err(database_error)?;
    Ok(vanished + abandoned)
}

/// Current accounted usage for `host_key`.
#[cfg(test)]
pub(super) async fn usage(pool: &sqlx::PgPool, host_key: &str) -> AppResult<(i64, i64, i64)> {
    let used = sqlx::query_as::<_, Usage>(
        r#"SELECT COALESCE(SUM(cpu_millis), 0)::BIGINT AS cpu_millis,
                  COALESCE(SUM(memory_bytes), 0)::BIGINT AS memory_bytes,
                  COALESCE(SUM(slots), 0)::BIGINT AS slots
             FROM "LocalContainerReservations" WHERE host_key = $1"#,
    )
    .bind(host_key)
    .fetch_one(pool)
    .await
    .map_err(database_error)?;
    Ok((used.cpu_millis, used.memory_bytes, used.slots))
}
