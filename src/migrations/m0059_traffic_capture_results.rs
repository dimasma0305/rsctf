//! Per-request outcomes for durable traffic-capture reconciliation.
//!
//! A global generation proves that the owner ran a pass, but cannot tell one
//! caller whether its particular libpcap device/filter/savefile opened. These
//! bounded result rows make startup fail closed without coupling an unrelated
//! teardown to another service's capture failure.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS "TrafficCaptureReconcileRequests" (
        generation BIGINT PRIMARY KEY,
        container_id TEXT NOT NULL,
        action TEXT NOT NULL,
        requested_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        applied_at TIMESTAMPTZ NULL,
        succeeded BOOLEAN NULL,
        error TEXT NULL,
        CONSTRAINT ck_trafficcapturerequest_generation CHECK (generation > 0),
        CONSTRAINT ck_trafficcapturerequest_action CHECK (action IN ('Start', 'Stop')),
        CONSTRAINT ck_trafficcapturerequest_result CHECK (
            (applied_at IS NULL AND succeeded IS NULL AND error IS NULL)
            OR (applied_at IS NOT NULL AND succeeded IS NOT NULL)
        )
    );

    CREATE INDEX IF NOT EXISTS ix_trafficcapturerequests_pending
        ON "TrafficCaptureReconcileRequests" (generation)
        WHERE applied_at IS NULL;

    CREATE INDEX IF NOT EXISTS ix_trafficcapturerequests_cleanup
        ON "TrafficCaptureReconcileRequests" (applied_at)
        WHERE applied_at IS NOT NULL;
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
                DROP INDEX IF EXISTS ix_trafficcapturerequests_cleanup;
                DROP INDEX IF EXISTS ix_trafficcapturerequests_pending;
                DROP TABLE IF EXISTS "TrafficCaptureReconcileRequests";
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
    fn creates_bounded_per_generation_results() {
        assert!(UP_SQL.contains("generation BIGINT PRIMARY KEY"));
        assert!(UP_SQL.contains("action IN ('Start', 'Stop')"));
        assert!(UP_SQL.contains("applied_at IS NULL"));
        assert!(UP_SQL.contains("ix_trafficcapturerequests_cleanup"));
    }
}
