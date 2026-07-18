use std::collections::HashSet;

use uuid::Uuid;

use super::{
    database_error, SessionFence, StatusUpdateOutcome, WorkerStore, WorkerStoreError,
    WorkloadStatus,
};

const MAX_STATUS_BATCH: usize = 1_000;

fn validate_workload_status(status: &WorkloadStatus) -> Result<(), WorkerStoreError> {
    if status.generation < 1 {
        return Err(WorkerStoreError::InvalidInput(
            "reported workload generation must be positive".to_owned(),
        ));
    }
    if status
        .message
        .as_deref()
        .is_some_and(|message| message.chars().count() > 4_096)
    {
        return Err(WorkerStoreError::InvalidInput(
            "workload status message exceeds 4096 characters".to_owned(),
        ));
    }
    Ok(())
}

impl WorkerStore {
    /// Apply a worker report only when all session, assignment and generation
    /// fences still match. The worker row lock makes reservation release exact
    /// relative to concurrent placement.
    pub async fn record_workload_status(
        &self,
        status: &WorkloadStatus,
    ) -> Result<StatusUpdateOutcome, WorkerStoreError> {
        validate_workload_status(status)?;

        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let current_session = sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM "WorkerNodes"
                WHERE id = $1 AND session_id = $2 AND session_epoch = $3
                  AND lease_expires_at > clock_timestamp()
                  AND administrative_state <> 'Disabled'
                FOR UPDATE"#,
        )
        .bind(status.session.worker_id)
        .bind(status.session.session_id)
        .bind(status.session.session_epoch)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if current_session.is_none() {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(StatusUpdateOutcome::Stale);
        }

        let result = sqlx::query(
            r#"UPDATE "WorkerWorkloads"
                  SET observed_state = $7,
                      observed_session_epoch = $2,
                      observed_message = $8,
                      observed_at = clock_timestamp(),
                      ready_at = CASE WHEN $7 = 'Ready' THEN clock_timestamp() ELSE NULL END,
                      updated_at = clock_timestamp()
                WHERE id = $3 AND worker_id = $1 AND assignment_id = $4
                  AND generation = $5 AND spec_hash_sha256 = $6"#,
        )
        .bind(status.session.worker_id)
        .bind(status.session.session_epoch)
        .bind(status.workload_id)
        .bind(status.assignment_id)
        .bind(status.generation)
        .bind(status.spec_hash_sha256.as_slice())
        .bind(status.state.as_str())
        .bind(status.message.as_deref())
        .execute(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(if result.rows_affected() == 1 {
            StatusUpdateOutcome::Applied
        } else {
            StatusUpdateOutcome::Stale
        })
    }

    /// Apply a bounded inventory/status batch under one session validation and
    /// one UPDATE. Every row still matches the assignment, generation and spec
    /// hash independently, so a concurrent reassignment is skipped rather than
    /// overwritten. The returned IDs are exactly the rows that were applied.
    pub async fn record_workload_status_batch(
        &self,
        session: SessionFence,
        statuses: &[WorkloadStatus],
    ) -> Result<Vec<Uuid>, WorkerStoreError> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        if statuses.len() > MAX_STATUS_BATCH {
            return Err(WorkerStoreError::InvalidInput(format!(
                "status batch exceeds {MAX_STATUS_BATCH} workloads"
            )));
        }
        let mut seen = HashSet::with_capacity(statuses.len());
        for status in statuses {
            validate_workload_status(status)?;
            if status.session != session {
                return Err(WorkerStoreError::InvalidInput(
                    "status batch contains a different worker session".to_owned(),
                ));
            }
            if !seen.insert(status.workload_id) {
                return Err(WorkerStoreError::InvalidInput(
                    "status batch contains duplicate workloads".to_owned(),
                ));
            }
        }

        let workload_ids = statuses.iter().map(|s| s.workload_id).collect::<Vec<_>>();
        let assignment_ids = statuses.iter().map(|s| s.assignment_id).collect::<Vec<_>>();
        let generations = statuses.iter().map(|s| s.generation).collect::<Vec<_>>();
        let spec_hashes = statuses
            .iter()
            .map(|s| s.spec_hash_sha256.to_vec())
            .collect::<Vec<_>>();
        let states = statuses
            .iter()
            .map(|s| s.state.as_str().to_owned())
            .collect::<Vec<_>>();
        let messages = statuses
            .iter()
            .map(|s| s.message.clone())
            .collect::<Vec<_>>();

        let mut transaction = crate::utils::database::begin_sqlx_transaction(&self.pool)
            .await
            .map_err(database_error)?;
        let current_session = sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM "WorkerNodes"
                WHERE id = $1 AND session_id = $2 AND session_epoch = $3
                  AND lease_expires_at > clock_timestamp()
                  AND administrative_state <> 'Disabled'
                FOR UPDATE"#,
        )
        .bind(session.worker_id)
        .bind(session.session_id)
        .bind(session.session_epoch)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        if current_session.is_none() {
            transaction.rollback().await.map_err(database_error)?;
            return Ok(Vec::new());
        }

        let applied = sqlx::query_scalar::<_, Uuid>(
            r#"UPDATE "WorkerWorkloads" AS workload
                  SET observed_state = incoming.state,
                      observed_session_epoch = $3,
                      observed_message = incoming.message,
                      observed_at = clock_timestamp(),
                      ready_at = CASE
                          WHEN incoming.state = 'Ready' THEN clock_timestamp() ELSE NULL
                      END,
                      updated_at = clock_timestamp()
                 FROM UNNEST(
                     $4::UUID[], $5::UUID[], $6::BIGINT[], $7::BYTEA[],
                     $8::TEXT[], $9::TEXT[]
                 ) AS incoming(
                     workload_id, assignment_id, generation, spec_hash, state, message
                 )
                WHERE workload.id = incoming.workload_id
                  AND workload.worker_id = $1
                  AND workload.assignment_id = incoming.assignment_id
                  AND workload.generation = incoming.generation
                  AND workload.spec_hash_sha256 = incoming.spec_hash
            RETURNING workload.id"#,
        )
        .bind(session.worker_id)
        .bind(session.session_id)
        .bind(session.session_epoch)
        .bind(workload_ids)
        .bind(assignment_ids)
        .bind(generations)
        .bind(spec_hashes)
        .bind(states)
        .bind(messages)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(applied)
    }
}
