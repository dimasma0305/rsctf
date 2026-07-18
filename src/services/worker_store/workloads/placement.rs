use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use super::super::{database_error, PlaceWorkload, WorkerStoreError};

#[derive(FromRow)]
pub(super) struct CandidateWorker {
    pub(super) id: Uuid,
    pub(super) session_id: Uuid,
    pub(super) session_epoch: i64,
    pub(super) lease_expires_at: DateTime<Utc>,
}

const CANDIDATE_WORKER_SQL: &str = r#"SELECT node.id, node.session_id, node.session_epoch,
           node.lease_expires_at
      FROM "WorkerNodes" node
      CROSS JOIN LATERAL (
          SELECT COALESCE(SUM(workload.reserved_cpu_millis), 0)::BIGINT
                     AS cpu_millis,
                 COALESCE(SUM(workload.reserved_memory_bytes), 0)::BIGINT
                     AS memory_bytes,
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
       AND node.certificate_expires_at > clock_timestamp()
       AND node.capabilities @> '{
           "ensureWorkload": true,
           "writeFlag": true,
           "tcpProxy": true,
           "inventory": true
       }'::jsonb
       AND CASE
           WHEN jsonb_typeof(node.capabilities -> 'maxDataLanes') = 'number'
           THEN (node.capabilities ->> 'maxDataLanes')::NUMERIC BETWEEN 1 AND 65535
           ELSE FALSE
       END
       AND CASE
           WHEN jsonb_typeof(node.capabilities -> 'maxWorkloadReplicas') = 'number'
           THEN (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC =
                    TRUNC((node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC)
                AND (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC
                    BETWEEN $9 AND 512
           ELSE FALSE
       END
       AND node.platform_os = $1
       AND node.platform_architecture = $2
       AND node.runtime_kind = $3
       AND ($4::UUID IS NULL OR node.id = $4)
       AND node.labels @> $5
       AND node.capacity_cpu_millis >= reserved.cpu_millis + $6
       AND node.capacity_memory_bytes >= reserved.memory_bytes + $7
       AND node.capacity_slots >= reserved.slots + $8
  ORDER BY node.capacity_memory_bytes - reserved.memory_bytes ASC,
           node.capacity_cpu_millis - reserved.cpu_millis ASC,
           node.capacity_slots - reserved.slots ASC,
           node.id"#;

pub(super) const MAX_PLACEMENT_LOCK_RETRIES: u32 = 32;

pub(super) async fn select_candidate(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &PlaceWorkload,
    required_replicas: i32,
    lock: bool,
) -> Result<Option<CandidateWorker>, WorkerStoreError> {
    let suffix = if lock {
        " FOR UPDATE OF node SKIP LOCKED LIMIT 1"
    } else {
        " LIMIT 1"
    };
    let query = format!("{CANDIDATE_WORKER_SQL}{suffix}");
    sqlx::query_as::<_, CandidateWorker>(&query)
        .bind(request.definition.required_os.as_str())
        .bind(request.definition.required_architecture.trim())
        .bind(request.definition.required_runtime.trim())
        .bind(request.exact_worker_id)
        .bind(&request.required_labels)
        .bind(request.definition.reservation.cpu_millis)
        .bind(request.definition.reservation.memory_bytes)
        .bind(request.definition.reservation.slots)
        .bind(required_replicas)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(database_error)
}

pub(super) fn placement_retry_delay(id: Uuid, attempt: u32) -> Duration {
    let mut seed = u64::from_le_bytes(id.as_bytes()[..8].try_into().expect("UUID prefix"));
    seed ^= u64::from(attempt).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let ceiling_ms = 2_u64.saturating_mul(1_u64 << attempt.min(5)).min(50);
    Duration::from_millis(1 + seed % ceiling_ms)
}

#[cfg(test)]
mod tests {
    use super::CANDIDATE_WORKER_SQL;

    #[test]
    fn placement_keeps_workload_slots_and_replica_capability_separate() {
        assert!(CANDIDATE_WORKER_SQL.contains("capacity_slots >= reserved.slots + $8"));
        assert!(CANDIDATE_WORKER_SQL.contains("BETWEEN $9 AND 512"));
    }
}
