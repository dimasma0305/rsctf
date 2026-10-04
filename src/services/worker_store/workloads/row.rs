//! Durable workload row validation and domain conversion.

use super::*;

impl TryFrom<WorkerWorkloadRow> for WorkerWorkload {
    type Error = WorkerStoreError;

    fn try_from(row: WorkerWorkloadRow) -> Result<Self, Self::Error> {
        let derived_replicas = stored_replica_count(&row.spec)?;
        if row.reserved_cpu_millis < 0 || row.reserved_memory_bytes < 0 || row.reserved_slots != 1 {
            return Err(WorkerStoreError::InvalidStoredData(format!(
                "workload {} has invalid stored resource dimensions",
                row.id
            )));
        }
        let spec_hash_sha256 = row.spec_hash_sha256.try_into().map_err(|hash: Vec<u8>| {
            WorkerStoreError::InvalidStoredData(format!(
                "workload {} has a {}-byte specification hash",
                row.id,
                hash.len()
            ))
        })?;
        let definition = WorkloadDefinition {
            spec: row.spec,
            spec_hash_sha256,
            required_os: PlatformOs::parse(&row.required_os)?,
            required_architecture: row.required_architecture,
            required_runtime: row.required_runtime,
            reservation: ResourceReservation {
                cpu_millis: row.reserved_cpu_millis,
                memory_bytes: row.reserved_memory_bytes,
                slots: row.reserved_slots,
            },
        };
        if derived_replicas != row.required_replicas {
            return Err(WorkerStoreError::InvalidStoredData(format!(
                "workload {} stores {} required replicas but its specification requires {derived_replicas}",
                row.id, row.required_replicas
            )));
        }
        Ok(Self {
            id: row.id,
            owner_kind: row.owner_kind,
            owner_key: row.owner_key,
            worker_id: row.worker_id,
            assignment_id: row.assignment_id,
            generation: row.generation,
            definition,
            required_labels: row.required_labels,
            desired_state: WorkloadDesiredState::parse(&row.desired_state)?,
            observed_state: WorkloadObservedState::parse(&row.observed_state)?,
            observed_session_epoch: row.observed_session_epoch,
            observed_message: row.observed_message,
            observed_at: row.observed_at,
            ready_at: row.ready_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}
