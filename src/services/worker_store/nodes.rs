use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use super::{database_error, is_unique_violation, WorkerStore};
use super::{
    AuthenticatedWorker, CreateWorker, PlatformOs, ResourceReservation, SessionFence,
    WorkerAdministrativeState, WorkerCertificate, WorkerInventory, WorkerNode, WorkerSession,
    WorkerStoreError,
};

#[derive(FromRow)]
struct WorkerNodeRow {
    id: Uuid,
    name: String,
    administrative_state: String,
    platform_os: Option<String>,
    platform_architecture: Option<String>,
    runtime_kind: Option<String>,
    runtime_version: Option<String>,
    labels: Value,
    capabilities: Value,
    capacity_cpu_millis: i64,
    capacity_memory_bytes: i64,
    capacity_slots: i32,
    certificate_serial: Option<String>,
    certificate_expires_at: Option<DateTime<Utc>>,
    session_id: Option<Uuid>,
    session_epoch: i64,
    boot_id: Option<Uuid>,
    heartbeat_at: Option<DateTime<Utc>>,
    lease_expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<WorkerNodeRow> for WorkerNode {
    type Error = WorkerStoreError;

    fn try_from(row: WorkerNodeRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            administrative_state: WorkerAdministrativeState::parse(&row.administrative_state)?,
            platform_os: row
                .platform_os
                .as_deref()
                .map(PlatformOs::parse)
                .transpose()?,
            architecture: row.platform_architecture,
            runtime_kind: row.runtime_kind,
            runtime_version: row.runtime_version,
            labels: row.labels,
            capabilities: row.capabilities,
            capacity: ResourceReservation {
                cpu_millis: row.capacity_cpu_millis,
                memory_bytes: row.capacity_memory_bytes,
                slots: row.capacity_slots,
            },
            certificate_serial: row.certificate_serial,
            certificate_expires_at: row.certificate_expires_at,
            session_id: row.session_id,
            session_epoch: row.session_epoch,
            boot_id: row.boot_id,
            heartbeat_at: row.heartbeat_at,
            lease_expires_at: row.lease_expires_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn lease_seconds(duration: Duration) -> Result<i64, WorkerStoreError> {
    let seconds = duration.as_secs();
    if seconds == 0 || seconds > i64::MAX as u64 {
        return Err(WorkerStoreError::InvalidInput(
            "worker lease must be between 1 second and i64::MAX seconds".to_owned(),
        ));
    }
    Ok(seconds as i64)
}

impl WorkerStore {
    /// Register a disabled-by-certificate worker identity with a one-use token.
    pub async fn create_worker(
        &self,
        request: CreateWorker,
    ) -> Result<WorkerNode, WorkerStoreError> {
        let name = request.name.trim();
        if name.is_empty() {
            return Err(WorkerStoreError::InvalidInput(
                "worker name cannot be empty".to_owned(),
            ));
        }
        if request.enrollment_token_expires_at <= Utc::now() {
            return Err(WorkerStoreError::InvalidInput(
                "enrollment token must expire in the future".to_owned(),
            ));
        }

        let result = sqlx::query(
            r#"INSERT INTO "WorkerNodes" (
                   id, name, enrollment_token_hash, enrollment_token_expires_at
               ) VALUES ($1, $2, $3, $4)"#,
        )
        .bind(request.id)
        .bind(name)
        .bind(request.enrollment_token_hash.as_slice())
        .bind(request.enrollment_token_expires_at)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => self
                .get_worker(request.id)
                .await?
                .ok_or_else(|| WorkerStoreError::InvalidStoredData("new worker vanished".into())),
            Err(error) if is_unique_violation(&error) => Err(WorkerStoreError::Conflict(
                "worker id or name already exists".to_owned(),
            )),
            Err(error) => Err(database_error(error)),
        }
    }

    /// Replace an unused enrollment token. Existing certificates remain valid
    /// until the replacement CSR is successfully exchanged.
    pub async fn issue_enrollment_token(
        &self,
        worker_id: Uuid,
        token_hash: [u8; 32],
        expires_at: DateTime<Utc>,
    ) -> Result<bool, WorkerStoreError> {
        if expires_at <= Utc::now() {
            return Err(WorkerStoreError::InvalidInput(
                "enrollment token must expire in the future".to_owned(),
            ));
        }
        let result = sqlx::query(
            r#"UPDATE "WorkerNodes"
                  SET enrollment_token_hash = $2,
                      enrollment_token_expires_at = $3,
                      enrollment_token_used_at = NULL,
                      updated_at = clock_timestamp()
                WHERE id = $1"#,
        )
        .bind(worker_id)
        .bind(token_hash.as_slice())
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Resolve the intended worker before signing its CSR/SAN. Disabled nodes
    /// intentionally remain eligible so an operator can replace a compromised
    /// certificate before explicitly re-enabling the node. Enrollment still
    /// finishes through [`Self::enroll_certificate`], which consumes the token
    /// atomically and closes concurrent exchange races.
    pub async fn resolve_enrollment_token(
        &self,
        token_hash: [u8; 32],
    ) -> Result<Option<Uuid>, WorkerStoreError> {
        sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM "WorkerNodes"
                WHERE enrollment_token_hash = $1
                  AND enrollment_token_used_at IS NULL
                  AND enrollment_token_expires_at > clock_timestamp()"#,
        )
        .bind(token_hash.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    /// Atomically consume a one-use enrollment token and bind the resulting
    /// client certificate. The raw token is never stored by this service.
    pub async fn enroll_certificate(
        &self,
        token_hash: [u8; 32],
        certificate: WorkerCertificate,
    ) -> Result<Option<AuthenticatedWorker>, WorkerStoreError> {
        if certificate.serial.trim().is_empty() || certificate.expires_at <= Utc::now() {
            return Err(WorkerStoreError::InvalidInput(
                "worker certificate serial must be non-empty and unexpired".to_owned(),
            ));
        }

        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let result = sqlx::query_as::<_, (Uuid, String, DateTime<Utc>)>(
            r#"UPDATE "WorkerNodes"
                  SET enrollment_token_hash = NULL,
                      enrollment_token_expires_at = NULL,
                      enrollment_token_used_at = clock_timestamp(),
                      certificate_fingerprint_sha256 = $2,
                      certificate_serial = $3,
                      certificate_expires_at = $4,
                      session_id = NULL,
                      session_epoch = CASE
                          WHEN session_id IS NULL THEN session_epoch
                          ELSE session_epoch + 1
                      END,
                      boot_id = NULL,
                      connected_at = NULL,
                      lease_expires_at = NULL,
                      updated_at = clock_timestamp()
                WHERE enrollment_token_hash = $1
                  AND enrollment_token_used_at IS NULL
                  AND enrollment_token_expires_at > clock_timestamp()
            RETURNING id, administrative_state, certificate_expires_at"#,
        )
        .bind(token_hash.as_slice())
        .bind(certificate.fingerprint_sha256.as_slice())
        .bind(certificate.serial.trim())
        .bind(certificate.expires_at)
        .fetch_optional(&mut *transaction)
        .await;
        let row = match result {
            Ok(row) => row,
            Err(error) if is_unique_violation(&error) => {
                transaction.rollback().await.map_err(database_error)?;
                return Err(WorkerStoreError::Conflict(
                    "certificate fingerprint is already bound to a worker".to_owned(),
                ));
            }
            Err(error) => return Err(database_error(error)),
        };
        if let Some((id, _, _)) = row.as_ref() {
            sqlx::query(
                r#"UPDATE "WorkerWorkloads"
                      SET observed_state = 'Lost',
                          observed_session_epoch = NULL,
                          observed_message = 'worker certificate replaced',
                          observed_at = clock_timestamp(),
                          ready_at = NULL,
                          updated_at = clock_timestamp()
                    WHERE worker_id = $1 AND observed_state <> 'Absent'"#,
            )
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        row.map(|(id, state, certificate_expires_at)| {
            Ok(AuthenticatedWorker {
                id,
                administrative_state: WorkerAdministrativeState::parse(&state)?,
                certificate_expires_at,
            })
        })
        .transpose()
    }

    /// Resolve an already TLS-verified peer by the exact DER certificate
    /// fingerprint. Disabled and expired identities fail closed.
    pub async fn authenticate_certificate(
        &self,
        fingerprint_sha256: [u8; 32],
    ) -> Result<Option<AuthenticatedWorker>, WorkerStoreError> {
        let row = sqlx::query_as::<_, (Uuid, String, DateTime<Utc>)>(
            r#"SELECT id, administrative_state, certificate_expires_at
                 FROM "WorkerNodes"
                WHERE certificate_fingerprint_sha256 = $1
                  AND certificate_expires_at > clock_timestamp()
                  AND administrative_state <> 'Disabled'"#,
        )
        .bind(fingerprint_sha256.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        row.map(|(id, state, certificate_expires_at)| {
            Ok(AuthenticatedWorker {
                id,
                administrative_state: WorkerAdministrativeState::parse(&state)?,
                certificate_expires_at,
            })
        })
        .transpose()
    }

    pub async fn get_worker(&self, id: Uuid) -> Result<Option<WorkerNode>, WorkerStoreError> {
        let row = sqlx::query_as::<_, WorkerNodeRow>(
            r#"SELECT id, name, administrative_state, platform_os,
                      platform_architecture, runtime_kind, runtime_version,
                      labels, capabilities, capacity_cpu_millis,
                      capacity_memory_bytes, capacity_slots,
                      certificate_serial, certificate_expires_at,
                      session_id, session_epoch, boot_id, heartbeat_at,
                      lease_expires_at, created_at, updated_at
                 FROM "WorkerNodes"
                WHERE id = $1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?;
        row.map(TryInto::try_into).transpose()
    }

    pub async fn list_workers(&self) -> Result<Vec<WorkerNode>, WorkerStoreError> {
        let rows = sqlx::query_as::<_, WorkerNodeRow>(
            r#"SELECT id, name, administrative_state, platform_os,
                      platform_architecture, runtime_kind, runtime_version,
                      labels, capabilities, capacity_cpu_millis,
                      capacity_memory_bytes, capacity_slots,
                      certificate_serial, certificate_expires_at,
                      session_id, session_epoch, boot_id, heartbeat_at,
                      lease_expires_at, created_at, updated_at
                 FROM "WorkerNodes"
             ORDER BY LOWER(name), id"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(database_error)?;
        rows.into_iter().map(TryInto::try_into).collect()
    }

    /// Claim a new session epoch and supersede any older connection.
    pub async fn open_session(
        &self,
        worker_id: Uuid,
        certificate_fingerprint_sha256: [u8; 32],
        session_id: Uuid,
        boot_id: Uuid,
        inventory: &WorkerInventory,
        lease: Duration,
    ) -> Result<Option<WorkerSession>, WorkerStoreError> {
        inventory.validate()?;
        let lease_seconds = lease_seconds(lease)?;
        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let row = sqlx::query_as::<_, (i64, DateTime<Utc>)>(
            r#"UPDATE "WorkerNodes"
                  SET platform_os = $4,
                      platform_architecture = $5,
                      runtime_kind = $6,
                      runtime_version = $7,
                      labels = $8,
                      capabilities = $9,
                      capacity_cpu_millis = $10,
                      capacity_memory_bytes = $11,
                      capacity_slots = $12,
                      session_id = $2,
                      session_epoch = session_epoch + 1,
                      boot_id = $3,
                      connected_at = clock_timestamp(),
                      heartbeat_at = clock_timestamp(),
                      lease_expires_at = clock_timestamp()
                          + ($13 * interval '1 second'),
                      updated_at = clock_timestamp()
                WHERE id = $1
                  AND administrative_state <> 'Disabled'
                  AND certificate_fingerprint_sha256 = $14
                  AND certificate_expires_at > clock_timestamp()
            RETURNING session_epoch, lease_expires_at"#,
        )
        .bind(worker_id)
        .bind(session_id)
        .bind(boot_id)
        .bind(inventory.platform_os.as_str())
        .bind(inventory.architecture.trim())
        .bind(inventory.runtime_kind.trim())
        .bind(inventory.runtime_version.trim())
        .bind(&inventory.labels)
        .bind(&inventory.capabilities)
        .bind(inventory.capacity.cpu_millis)
        .bind(inventory.capacity.memory_bytes)
        .bind(inventory.capacity.slots)
        .bind(lease_seconds)
        .bind(certificate_fingerprint_sha256.as_slice())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if row.is_some() {
            sqlx::query(
                r#"UPDATE "WorkerWorkloads"
                      SET observed_state = 'Unknown',
                          observed_session_epoch = NULL,
                          observed_message = 'awaiting adoption by current session',
                          observed_at = clock_timestamp(),
                          ready_at = NULL,
                          updated_at = clock_timestamp()
                    WHERE worker_id = $1
                      AND (desired_state = 'Present' OR observed_state <> 'Absent')"#,
            )
            .bind(worker_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(row.map(|(session_epoch, lease_expires_at)| WorkerSession {
            fence: SessionFence {
                worker_id,
                session_id,
                session_epoch,
            },
            lease_expires_at,
        }))
    }

    /// Renew only the exact active session; receipt time comes from PostgreSQL.
    pub async fn heartbeat(
        &self,
        fence: SessionFence,
        lease: Duration,
    ) -> Result<Option<DateTime<Utc>>, WorkerStoreError> {
        let lease_seconds = lease_seconds(lease)?;
        sqlx::query_scalar::<_, DateTime<Utc>>(
            r#"UPDATE "WorkerNodes"
                  SET heartbeat_at = clock_timestamp(),
                      lease_expires_at = clock_timestamp()
                          + ($4 * interval '1 second'),
                      updated_at = clock_timestamp()
                WHERE id = $1
                  AND session_id = $2
                  AND session_epoch = $3
                  AND administrative_state <> 'Disabled'
                  AND certificate_expires_at > clock_timestamp()
            RETURNING lease_expires_at"#,
        )
        .bind(fence.worker_id)
        .bind(fence.session_id)
        .bind(fence.session_epoch)
        .bind(lease_seconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)
    }

    pub async fn close_session(&self, fence: SessionFence) -> Result<bool, WorkerStoreError> {
        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let result = sqlx::query(
            r#"UPDATE "WorkerNodes"
                  SET session_id = NULL,
                      boot_id = NULL,
                      connected_at = NULL,
                      lease_expires_at = NULL,
                      updated_at = clock_timestamp()
                WHERE id = $1 AND session_id = $2 AND session_epoch = $3"#,
        )
        .bind(fence.worker_id)
        .bind(fence.session_id)
        .bind(fence.session_epoch)
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        if result.rows_affected() == 1 {
            sqlx::query(
                r#"UPDATE "WorkerWorkloads"
                      SET observed_state = 'Lost',
                          observed_session_epoch = NULL,
                          observed_message = 'worker session closed',
                          observed_at = clock_timestamp(),
                          ready_at = NULL,
                          updated_at = clock_timestamp()
                    WHERE worker_id = $1 AND observed_state <> 'Absent'"#,
            )
            .bind(fence.worker_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(result.rows_affected() == 1)
    }

    /// Administrative disable immediately increments the session fence and
    /// marks still-desired workloads lost. Draining preserves live routes but
    /// makes the worker ineligible for new placement.
    pub async fn set_administrative_state(
        &self,
        worker_id: Uuid,
        state: WorkerAdministrativeState,
    ) -> Result<bool, WorkerStoreError> {
        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let updated = sqlx::query_scalar::<_, Uuid>(
            r#"UPDATE "WorkerNodes"
                  SET administrative_state = $2,
                      session_id = CASE WHEN $2 = 'Disabled' THEN NULL ELSE session_id END,
                      session_epoch = CASE
                          WHEN $2 = 'Disabled' AND session_id IS NOT NULL
                              THEN session_epoch + 1
                          ELSE session_epoch
                      END,
                      boot_id = CASE WHEN $2 = 'Disabled' THEN NULL ELSE boot_id END,
                      connected_at = CASE
                          WHEN $2 = 'Disabled' THEN NULL ELSE connected_at
                      END,
                      lease_expires_at = CASE
                          WHEN $2 = 'Disabled' THEN NULL ELSE lease_expires_at
                      END,
                      updated_at = clock_timestamp()
                WHERE id = $1
            RETURNING id"#,
        )
        .bind(worker_id)
        .bind(state.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if updated.is_some() && state == WorkerAdministrativeState::Disabled {
            sqlx::query(
                r#"UPDATE "WorkerWorkloads"
                      SET observed_state = 'Lost',
                          observed_session_epoch = NULL,
                          observed_message = 'worker disabled',
                          observed_at = clock_timestamp(),
                          ready_at = NULL,
                          updated_at = clock_timestamp()
                    WHERE worker_id = $1 AND observed_state <> 'Absent'"#,
            )
            .bind(worker_id)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(updated.is_some())
    }

    /// Fence expired sessions in bounded, skip-locked batches. Workloads stay
    /// assigned and reserved because an unreachable host may still be running.
    pub async fn expire_sessions(&self, limit: i64) -> Result<Vec<Uuid>, WorkerStoreError> {
        if limit <= 0 {
            return Err(WorkerStoreError::InvalidInput(
                "expiry batch limit must be positive".to_owned(),
            ));
        }
        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let ids = sqlx::query_scalar::<_, Uuid>(
            r#"WITH expired AS (
                   SELECT id
                     FROM "WorkerNodes"
                    WHERE session_id IS NOT NULL
                      AND lease_expires_at <= clock_timestamp()
                    ORDER BY lease_expires_at, id
                    FOR UPDATE SKIP LOCKED
                    LIMIT $1
               )
               UPDATE "WorkerNodes" node
                  SET session_id = NULL,
                      session_epoch = session_epoch + 1,
                      boot_id = NULL,
                      connected_at = NULL,
                      lease_expires_at = NULL,
                      updated_at = clock_timestamp()
                 FROM expired
                WHERE node.id = expired.id
            RETURNING node.id"#,
        )
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        if !ids.is_empty() {
            sqlx::query(
                r#"UPDATE "WorkerWorkloads"
                      SET observed_state = 'Lost',
                          observed_session_epoch = NULL,
                          observed_message = 'worker lease expired',
                          observed_at = clock_timestamp(),
                          ready_at = NULL,
                          updated_at = clock_timestamp()
                    WHERE worker_id = ANY($1)
                      AND observed_state <> 'Absent'"#,
            )
            .bind(&ids)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(ids)
    }
}
