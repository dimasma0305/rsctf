use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rsctf_worker_protocol::MAX_WORKLOAD_REPLICAS;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use super::{database_error, is_unique_violation, WorkerStore};
use super::{
    DefinitionUpdateOutcome, DesiredUpdateOutcome, DueWorkload, PlaceWorkload, PlacementOutcome,
    PlatformOs, ResourceReservation, SessionFence, UpdateWorkload, WorkerSession, WorkerStoreError,
    WorkerWorkload, WorkloadDefinition, WorkloadDesiredState, WorkloadObservedState,
    WorkloadPlacement,
};

mod placement;
use placement::{placement_retry_delay, select_candidate, MAX_PLACEMENT_LOCK_RETRIES};

#[derive(Clone, FromRow)]
struct WorkerWorkloadRow {
    id: Uuid,
    owner_kind: String,
    owner_key: String,
    worker_id: Uuid,
    assignment_id: Uuid,
    generation: i64,
    spec_hash_sha256: Vec<u8>,
    spec: Value,
    required_os: String,
    required_architecture: String,
    required_runtime: String,
    required_labels: Value,
    reserved_cpu_millis: i64,
    reserved_memory_bytes: i64,
    reserved_slots: i32,
    required_replicas: i32,
    desired_state: String,
    observed_state: String,
    observed_session_epoch: Option<i64>,
    observed_message: Option<String>,
    observed_at: Option<DateTime<Utc>>,
    ready_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

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

pub(super) fn stored_replica_count(spec: &Value) -> Result<i32, WorkerStoreError> {
    let services = spec
        .as_object()
        .and_then(|spec| spec.get("services"))
        .and_then(Value::as_array)
        .filter(|services| !services.is_empty())
        .ok_or_else(|| {
            WorkerStoreError::InvalidStoredData(
                "workload specification has no service array".to_owned(),
            )
        })?;
    let mut total = 0_usize;
    for service in services {
        let replicas = service
            .as_object()
            .and_then(|service| service.get("replicas"))
            .and_then(Value::as_u64)
            .and_then(|replicas| usize::try_from(replicas).ok())
            .filter(|replicas| *replicas > 0)
            .ok_or_else(|| {
                WorkerStoreError::InvalidStoredData(
                    "workload service has an invalid replica count".to_owned(),
                )
            })?;
        total = total.checked_add(replicas).ok_or_else(|| {
            WorkerStoreError::InvalidStoredData(
                "workload replica count exceeds numeric limits".to_owned(),
            )
        })?;
        if total > MAX_WORKLOAD_REPLICAS {
            return Err(WorkerStoreError::InvalidStoredData(format!(
                "workload has more than {MAX_WORKLOAD_REPLICAS} replicas"
            )));
        }
    }
    i32::try_from(total).map_err(|_| {
        WorkerStoreError::InvalidStoredData(
            "workload replica count exceeds database limits".to_owned(),
        )
    })
}

#[derive(Clone, Copy, FromRow)]
struct DispatchIdentity {
    id: Uuid,
    worker_id: Uuid,
    session_id: Uuid,
    session_epoch: i64,
    lease_expires_at: DateTime<Utc>,
}

fn validate_owner<'a>(kind: &'a str, key: &'a str) -> Result<(&'a str, &'a str), WorkerStoreError> {
    let kind = kind.trim();
    let key = key.trim();
    if kind.is_empty() || kind.chars().count() > 64 {
        return Err(WorkerStoreError::InvalidInput(
            "workload owner kind must contain 1 to 64 characters".to_owned(),
        ));
    }
    if key.is_empty() || key.chars().count() > 512 {
        return Err(WorkerStoreError::InvalidInput(
            "workload owner key must contain 1 to 512 characters".to_owned(),
        ));
    }
    Ok((kind, key))
}

fn validate_batch(limit: i64) -> Result<(), WorkerStoreError> {
    if !(1..=1_000).contains(&limit) {
        return Err(WorkerStoreError::InvalidInput(
            "workload batch limit must be between 1 and 1000".to_owned(),
        ));
    }
    Ok(())
}

