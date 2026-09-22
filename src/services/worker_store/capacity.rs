//! Aggregate free capacity of the trusted worker plane for operator planning.
//! The scheduler admits per node; this read only totals the same reservation
//! rule across every node that could accept a placement right now.

use sqlx::FromRow;

use super::{database_error, WorkerStore, WorkerStoreError};

/// Sum of capacity minus live reservations over placeable workers.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, FromRow, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct WorkerCapacitySummary {
    pub workers: i64,
    pub cpu_millis: i64,
    pub memory_bytes: i64,
    pub slots: i64,
}

const CAPACITY_SUMMARY_SQL: &str = r#"SELECT COUNT(*)::BIGINT AS workers,
           COALESCE(SUM(node.capacity_cpu_millis - reserved.cpu_millis), 0)::BIGINT AS cpu_millis,
           COALESCE(SUM(node.capacity_memory_bytes - reserved.memory_bytes), 0)::BIGINT
               AS memory_bytes,
           COALESCE(SUM(node.capacity_slots - reserved.slots), 0)::BIGINT AS slots
      FROM "WorkerNodes" node
      CROSS JOIN LATERAL (
          SELECT COALESCE(SUM(workload.reserved_cpu_millis), 0)::BIGINT AS cpu_millis,
                 COALESCE(SUM(workload.reserved_memory_bytes), 0)::BIGINT AS memory_bytes,
                 COALESCE(SUM(workload.reserved_slots), 0)::BIGINT AS slots
            FROM "WorkerWorkloads" workload
           WHERE workload.worker_id = node.id
             AND (
                 workload.desired_state = 'Present'
                 OR workload.observed_state <> 'Absent'
             )
      ) reserved
     WHERE node.administrative_state = 'Enabled'
       AND node.session_id IS NOT NULL
       AND node.lease_expires_at > clock_timestamp()
       AND node.certificate_expires_at > clock_timestamp()"#;

impl WorkerStore {
    /// Free capacity across enabled, connected workers. Negative per-node
    /// remainders cannot occur because placement reserves atomically.
    pub async fn capacity_summary(&self) -> Result<WorkerCapacitySummary, WorkerStoreError> {
        sqlx::query_as::<_, WorkerCapacitySummary>(CAPACITY_SUMMARY_SQL)
            .fetch_one(&self.pool)
            .await
            .map_err(database_error)
    }
}

#[cfg(test)]
mod tests {
    use super::CAPACITY_SUMMARY_SQL;

    #[test]
    fn summary_uses_the_placement_reservation_and_liveness_rules() {
        assert!(CAPACITY_SUMMARY_SQL.contains("workload.desired_state = 'Present'"));
        assert!(CAPACITY_SUMMARY_SQL.contains("workload.observed_state <> 'Absent'"));
        assert!(CAPACITY_SUMMARY_SQL.contains("node.lease_expires_at > clock_timestamp()"));
        assert!(CAPACITY_SUMMARY_SQL.contains("node.administrative_state = 'Enabled'"));
    }
}
