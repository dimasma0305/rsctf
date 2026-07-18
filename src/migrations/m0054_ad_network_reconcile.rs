//! Durable generation fence for cross-replica A&D network reconciliation.
//!
//! PostgreSQL is authoritative: API/engine replicas advance the requested
//! generation only after their policy mutation commits, while the singleton
//! network owner advances the applied generation only after kernel activation
//! succeeds. A singleton row is enough because wg0 and its firewall policy are
//! deployment-wide state.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS "AdNetworkReconcileState" (
        id SMALLINT PRIMARY KEY,
        requested_generation BIGINT NOT NULL DEFAULT 0,
        applied_generation BIGINT NOT NULL DEFAULT 0,
        requested_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
        applied_at TIMESTAMPTZ NULL,
        CONSTRAINT ck_adnetworkreconcile_singleton CHECK (id = 1),
        CONSTRAINT ck_adnetworkreconcile_generations CHECK (
            requested_generation >= 0
            AND applied_generation >= 0
            AND applied_generation <= requested_generation
        )
    );

    INSERT INTO "AdNetworkReconcileState"
        (id, requested_generation, applied_generation, requested_at, applied_at)
    VALUES (1, 0, 0, clock_timestamp(), NULL)
    ON CONFLICT (id) DO NOTHING;
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
            .execute_unprepared(r#"DROP TABLE IF EXISTS "AdNetworkReconcileState";"#)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn creates_one_monotonic_reconcile_cursor_idempotently() {
        assert!(UP_SQL.contains("CREATE TABLE IF NOT EXISTS \"AdNetworkReconcileState\""));
        assert!(UP_SQL.contains("CHECK (id = 1)"));
        assert!(UP_SQL.contains("applied_generation <= requested_generation"));
        assert!(UP_SQL.contains("ON CONFLICT (id) DO NOTHING"));
    }
}