impl WorkerStore {
    pub async fn get_workload(&self, id: Uuid) -> Result<Option<WorkerWorkload>, WorkerStoreError> {
        let row = sqlx::query_as::<_, WorkerWorkloadRow>(
            r#"SELECT id, owner_kind, owner_key, worker_id, assignment_id,
                      generation, spec_hash_sha256, spec, required_os,
                      required_architecture, required_runtime, required_labels,
                      reserved_cpu_millis, reserved_memory_bytes, reserved_slots,
                      required_replicas,
                      desired_state, observed_state, observed_session_epoch,
                      observed_message, observed_at, ready_at, created_at, updated_at
                 FROM "WorkerWorkloads"
                WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        row.map(TryInto::try_into).transpose()
    }

    pub async fn find_workload_by_owner(
        &self,
        owner_kind: &str,
        owner_key: &str,
    ) -> Result<Option<WorkerWorkload>, WorkerStoreError> {
        let (owner_kind, owner_key) = validate_owner(owner_kind, owner_key)?;
        let row = sqlx::query_as::<_, WorkerWorkloadRow>(
            r#"SELECT id, owner_kind, owner_key, worker_id, assignment_id,
                      generation, spec_hash_sha256, spec, required_os,
                      required_architecture, required_runtime, required_labels,
                      reserved_cpu_millis, reserved_memory_bytes, reserved_slots,
                      required_replicas,
                      desired_state, observed_state, observed_session_epoch,
                      observed_message, observed_at, ready_at, created_at, updated_at
                 FROM "WorkerWorkloads"
                WHERE owner_kind = $1 AND owner_key = $2"#,
        )
        .bind(owner_kind)
        .bind(owner_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        row.map(TryInto::try_into).transpose()
    }

    /// Return the exact live session allowed to carry player/checker traffic.
    /// A `Ready` observation from a superseded session never publishes a route.
    pub async fn ready_workload_session(
        &self,
        workload_id: Uuid,
    ) -> Result<Option<WorkerSession>, WorkerStoreError> {
        let row = sqlx::query_as::<_, (Uuid, Uuid, i64, DateTime<Utc>)>(
            r#"SELECT node.id, node.session_id, node.session_epoch,
                      node.lease_expires_at
                 FROM "WorkerWorkloads" workload
                 JOIN "WorkerNodes" node ON node.id = workload.worker_id
                WHERE workload.id = $1
                  AND workload.desired_state = 'Present'
                  AND workload.observed_state = 'Ready'
                  AND workload.observed_session_epoch = node.session_epoch
                  AND node.session_id IS NOT NULL
                  AND node.lease_expires_at > clock_timestamp()
                  AND node.certificate_expires_at > clock_timestamp()
                  AND node.administrative_state <> 'Disabled'
                  AND node.capabilities @> '{
                      "ensureWorkload": true,
                      "writeFlag": true,
                      "tcpProxy": true,
                      "inventory": true
                  }'::jsonb
                  AND CASE
                      WHEN jsonb_typeof(node.capabilities -> 'maxDataLanes') = 'number'
                      THEN (node.capabilities ->> 'maxDataLanes')::NUMERIC
                           BETWEEN 1 AND 65535
                      ELSE FALSE
                  END
                  AND CASE
                      WHEN jsonb_typeof(
                          node.capabilities -> 'maxWorkloadReplicas'
                      ) = 'number'
                      THEN (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC =
                               TRUNC((node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC)
                           AND (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC
                               BETWEEN workload.required_replicas AND 512
                      ELSE FALSE
                  END"#,
        )
        .bind(workload_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(row.map(
            |(worker_id, session_id, session_epoch, lease_expires_at)| WorkerSession {
                fence: SessionFence {
                    worker_id,
                    session_id,
                    session_epoch,
                },
                lease_expires_at,
            },
        ))
    }

    /// Select a compatible live worker and reserve exact row-derived capacity.
    /// An exact worker pin supports `worker://...` local image identities.
    pub async fn place_workload(
        &self,
        request: &PlaceWorkload,
    ) -> Result<PlacementOutcome, WorkerStoreError> {
        let required_replicas = request.definition.validate()?;
        let (owner_kind, owner_key) = validate_owner(&request.owner_kind, &request.owner_key)?;
        if !request.required_labels.is_object() {
            return Err(WorkerStoreError::InvalidInput(
                "required worker labels must be a JSON object".to_owned(),
            ));
        }
        if let Some(existing) = self.find_workload_by_owner(owner_kind, owner_key).await? {
            return Ok(PlacementOutcome::AlreadyExists(existing));
        }

        let mut attempt = 0;
        let (mut transaction, candidate) = loop {
            let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
                .await
                .map_err(database_error)?;
            if let Some(candidate) =
                select_candidate(&mut transaction, request, required_replicas, true).await?
            {
                break (transaction, candidate);
            }

            // A plain read can still see a capacity-qualified row while its
            // lock is held by another placement. Retry that transient case;
            // return immediately when committed state really has no capacity.
            let locked_candidate_exists =
                select_candidate(&mut transaction, request, required_replicas, false)
                    .await?
                    .is_some();
            transaction.commit().await.map_err(database_error)?;
            if let Some(existing) = self.find_workload_by_owner(owner_kind, owner_key).await? {
                return Ok(PlacementOutcome::AlreadyExists(existing));
            }
            if !locked_candidate_exists || attempt >= MAX_PLACEMENT_LOCK_RETRIES {
                return Ok(PlacementOutcome::NoCompatibleCapacity);
            }
            tokio::time::sleep(placement_retry_delay(request.id, attempt)).await;
            attempt += 1;
        };

        let insert = sqlx::query_scalar::<_, Uuid>(
            r#"INSERT INTO "WorkerWorkloads" (
                   id, owner_kind, owner_key, worker_id, assignment_id, generation,
                   spec_hash_sha256, spec, required_os, required_architecture,
                   required_runtime, required_labels, reserved_cpu_millis,
                   reserved_memory_bytes, reserved_slots, required_replicas
               ) VALUES (
                   $1, $2, $3, $4, $5, 1, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15
               )
               ON CONFLICT (owner_kind, owner_key) DO NOTHING
               RETURNING id"#,
        )
        .bind(request.id)
        .bind(owner_kind)
        .bind(owner_key)
        .bind(candidate.id)
        .bind(request.assignment_id)
        .bind(request.definition.spec_hash_sha256.as_slice())
        .bind(&request.definition.spec)
        .bind(request.definition.required_os.as_str())
        .bind(request.definition.required_architecture.trim())
        .bind(request.definition.required_runtime.trim())
        .bind(&request.required_labels)
        .bind(request.definition.reservation.cpu_millis)
        .bind(request.definition.reservation.memory_bytes)
        .bind(request.definition.reservation.slots)
        .bind(required_replicas)
        .fetch_optional(&mut *transaction)
        .await;
        let inserted = match insert {
            Ok(inserted) => inserted,
            Err(error) if is_unique_violation(&error) => {
                transaction.rollback().await.map_err(database_error)?;
                return Err(WorkerStoreError::Conflict(
                    "workload id or assignment id already exists".to_owned(),
                ));
            }
            Err(error) => return Err(database_error(error)),
        };
        transaction.commit().await.map_err(database_error)?;

        let workload = match inserted {
            Some(id) => self.get_workload(id).await?.ok_or_else(|| {
                WorkerStoreError::InvalidStoredData("placed workload vanished".to_owned())
            })?,
            None => self
                .find_workload_by_owner(owner_kind, owner_key)
                .await?
                .ok_or_else(|| {
                    WorkerStoreError::InvalidStoredData(
                        "owner conflict did not resolve to a workload".to_owned(),
                    )
                })?,
        };
        if inserted.is_none() {
            return Ok(PlacementOutcome::AlreadyExists(workload));
        }
        Ok(PlacementOutcome::Placed(WorkloadPlacement {
            workload,
            session: WorkerSession {
                fence: SessionFence {
                    worker_id: candidate.id,
                    session_id: candidate.session_id,
                    session_epoch: candidate.session_epoch,
                },
                lease_expires_at: candidate.lease_expires_at,
            },
        }))
    }

    /// Replace a workload definition (including replica reservations) while
    /// retaining its placement. The generation is an internal command fence,
    /// unrelated to A&D or KotH score calculation.
    pub async fn update_workload_definition(
        &self,
        request: &UpdateWorkload,
    ) -> Result<DefinitionUpdateOutcome, WorkerStoreError> {
        let required_replicas = request.definition.validate()?;
        if request.expected_generation < 1 {
            return Err(WorkerStoreError::InvalidInput(
                "expected workload generation must be positive".to_owned(),
            ));
        }
        let current = sqlx::query_as::<_, (Uuid, Value)>(
            r#"SELECT worker_id, required_labels
                 FROM "WorkerWorkloads"
                WHERE id = $1 AND assignment_id = $2 AND generation = $3
                  AND desired_state = 'Present'"#,
        )
        .bind(request.id)
        .bind(request.assignment_id)
        .bind(request.expected_generation)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        let Some((worker_id, required_labels)) = current else {
            return Ok(DefinitionUpdateOutcome::Stale);
        };

        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let capacity = sqlx::query_as::<_, (bool, bool)>(
            r#"SELECT
                   node.platform_os = $3
                   AND node.platform_architecture = $4
                   AND node.runtime_kind = $5
                   AND node.labels @> $6
                   AND CASE
                       WHEN jsonb_typeof(
                           node.capabilities -> 'maxWorkloadReplicas'
                       ) = 'number'
                       THEN (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC =
                                TRUNC((node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC)
                            AND (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC
                                BETWEEN $7 AND 512
                       ELSE FALSE
                   END AS compatible,
                   node.capacity_cpu_millis >= reserved.cpu_millis + $8
                   AND node.capacity_memory_bytes >= reserved.memory_bytes + $9
                   AND node.capacity_slots >= reserved.slots + $10 AS has_capacity
                 FROM "WorkerNodes" node
                 CROSS JOIN LATERAL (
                     SELECT COALESCE(SUM(workload.reserved_cpu_millis), 0)::BIGINT
                                AS cpu_millis,
                            COALESCE(SUM(workload.reserved_memory_bytes), 0)::BIGINT
                                AS memory_bytes,
                            COALESCE(SUM(workload.reserved_slots), 0)::BIGINT AS slots
                       FROM "WorkerWorkloads" workload
                      WHERE workload.worker_id = node.id
                        AND workload.id <> $2
                        AND (
                            workload.desired_state = 'Present'
                            OR workload.observed_state <> 'Absent'
                        )
                 ) reserved
                WHERE node.id = $1
                FOR UPDATE OF node"#,
        )
        .bind(worker_id)
        .bind(request.id)
        .bind(request.definition.required_os.as_str())
        .bind(request.definition.required_architecture.trim())
        .bind(request.definition.required_runtime.trim())
        .bind(&required_labels)
        .bind(required_replicas)
        .bind(request.definition.reservation.cpu_millis)
        .bind(request.definition.reservation.memory_bytes)
        .bind(request.definition.reservation.slots)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        let Some((compatible, has_capacity)) = capacity else {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(DefinitionUpdateOutcome::Stale);
        };
        if !compatible {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(DefinitionUpdateOutcome::WorkerNoLongerCompatible);
        }
        if !has_capacity {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(DefinitionUpdateOutcome::InsufficientCapacity);
        }

        let next_generation = request.expected_generation.checked_add(1).ok_or_else(|| {
            WorkerStoreError::InvalidInput("workload generation is exhausted".to_owned())
        })?;
        let updated = sqlx::query_scalar::<_, i64>(
            r#"UPDATE "WorkerWorkloads"
                  SET generation = $4,
                      spec_hash_sha256 = $5,
                      spec = $6,
                      required_os = $7,
                      required_architecture = $8,
                      required_runtime = $9,
                      reserved_cpu_millis = $10,
                      reserved_memory_bytes = $11,
                      reserved_slots = $12,
                      required_replicas = $13,
                      observed_state = 'Unknown',
                      observed_session_epoch = NULL,
                      observed_message = NULL,
                      observed_at = NULL,
                      ready_at = NULL,
                      updated_at = clock_timestamp()
                WHERE id = $1 AND assignment_id = $2 AND generation = $3
                  AND worker_id = $14 AND desired_state = 'Present'
            RETURNING generation"#,
        )
        .bind(request.id)
        .bind(request.assignment_id)
        .bind(request.expected_generation)
        .bind(next_generation)
        .bind(request.definition.spec_hash_sha256.as_slice())
        .bind(&request.definition.spec)
        .bind(request.definition.required_os.as_str())
        .bind(request.definition.required_architecture.trim())
        .bind(request.definition.required_runtime.trim())
        .bind(request.definition.reservation.cpu_millis)
        .bind(request.definition.reservation.memory_bytes)
        .bind(request.definition.reservation.slots)
        .bind(required_replicas)
        .bind(worker_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(match updated {
            Some(generation) => DefinitionUpdateOutcome::Updated { generation },
            None => DefinitionUpdateOutcome::Stale,
        })
    }

    /// Request deletion. Capacity remains reserved until a fenced `Absent`
    /// observation or an explicit operator force-forget.
    pub async fn mark_desired_absent(
        &self,
        id: Uuid,
        assignment_id: Uuid,
        expected_generation: i64,
    ) -> Result<DesiredUpdateOutcome, WorkerStoreError> {
        let worker_id = sqlx::query_scalar::<_, Uuid>(
            r#"SELECT worker_id FROM "WorkerWorkloads"
                WHERE id = $1 AND assignment_id = $2 AND generation = $3
                  AND desired_state = 'Present'"#,
        )
        .bind(id)
        .bind(assignment_id)
        .bind(expected_generation)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        let Some(worker_id) = worker_id else {
            return Ok(DesiredUpdateOutcome::Stale);
        };

        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        sqlx::query(r#"SELECT id FROM "WorkerNodes" WHERE id = $1 FOR UPDATE"#)
            .bind(worker_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let generation = sqlx::query_scalar::<_, i64>(
            r#"UPDATE "WorkerWorkloads"
                  SET generation = generation + 1,
                      desired_state = 'Absent',
                      observed_state = 'Unknown',
                      observed_session_epoch = NULL,
                      observed_message = NULL,
                      observed_at = NULL,
                      ready_at = NULL,
                      updated_at = clock_timestamp()
                WHERE id = $1 AND assignment_id = $2 AND generation = $3
                  AND worker_id = $4 AND desired_state = 'Present'
            RETURNING generation"#,
        )
        .bind(id)
        .bind(assignment_id)
        .bind(expected_generation)
        .bind(worker_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(match generation {
            Some(generation) => DesiredUpdateOutcome::Updated { generation },
            None => DesiredUpdateOutcome::Stale,
        })
    }

    /// Fence crash-orphaned backend workloads whose application bookkeeping
    /// row was never committed. Container creation allocates the durable
    /// `container:<uuid>` owner before contacting a worker; a process crash in
    /// the short gap before inserting `Containers` must not reserve capacity
    /// or run challenge services forever.
    pub async fn mark_orphaned_container_workloads_absent(
        &self,
        created_before: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<Uuid>, WorkerStoreError> {
        validate_batch(limit)?;
        let ids = sqlx::query_scalar::<_, Uuid>(
            r#"WITH orphan AS (
                   SELECT workload.id
                     FROM "WorkerWorkloads" workload
                    WHERE workload.owner_kind = 'container'
                      AND workload.owner_key ~
                          '^container:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
                      AND workload.desired_state = 'Present'
                      AND workload.created_at <= $1
                      AND NOT EXISTS (
                          SELECT 1
                            FROM "Containers" container
                           WHERE container.id::TEXT =
                                 SUBSTRING(workload.owner_key FROM 11)
                      )
                 ORDER BY workload.created_at, workload.id
                    FOR UPDATE OF workload SKIP LOCKED
                    LIMIT $2
               )
               UPDATE "WorkerWorkloads" workload
                  SET generation = workload.generation + 1,
                      desired_state = 'Absent',
                      observed_state = 'Unknown',
                      observed_session_epoch = NULL,
                      observed_message = 'application bookkeeping was not committed',
                      observed_at = NULL,
                      ready_at = NULL,
                      updated_at = clock_timestamp()
                 FROM orphan
                WHERE workload.id = orphan.id
            RETURNING workload.id"#,
        )
        .bind(created_before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(ids)
    }

    /// Delay the next periodic retry after a command was queued or the exact
    /// workload already occupied its bounded in-flight slot. A superseded
    /// session or generation cannot postpone current work.
    pub async fn mark_dispatched(
        &self,
        session: SessionFence,
        workload_id: Uuid,
        assignment_id: Uuid,
        generation: i64,
    ) -> Result<bool, WorkerStoreError> {
        let result = sqlx::query(
            r#"UPDATE "WorkerWorkloads" workload
                  SET updated_at = clock_timestamp()
                WHERE workload.id = $4
                  AND workload.worker_id = $1
                  AND workload.assignment_id = $5
                  AND workload.generation = $6
                  AND (
                      (
                          workload.desired_state = 'Present'
                          AND (
                              workload.observed_state <> 'Ready'
                              OR workload.observed_session_epoch IS DISTINCT FROM $3
                          )
                      )
                      OR (
                          workload.desired_state = 'Absent'
                          AND workload.observed_state <> 'Absent'
                      )
                  )
                  AND EXISTS (
                      SELECT 1 FROM "WorkerNodes" node
                       WHERE node.id = workload.worker_id
                         AND node.session_id = $2
                         AND node.session_epoch = $3
                         AND node.lease_expires_at > clock_timestamp()
                         AND CASE
                             WHEN jsonb_typeof(
                                 node.capabilities -> 'maxWorkloadReplicas'
                             ) = 'number'
                             THEN (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC =
                                      TRUNC((node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC)
                                  AND (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC
                                      BETWEEN workload.required_replicas AND 512
                             ELSE FALSE
                         END
                  )"#,
        )
        .bind(session.worker_id)
        .bind(session.session_id)
        .bind(session.session_epoch)
        .bind(workload_id)
        .bind(assignment_id)
        .bind(generation)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// All assignments an exact newly-opened session must adopt/reconcile.
    pub async fn list_session_workloads(
        &self,
        session: SessionFence,
        after_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<WorkerWorkload>, WorkerStoreError> {
        validate_batch(limit)?;
        let rows = sqlx::query_as::<_, WorkerWorkloadRow>(
            r#"SELECT workload.id, workload.owner_kind, workload.owner_key,
                      workload.worker_id, workload.assignment_id, workload.generation,
                      workload.spec_hash_sha256, workload.spec, workload.required_os,
                      workload.required_architecture, workload.required_runtime,
                      workload.required_labels, workload.reserved_cpu_millis,
                      workload.reserved_memory_bytes, workload.reserved_slots,
                      workload.required_replicas,
                      workload.desired_state, workload.observed_state,
                      workload.observed_session_epoch, workload.observed_message,
                      workload.observed_at, workload.ready_at, workload.created_at,
                      workload.updated_at
                 FROM "WorkerWorkloads" workload
                WHERE workload.worker_id = $1
                  AND ($4::UUID IS NULL OR workload.id > $4)
                  AND (
                      workload.desired_state = 'Present'
                      OR workload.observed_state <> 'Absent'
                  )
                  AND EXISTS (
                      SELECT 1 FROM "WorkerNodes" node
                       WHERE node.id = workload.worker_id
                         AND node.session_id = $2
                         AND node.session_epoch = $3
                         AND node.lease_expires_at > clock_timestamp()
                         AND CASE
                             WHEN jsonb_typeof(
                                 node.capabilities -> 'maxWorkloadReplicas'
                             ) = 'number'
                             THEN (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC =
                                      TRUNC((node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC)
                                  AND (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC
                                      BETWEEN workload.required_replicas AND 512
                             ELSE FALSE
                         END
                  )
             ORDER BY workload.id
                LIMIT $5"#,
        )
        .bind(session.worker_id)
        .bind(session.session_id)
        .bind(session.session_epoch)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Discover desired/observed mismatches across live workers. Opening a new
    /// session atomically resets that worker's active observations to Unknown,
    /// so this hot global scan does not walk every steady Ready row just to
    /// compare epochs. Failed creates use a longer retry interval so a
    /// permanent runtime error cannot create a tight pull loop; the live
    /// registry separately suppresses in-flight work.
    pub async fn list_due_workloads(
        &self,
        retry_before: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<DueWorkload>, WorkerStoreError> {
        validate_batch(limit)?;
        let dispatches = sqlx::query_as::<_, DispatchIdentity>(
            r#"SELECT workload.id, workload.worker_id, node.session_id,
                      node.session_epoch, node.lease_expires_at
                 FROM "WorkerWorkloads" workload
                 JOIN "WorkerNodes" node ON node.id = workload.worker_id
                WHERE node.session_id IS NOT NULL
                  AND node.lease_expires_at > clock_timestamp()
                  AND node.certificate_expires_at > clock_timestamp()
                  AND node.administrative_state <> 'Disabled'
                  AND node.capabilities @> '{
                      "ensureWorkload": true,
                      "writeFlag": true,
                      "tcpProxy": true,
                      "inventory": true
                  }'::jsonb
                  AND CASE
                      WHEN jsonb_typeof(node.capabilities -> 'maxDataLanes') = 'number'
                      THEN (node.capabilities ->> 'maxDataLanes')::NUMERIC
                           BETWEEN 1 AND 65535
                      ELSE FALSE
                  END
                  AND CASE
                      WHEN jsonb_typeof(
                          node.capabilities -> 'maxWorkloadReplicas'
                      ) = 'number'
                      THEN (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC =
                               TRUNC((node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC)
                           AND (node.capabilities ->> 'maxWorkloadReplicas')::NUMERIC
                               BETWEEN workload.required_replicas AND 512
                      ELSE FALSE
                  END
                  AND workload.updated_at <= CASE
                      WHEN workload.desired_state = 'Present'
                       AND workload.observed_state = 'Failed'
                      THEN $1 - INTERVAL '28 seconds'
                      ELSE $1
                  END
                  AND (
                      (workload.desired_state = 'Present'
                       AND workload.observed_state <> 'Ready')
                      OR (workload.desired_state = 'Absent'
                          AND workload.observed_state <> 'Absent')
                  )
             ORDER BY workload.updated_at, workload.id
                LIMIT $2"#,
        )
        .bind(retry_before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        if dispatches.is_empty() {
            return Ok(Vec::new());
        }

        let ids = dispatches
            .iter()
            .map(|dispatch| dispatch.id)
            .collect::<Vec<_>>();
        let workloads = sqlx::query_as::<_, WorkerWorkloadRow>(
            r#"SELECT id, owner_kind, owner_key, worker_id, assignment_id,
                      generation, spec_hash_sha256, spec, required_os,
                      required_architecture, required_runtime, required_labels,
                      reserved_cpu_millis, reserved_memory_bytes, reserved_slots,
                      required_replicas,
                      desired_state, observed_state, observed_session_epoch,
                      observed_message, observed_at, ready_at, created_at, updated_at
                 FROM "WorkerWorkloads"
                WHERE id = ANY($1)"#,
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        let mut workloads = workloads
            .into_iter()
            .map(|row| {
                let workload: WorkerWorkload = row.try_into()?;
                Ok((workload.id, workload))
            })
            .collect::<Result<HashMap<_, _>, WorkerStoreError>>()?;

        let mut due = Vec::with_capacity(dispatches.len());
        for dispatch in dispatches {
            if let Some(workload) = workloads.remove(&dispatch.id) {
                due.push(DueWorkload {
                    workload,
                    session: WorkerSession {
                        fence: SessionFence {
                            worker_id: dispatch.worker_id,
                            session_id: dispatch.session_id,
                            session_epoch: dispatch.session_epoch,
                        },
                        lease_expires_at: dispatch.lease_expires_at,
                    },
                });
            }
        }
        Ok(due)
    }

    /// Explicit operator escape hatch for an unreachable worker. This is the
    /// only transition which releases a lost workload's reservation without an
    /// agent acknowledgement.
    pub async fn force_forget_workload(
        &self,
        id: Uuid,
        assignment_id: Uuid,
    ) -> Result<bool, WorkerStoreError> {
        let worker_id = sqlx::query_scalar::<_, Uuid>(
            r#"SELECT worker_id FROM "WorkerWorkloads"
                WHERE id = $1 AND assignment_id = $2"#,
        )
        .bind(id)
        .bind(assignment_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        let Some(worker_id) = worker_id else {
            return Ok(false);
        };
        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        sqlx::query(r#"SELECT id FROM "WorkerNodes" WHERE id = $1 FOR UPDATE"#)
            .bind(worker_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let result = sqlx::query(
            r#"UPDATE "WorkerWorkloads"
                  SET generation = generation + 1,
                      desired_state = 'Absent',
                      observed_state = 'Absent',
                      observed_session_epoch = NULL,
                      observed_message = 'force-forgotten by operator',
                      observed_at = clock_timestamp(),
                      ready_at = NULL,
                      updated_at = clock_timestamp()
                WHERE id = $1 AND assignment_id = $2 AND worker_id = $3"#,
        )
        .bind(id)
        .bind(assignment_id)
        .bind(worker_id)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(result.rows_affected() == 1)
    }
}
