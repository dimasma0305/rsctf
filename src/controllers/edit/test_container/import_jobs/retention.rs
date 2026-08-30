//! Bounded source-blob retention after an import reaches a terminal state.

use std::collections::HashSet;

use uuid::Uuid;

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

/// Release ZIP sources after the result is durable. Exact retries recover the
/// persisted result and no longer need the paid upload bytes. `owner_job_id`
/// includes coalesced retry rows; the sweep form repairs a crash after commit.
pub(super) async fn release_terminal_sources(
    st: &SharedState,
    owner_job_id: Option<Uuid>,
) -> AppResult<usize> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = sqlx::query_as::<_, (Uuid, i32)>(
        r#"SELECT job.id, job.source_file_id
             FROM "ChallengeImportJobs" job
            WHERE job.source_file_id IS NOT NULL
              AND job.status IN (2, 3)
              AND ($1::uuid IS NULL OR job.id = $1 OR job.coalesced_job_id = $1)
            ORDER BY job.updated_at, job.id
            FOR UPDATE OF job SKIP LOCKED
            LIMIT 32"#,
    )
    .bind(owner_job_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut hashes = HashSet::new();
    for (job_id, file_id) in &rows {
        sqlx::query(
            r#"UPDATE "ChallengeImportJobs"
                  SET source_file_id = NULL, updated_at = clock_timestamp()
                WHERE id = $1 AND source_file_id = $2 AND status IN (2, 3)"#,
        )
        .bind(job_id)
        .bind(file_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if let Some(hash) = sqlx::query_scalar::<_, String>(
            r#"UPDATE "Files"
                  SET reference_count = GREATEST(reference_count - 1, 0)
                WHERE id = $1
            RETURNING hash"#,
        )
        .bind(file_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        {
            hashes.insert(hash);
        }
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    for hash in hashes {
        crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
            .await?;
    }
    Ok(rows.len())
}
