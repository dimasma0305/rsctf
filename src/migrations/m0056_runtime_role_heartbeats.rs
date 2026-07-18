//! Lightweight PostgreSQL presence registry for split runtime roles.
//!
//! Heartbeats are deliberately advisory. Durable work ownership continues to
//! use its existing leases and fences; this table only lets readiness reject a
//! split topology whose required role has disappeared.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS "RuntimeRoleHeartbeats" (
        instance_id UUID PRIMARY KEY,
        role TEXT NOT NULL,
        started_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        heartbeat_at_utc TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        CONSTRAINT ck_runtime_role_heartbeats_role CHECK (
            role IN ('web', 'control', 'engine', 'network')
        )
    );

    CREATE INDEX IF NOT EXISTS ix_runtime_role_heartbeats_freshness_role
        ON "RuntimeRoleHeartbeats" (heartbeat_at_utc DESC, role);
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
            .execute_unprepared(r#"DROP TABLE IF EXISTS "RuntimeRoleHeartbeats";"#)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn creates_a_bounded_role_registry_idempotently() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"RuntimeRoleHeartbeats\""));
        assert!(UP_SQL.contains("instance_id UUID PRIMARY KEY"));
        assert!(UP_SQL.contains("'web', 'control', 'engine', 'network'"));
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS"));
        assert!(UP_SQL.contains("heartbeat_at_utc DESC, role"));
    }
}
