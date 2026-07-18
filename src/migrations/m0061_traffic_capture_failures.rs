//! Durable post-start traffic-capture failures and revocation progress.
//!
//! A libpcap thread can fail after its startup request was acknowledged. The
//! failure row and the exact endpoint deactivation are committed together;
//! network-policy revocation is then retried until it is acknowledged.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS "TrafficCaptureFailures" (
        id BIGSERIAL PRIMARY KEY,
        service_id INTEGER NOT NULL,
        container_id TEXT NOT NULL,
        host TEXT NOT NULL,
        port INTEGER NOT NULL,
        challenge_id INTEGER NOT NULL,
        participation_id INTEGER NOT NULL,
        detected_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        error TEXT NOT NULL,
        endpoint_was_current BOOLEAN NOT NULL,
        endpoint_deactivated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        network_revoked_at TIMESTAMPTZ NULL,
        last_reconcile_error TEXT NULL,
        CONSTRAINT ck_trafficcapturefailure_port CHECK (port BETWEEN 1 AND 65535)
    );

    CREATE UNIQUE INDEX IF NOT EXISTS ux_trafficcapturefailures_pending
        ON "TrafficCaptureFailures" (service_id, container_id)
        WHERE network_revoked_at IS NULL;

    CREATE INDEX IF NOT EXISTS ix_trafficcapturefailures_retention
        ON "TrafficCaptureFailures" (network_revoked_at)
        WHERE network_revoked_at IS NOT NULL;
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
                DROP INDEX IF EXISTS ix_trafficcapturefailures_retention;
                DROP INDEX IF EXISTS ux_trafficcapturefailures_pending;
                DROP TABLE IF EXISTS "TrafficCaptureFailures";
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
    fn failure_and_revocation_state_is_idempotent_and_bounded() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"TrafficCaptureFailures\""));
        assert!(UP_SQL.contains("endpoint_was_current BOOLEAN NOT NULL"));
        assert!(UP_SQL.contains("network_revoked_at TIMESTAMPTZ NULL"));
        assert!(UP_SQL.contains("WHERE network_revoked_at IS NULL"));
        assert!(UP_SQL.contains("WHERE network_revoked_at IS NOT NULL"));
    }
}
