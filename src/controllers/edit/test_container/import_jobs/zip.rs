//! ZIP-source reservation and publication for durable challenge imports.

use super::*;

pub(in crate::controllers::edit::test_container) async fn enqueue_zip(
    st: &SharedState,
    game_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    bytes: Vec<u8>,
    policy: ImportPolicy,
    upload_reservation: tokio::sync::SemaphorePermit<'static>,
) -> AppResult<axum::response::Response> {
    let st = st.clone();
    tokio::spawn(async move {
        // Once the body is complete, the operation owns both its bytes and the
        // aggregate memory permit independently of the HTTP future. A client
        // disconnect can neither orphan the stored blob nor free its budget
        // while admission is still committing.
        let _upload_reservation = upload_reservation;
        enqueue_zip_owned(&st, game_id, actor_user_id, operation_id, bytes, policy).await
    })
    .await
    .map_err(|error| AppError::internal(format!("ZIP admission task failed: {error}")))?
}

async fn enqueue_zip_owned(
    st: &SharedState,
    game_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    bytes: Vec<u8>,
    policy: ImportPolicy,
) -> AppResult<axum::response::Response> {
    let hash = sha256_hex(&bytes);
    let source_key = match pending_submitter(policy) {
        Some(submitter) => format!("zip:{hash}:pending:{submitter}"),
        None => format!("zip:{hash}:trusted"),
    };
    let admitted = begin_admitted(st, game_id, actor_user_id, operation_id, &source_key).await;
    let mut transaction = match admitted {
        Ok(Ok(transaction)) => transaction,
        Ok(Err(job_id)) => return Ok(accepted(load_job_model(st, job_id).await?)),
        Err(AppError::ServiceUnavailable(_)) => return Ok(busy()),
        Err(error) => return Err(error),
    };
    let job_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "ChallengeImportJobs"
              (id, game_id, actor_user_id, operation_id, source_kind, import_policy,
               source_key, source_staged)
            VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE)"#,
    )
    .bind(job_id)
    .bind(game_id)
    .bind(actor_user_id)
    .bind(operation_id)
    .bind(SOURCE_ZIP)
    .bind(policy_code(policy))
    .bind(&source_key)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    // Deployment-wide/per-event admission now exists durably before object
    // storage is touched. A disconnected request cannot create an uncounted
    // staged blob, and workers ignore the reservation until publication.
    let staged = crate::services::blob_refs::stage_blob(
        st.pg(),
        st.storage.as_ref(),
        crate::services::blob_refs::scoped_operation_id(operation_id, "challenge-import-zip", 0),
        &format!("challenge-import-zip:{game_id}:{operation_id}"),
        Some(actor_user_id),
        "challenge-import.zip",
        &bytes,
    )
    .await;
    let staged = match staged {
        Ok(staged) => staged,
        Err(error) => {
            finish_job(st, job_id, Err(error)).await?;
            return Ok(accepted(load_job_model(st, job_id).await?));
        }
    };
    if let Err(error) = publish_zip_source(st, job_id, game_id, &source_key, &staged).await {
        finish_job(st, job_id, Err(error)).await?;
    }
    Ok(accepted(load_job_model(st, job_id).await?))
}

async fn publish_zip_source(
    st: &SharedState,
    job_id: Uuid,
    game_id: i32,
    source_key: &str,
    staged: &crate::services::blob_refs::StagedBlob,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let source_file_id =
        crate::services::blob_refs::publish_staged_blob(&mut transaction, staged).await?;
    let attached = sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET source_file_id = $2, source_staged = TRUE,
                  updated_at = clock_timestamp()
            WHERE id = $1 AND source_kind = 0 AND status = 0
              AND source_staged = FALSE AND source_file_id IS NULL"#,
    )
    .bind(job_id)
    .bind(source_file_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if attached != 1 {
        return Err(AppError::conflict(
            "challenge import staging reservation changed",
        ));
    }
    let _ = coalesce_revision(&mut transaction, job_id, game_id, SOURCE_ZIP, source_key).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}
