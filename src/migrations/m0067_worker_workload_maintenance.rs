//! Keep global reconciliation and terminal workload retention bounded as the
//! worker history grows.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP_SQL: &str = r#"
    CREATE INDEX IF NOT EXISTS ix_workerworkloads_global_reconcile_due
        ON "WorkerWorkloads" (updated_at, id)
        INCLUDE (
            worker_id, assignment_id, generation, desired_state,
            observed_state, observed_session_epoch
        )
        WHERE (desired_state = 'Present' AND observed_state <> 'Ready')
           OR (desired_state = 'Absent' AND observed_state <> 'Absent');

    CREATE INDEX IF NOT EXISTS ix_workerworkloads_terminal_retention
        ON "WorkerWorkloads" (updated_at, id)
        WHERE desired_state = 'Absent' AND observed_state = 'Absent';

    CREATE INDEX IF NOT EXISTS ix_containers_worker_handle_workload
        ON "Containers" ((SPLIT_PART(container_id, ':', 2)))
        WHERE container_id LIKE 'rsctf-worker:%';
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
                DROP INDEX IF EXISTS ix_containers_worker_handle_workload;
                DROP INDEX IF EXISTS ix_workerworkloads_terminal_retention;
                DROP INDEX IF EXISTS ix_workerworkloads_global_reconcile_due;
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
    fn indexes_global_due_work_and_bounded_terminal_cleanup() {
        assert!(UP_SQL.contains("ix_workerworkloads_global_reconcile_due"));
        assert!(UP_SQL.contains("ON \"WorkerWorkloads\" (updated_at, id)"));
        assert!(UP_SQL.contains("ix_workerworkloads_terminal_retention"));
        assert!(UP_SQL.contains("desired_state = 'Present' AND observed_state <> 'Ready'"));
        assert!(UP_SQL.contains("desired_state = 'Absent' AND observed_state = 'Absent'"));
        assert!(UP_SQL.contains("ix_containers_worker_handle_workload"));
    }
}
