//! Durable generation fence for cross-replica traffic-capture reconciliation.
//!
//! Mutating replicas advance `requested_generation` only after PostgreSQL holds
//! the new desired container state. The singleton capture owner advances
//! `applied_generation` only after obsolete libpcap threads have stopped and the
//! desired replacement set has been started.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS "TrafficCaptureReconcileState" (
        id SMALLINT PRIMARY KEY,
        requested_generation BIGINT NOT NULL DEFAULT 0,
        applied_generation BIGINT NOT NULL DEFAULT 0,
        requested_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        applied_at TIMESTAMPTZ NULL,
        CONSTRAINT ck_trafficcapturereconcile_singleton CHECK (id = 1),
        CONSTRAINT ck_trafficcapturereconcile_generations CHECK (
            requested_generation >= 0
            AND applied_generation >= 0
            AND applied_generation <= requested_generation
        )
    );

    INSERT INTO "TrafficCaptureReconcileState"
        (id, requested_generation, applied_generation, requested_at, applied_at)
    VALUES (1, 0, 0, clock_timestamp(), NULL)
    ON CONFLICT (id) DO NOTHING;

    CREATE INDEX IF NOT EXISTS ix_adteamservices_capture_container
        ON "AdTeamServices" (container_id)
        WHERE container_id IS NOT NULL;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP_SQL).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DROP INDEX IF EXISTS ix_adteamservices_capture_container;
                DROP TABLE IF EXISTS "TrafficCaptureReconcileState";
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn creates_idempotent_monotonic_capture_cursor() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"TrafficCaptureReconcileState\""));
        assert!(UP_SQL.contains("CHECK (id = 1)"));
        assert!(UP_SQL.contains("applied_generation <= requested_generation"));
        assert!(UP_SQL.contains("ON CONFLICT (id) DO NOTHING"));
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS ix_adteamservices_capture_container"));
        assert!(UP_SQL.contains("WHERE container_id IS NOT NULL"));
    }
}
