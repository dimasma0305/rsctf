//! Separate per-workload network-slot reservations from per-network replica
//! capacity while preserving the replica totals stored by revision 1.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

pub(super) const UP_SQL: &str = r#"
    LOCK TABLE "WorkerWorkloads" IN ACCESS EXCLUSIVE MODE;

    UPDATE "WorkerNodes"
       SET capabilities = jsonb_set(
           capabilities, '{maxWorkloadReplicas}', '0'::jsonb, TRUE
       )
     WHERE NOT capabilities ? 'maxWorkloadReplicas';

    ALTER TABLE "WorkerWorkloads"
        ADD COLUMN IF NOT EXISTS required_replicas INTEGER NULL;

    UPDATE "WorkerWorkloads"
       SET required_replicas = reserved_slots
     WHERE required_replicas IS NULL;

    UPDATE "WorkerWorkloads"
       SET reserved_slots = 1
     WHERE reserved_slots <> 1;

    ALTER TABLE "WorkerWorkloads"
        ALTER COLUMN required_replicas SET NOT NULL;

    DO $$
    BEGIN
        IF NOT EXISTS (
            SELECT 1
              FROM pg_constraint
             WHERE conrelid = '"WorkerWorkloads"'::regclass
               AND conname = 'ck_workerworkloads_required_replicas'
        ) THEN
            ALTER TABLE "WorkerWorkloads"
                ADD CONSTRAINT ck_workerworkloads_required_replicas
                CHECK (required_replicas BETWEEN 1 AND 512) NOT VALID;
        END IF;
        IF NOT EXISTS (
            SELECT 1
              FROM pg_constraint
             WHERE conrelid = '"WorkerWorkloads"'::regclass
               AND conname = 'ck_workerworkloads_single_slot'
        ) THEN
            ALTER TABLE "WorkerWorkloads"
                ADD CONSTRAINT ck_workerworkloads_single_slot
                CHECK (reserved_slots = 1) NOT VALID;
        END IF;
    END
    $$;

    ALTER TABLE "WorkerWorkloads"
        VALIDATE CONSTRAINT ck_workerworkloads_required_replicas;
    ALTER TABLE "WorkerWorkloads"
        VALIDATE CONSTRAINT ck_workerworkloads_single_slot;
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
                LOCK TABLE "WorkerWorkloads" IN ACCESS EXCLUSIVE MODE;
                ALTER TABLE "WorkerWorkloads"
                    DROP CONSTRAINT IF EXISTS ck_workerworkloads_single_slot;
                UPDATE "WorkerWorkloads"
                   SET reserved_slots = required_replicas;
                ALTER TABLE "WorkerWorkloads"
                    DROP CONSTRAINT IF EXISTS ck_workerworkloads_required_replicas;
                ALTER TABLE "WorkerWorkloads"
                    DROP COLUMN IF EXISTS required_replicas;
                "#,
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::UP_SQL;
    use rsctf_worker_protocol::MAX_WORKLOAD_REPLICAS;

    #[test]
    fn backfills_replica_dimension_before_normalizing_workload_slots() {
        let backfill = UP_SQL
            .find("SET required_replicas = reserved_slots")
            .unwrap();
        let normalize = UP_SQL.find("SET reserved_slots = 1").unwrap();
        assert!(backfill < normalize);
        assert!(UP_SQL.contains("CHECK (reserved_slots = 1)"));
        assert!(UP_SQL.contains("'{maxWorkloadReplicas}', '0'::jsonb"));
        assert!(UP_SQL.contains(&format!(
            "CHECK (required_replicas BETWEEN 1 AND {MAX_WORKLOAD_REPLICAS})"
        )));
    }
}
