//! Fence damaged durable worker definitions out of the reconcile queue while
//! preserving their assignment, reservation, and bounded operator evidence.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(crate) const UP_SQL: &str = r#"
    ALTER TABLE "WorkerWorkloads"
        ADD COLUMN IF NOT EXISTS reconcile_quarantine_generation BIGINT NULL,
        ADD COLUMN IF NOT EXISTS reconcile_quarantined_at TIMESTAMPTZ NULL,
        ADD COLUMN IF NOT EXISTS reconcile_quarantine_message TEXT NULL;

    DO $$
    BEGIN
        IF NOT EXISTS (
            SELECT 1
              FROM pg_constraint
             WHERE conrelid = '"WorkerWorkloads"'::regclass
               AND conname = 'ck_workerworkloads_reconcile_quarantine'
        ) THEN
            ALTER TABLE "WorkerWorkloads"
                ADD CONSTRAINT ck_workerworkloads_reconcile_quarantine CHECK (
                    (
                        reconcile_quarantine_generation IS NULL
                        AND reconcile_quarantined_at IS NULL
                        AND reconcile_quarantine_message IS NULL
                    )
                    OR (
                        reconcile_quarantine_generation >= 1
                        AND reconcile_quarantined_at IS NOT NULL
                        AND reconcile_quarantine_message IS NOT NULL
                        AND OCTET_LENGTH(reconcile_quarantine_message) BETWEEN 1 AND 1024
                    )
                );
        END IF;
    END $$;

    CREATE INDEX IF NOT EXISTS ix_workerworkloads_reconcile_unquarantined
        ON "WorkerWorkloads" (updated_at, id)
        INCLUDE (
            worker_id, assignment_id, generation, desired_state,
            observed_state, observed_session_epoch
        )
        WHERE reconcile_quarantine_generation IS DISTINCT FROM generation
          AND (
              (desired_state = 'Present' AND observed_state <> 'Ready')
              OR (desired_state = 'Absent' AND observed_state <> 'Absent')
          );
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
                DROP INDEX IF EXISTS ix_workerworkloads_reconcile_unquarantined;
                ALTER TABLE "WorkerWorkloads"
                    DROP CONSTRAINT IF EXISTS ck_workerworkloads_reconcile_quarantine,
                    DROP COLUMN IF EXISTS reconcile_quarantine_message,
                    DROP COLUMN IF EXISTS reconcile_quarantined_at,
                    DROP COLUMN IF EXISTS reconcile_quarantine_generation;
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
    fn migration_keeps_quarantine_bounded_and_out_of_the_due_index() {
        assert!(UP_SQL.contains("reconcile_quarantine_generation"));
        assert!(UP_SQL.contains("OCTET_LENGTH(reconcile_quarantine_message) BETWEEN 1 AND 1024"));
        assert!(UP_SQL.contains("reconcile_quarantine_generation IS DISTINCT FROM generation"));
        assert!(UP_SQL.contains("ix_workerworkloads_reconcile_unquarantined"));
    }
}
