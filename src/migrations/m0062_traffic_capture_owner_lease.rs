//! Expiring traffic-capture ownership and exact live-endpoint publication.
//!
//! Capture-required VPN routes are admitted only when the current owner has
//! acknowledged the exact service identity and its lease is still healthy.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS "TrafficCaptureOwnerState" (
        id SMALLINT PRIMARY KEY,
        owner_id UUID NULL,
        owner_epoch BIGINT NOT NULL DEFAULT 0,
        heartbeat_at TIMESTAMPTZ NULL,
        lease_expires_at TIMESTAMPTZ NULL,
        draining BOOLEAN NOT NULL DEFAULT TRUE,
        CONSTRAINT ck_trafficcaptureowner_singleton CHECK (id = 1),
        CONSTRAINT ck_trafficcaptureowner_epoch CHECK (owner_epoch >= 0),
        CONSTRAINT ck_trafficcaptureowner_lease CHECK (
            (owner_id IS NULL AND lease_expires_at IS NULL AND draining = TRUE)
            OR (owner_id IS NOT NULL AND lease_expires_at IS NOT NULL)
        )
    );

    INSERT INTO "TrafficCaptureOwnerState"
        (id, owner_id, owner_epoch, heartbeat_at, lease_expires_at, draining)
    VALUES (1, NULL, 0, NULL, NULL, TRUE)
    ON CONFLICT (id) DO NOTHING;

    CREATE TABLE IF NOT EXISTS "TrafficCaptureLiveEndpoints" (
        service_id INTEGER PRIMARY KEY
            REFERENCES "AdTeamServices" (id) ON DELETE CASCADE,
        container_id TEXT NOT NULL,
        host TEXT NOT NULL,
        port INTEGER NOT NULL,
        owner_id UUID NOT NULL,
        owner_epoch BIGINT NOT NULL,
        acknowledged_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        CONSTRAINT ck_trafficcapturelive_port CHECK (port BETWEEN 1 AND 65535),
        CONSTRAINT ck_trafficcapturelive_epoch CHECK (owner_epoch > 0)
    );

    CREATE INDEX IF NOT EXISTS ix_trafficcapturelive_owner
        ON "TrafficCaptureLiveEndpoints" (owner_id, owner_epoch);
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
                DROP INDEX IF EXISTS ix_trafficcapturelive_owner;
                DROP TABLE IF EXISTS "TrafficCaptureLiveEndpoints";
                DROP TABLE IF EXISTS "TrafficCaptureOwnerState";
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
    fn owner_lease_and_exact_endpoint_ack_are_idempotent() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"TrafficCaptureOwnerState\""));
        assert!(UP_SQL.contains("lease_expires_at TIMESTAMPTZ NULL"));
        assert!(UP_SQL.contains("draining BOOLEAN NOT NULL DEFAULT TRUE"));
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"TrafficCaptureLiveEndpoints\""));
        assert!(UP_SQL.contains("REFERENCES \"AdTeamServices\" (id) ON DELETE CASCADE"));
        assert!(UP_SQL.contains("owner_epoch BIGINT NOT NULL"));
        assert!(UP_SQL.contains("ON CONFLICT (id) DO NOTHING"));
    }
}
