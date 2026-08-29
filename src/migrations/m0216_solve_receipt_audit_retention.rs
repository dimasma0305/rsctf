//! Keep consumed solve-receipt provenance on a bounded, indexed retention path.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

pub(crate) const UP_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS ix_solve_receipt_audit_retention
    ON "SolveReceiptAudit" (consumed_at_utc, receipt_id);
"#;

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
    fn audit_retention_is_idempotent_and_cursor_indexed() {
        assert!(UP_SQL.contains("CREATE INDEX IF NOT EXISTS"));
        assert!(UP_SQL.contains("consumed_at_utc, receipt_id"));
    }
}
