use sea_orm_migration::prelude::*;

/// Durable aggregate admission for the local Docker backend.
///
/// `LocalContainerCapacityHosts` holds one row per Docker installation scope.
/// Every admission upserts that row inside its transaction, which takes the
/// row lock and serializes replicas that share one daemon. The row also
/// records the capacity the most recently admitting replica was configured
/// with, so operators can inspect what the fleet is enforcing.
///
/// `LocalContainerReservations` holds one row per admitted workload, keyed by
/// the durable container operation identity so an exact retry re-reserves
/// instead of double counting. `backend_id` is attached once the runtime
/// returns the container identity and is how removal releases the row.
pub(crate) const UP_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS "LocalContainerCapacityHosts" (
    host_key TEXT PRIMARY KEY,
    cpu_millis BIGINT NOT NULL,
    memory_bytes BIGINT NOT NULL,
    slots INTEGER NOT NULL,
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (octet_length(host_key) BETWEEN 1 AND 128),
    CHECK (cpu_millis > 0 AND memory_bytes > 0 AND slots > 0)
);

CREATE TABLE IF NOT EXISTS "LocalContainerReservations" (
    reservation_key TEXT PRIMARY KEY,
    host_key TEXT NOT NULL,
    cpu_millis BIGINT NOT NULL,
    memory_bytes BIGINT NOT NULL,
    slots INTEGER NOT NULL,
    backend_id TEXT NULL,
    created_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK (octet_length(reservation_key) BETWEEN 1 AND 512),
    CHECK (octet_length(host_key) BETWEEN 1 AND 128),
    CHECK (cpu_millis > 0 AND memory_bytes > 0 AND slots > 0),
    CHECK (backend_id IS NULL OR octet_length(backend_id) BETWEEN 1 AND 512)
);
CREATE INDEX IF NOT EXISTS ix_local_container_reservations_host_backend
    ON "LocalContainerReservations" (host_key, backend_id);
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn local_capacity_tables_are_idempotent_and_keyed_by_operation() {
        assert!(UP_SQL.contains(r#"CREATE TABLE IF NOT EXISTS "LocalContainerCapacityHosts""#));
        assert!(UP_SQL.contains("host_key TEXT PRIMARY KEY"));
        assert!(UP_SQL.contains(r#"CREATE TABLE IF NOT EXISTS "LocalContainerReservations""#));
        assert!(UP_SQL.contains("reservation_key TEXT PRIMARY KEY"));
        assert!(UP_SQL.contains("backend_id TEXT NULL"));
        assert!(UP_SQL.contains("ix_local_container_reservations_host_backend"));
        assert!(UP_SQL.contains("cpu_millis > 0 AND memory_bytes > 0 AND slots > 0"));
    }
}
