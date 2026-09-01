use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{database_error, WorkerStore, WorkerStoreError};

impl WorkerStore {
    pub async fn delete_expired_enrollment_operations(
        &self,
        completed_before: DateTime<Utc>,
        limit: i64,
    ) -> Result<u64, WorkerStoreError> {
        if !(1..=1_000).contains(&limit) {
            return Err(WorkerStoreError::InvalidInput(
                "enrollment maintenance limit must be between 1 and 1000".to_owned(),
            ));
        }
        let result = sqlx::query(
            r#"WITH expired AS (
                   SELECT operation_id FROM "WorkerEnrollmentOperations"
                    WHERE (state = 'Completed' AND completed_at <= $1)
                       OR (state <> 'Completed' AND claim_expires_at <= $1)
                 ORDER BY COALESCE(completed_at, claim_expires_at), operation_id
                    FOR UPDATE SKIP LOCKED LIMIT $2
               )
               DELETE FROM "WorkerEnrollmentOperations" operation
                USING expired WHERE operation.operation_id = expired.operation_id"#,
        )
        .bind(completed_before)
        .bind(limit)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(result.rows_affected())
    }

    /// Delete a bounded batch of fully-absent workload history after its
    /// application container record has gone away. Active container handles
    /// remain resolvable even if they outlive the normal destroy sequence.
    pub async fn delete_terminal_workloads(
        &self,
        completed_before: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<Uuid>, WorkerStoreError> {
        if !(1..=1_000).contains(&limit) {
            return Err(WorkerStoreError::InvalidInput(
                "workload maintenance limit must be between 1 and 1000".to_owned(),
            ));
        }
        sqlx::query_scalar::<_, Uuid>(
            r#"WITH terminal AS (
                   SELECT workload.id
                     FROM "WorkerWorkloads" workload
                    WHERE workload.desired_state = 'Absent'
                      AND workload.observed_state = 'Absent'
                      AND workload.updated_at <= $1
                      AND NOT EXISTS (
                          SELECT 1
                            FROM "Containers" container
                           WHERE container.container_id LIKE 'rsctf-worker:%'
                             AND SPLIT_PART(container.container_id, ':', 2) =
                                 workload.id::TEXT
                      )
                 ORDER BY workload.updated_at, workload.id
                    FOR UPDATE OF workload SKIP LOCKED
                    LIMIT $2
               )
               DELETE FROM "WorkerWorkloads" workload
                USING terminal
                WHERE workload.id = terminal.id
            RETURNING workload.id"#,
        )
        .bind(completed_before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_unbounded_maintenance_batches_before_database_access() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("syntactically valid test database URL");
        let store = WorkerStore::new(pool);
        assert!(matches!(
            store.delete_terminal_workloads(Utc::now(), 0).await,
            Err(WorkerStoreError::InvalidInput(_))
        ));
        assert!(matches!(
            store.delete_terminal_workloads(Utc::now(), 1_001).await,
            Err(WorkerStoreError::InvalidInput(_))
        ));
        assert!(matches!(
            store
                .delete_expired_enrollment_operations(Utc::now(), 0)
                .await,
            Err(WorkerStoreError::InvalidInput(_))
        ));
    }
}
