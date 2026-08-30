//! Cooperative cancellation metadata for durable control-plane jobs.

use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::prelude::*;

pub(crate) const UP_SQL: &str = r#"
ALTER TABLE "ControlPlaneJobs"
    ADD COLUMN IF NOT EXISTS cancel_requested_at_utc TIMESTAMPTZ NULL;

CREATE INDEX IF NOT EXISTS ix_control_plane_jobs_cancel_requested
    ON "ControlPlaneJobs" (updated_at_utc, id)
    WHERE status = 1 AND cancel_requested_at_utc IS NOT NULL;
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
        // Production migrations are forward-only. Retain cancellation intent
        // so rollback tooling cannot silently resume work an operator stopped.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;

    #[test]
    fn cancellation_is_forward_only_and_indexed() {
        assert!(UP_SQL.contains("ADD COLUMN IF NOT EXISTS cancel_requested_at_utc"));
        assert!(UP_SQL.contains("WHERE status = 1 AND cancel_requested_at_utc IS NOT NULL"));
    }
}
