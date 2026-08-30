//! ZIP-source reservation and publication for durable challenge imports.

use super::*;

const STAGING_LEASE_SECONDS: i64 = 90;

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
        // The admission task owns both bytes and their aggregate memory permit,
        // so disconnecting the HTTP request cannot orphan an uncounted upload.
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
    let staging_owner = Uuid::new_v4();
    let admitted = begin_admitted(st, game_id, actor_user_id, operation_id, &source_key).await;
    let mut transaction = match admitted {
        Ok(Ok(transaction)) => transaction,
        Ok(Err(job_id)) => {
            if !claim_zip_staging(
                st.pg(),
                job_id,
                game_id,
                actor_user_id,
                operation_id,
                &source_key,
                staging_owner,
            )
            .await?
            {
                return Ok(accepted(load_job_model(st, job_id).await?));
            }
            return stage_and_publish_zip(
                st,
                job_id,
                game_id,
                actor_user_id,
                operation_id,
                source_key,
                staging_owner,
                bytes,
            )
            .await;
        }
        Err(AppError::ServiceUnavailable(_)) | Err(AppError::RetryableUnavailable { .. }) => {
            return Ok(busy())
        }
        Err(error) => return Err(error),
    };
    let job_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO "ChallengeImportJobs"
              (id, game_id, actor_user_id, operation_id, source_kind, import_policy,
               source_key, source_staged, lease_owner, lease_expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE, $8,
                    clock_timestamp() + make_interval(secs => $9))"#,
    )
    .bind(job_id)
    .bind(game_id)
    .bind(actor_user_id)
    .bind(operation_id)
    .bind(SOURCE_ZIP)
    .bind(policy_code(policy))
    .bind(&source_key)
    .bind(staging_owner)
    .bind(STAGING_LEASE_SECONDS)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    stage_and_publish_zip(
        st,
        job_id,
        game_id,
        actor_user_id,
        operation_id,
        source_key,
        staging_owner,
        bytes,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn stage_and_publish_zip(
    st: &SharedState,
    job_id: Uuid,
    game_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    source_key: String,
    staging_owner: Uuid,
    bytes: Vec<u8>,
) -> AppResult<axum::response::Response> {
    // Deployment and event admission is now durable before object storage.
    // Workers ignore this reservation until its staged blob is published.
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
            if matches!(
                &error,
                AppError::ServiceUnavailable(_)
                    | AppError::RetryableUnavailable { .. }
                    | AppError::TooManyRequests
            ) {
                defer_zip_staging(st.pg(), job_id, staging_owner).await?;
            } else {
                fail_zip_staging(st.pg(), job_id, staging_owner, &error).await?;
            }
            return Ok(accepted(load_job_model(st, job_id).await?));
        }
    };
    if let Err(error) = publish_zip_source(
        st.pg(),
        job_id,
        game_id,
        &source_key,
        staging_owner,
        &staged,
    )
    .await
    {
        fail_zip_staging(st.pg(), job_id, staging_owner, &error).await?;
    }
    Ok(accepted(load_job_model(st, job_id).await?))
}

async fn claim_zip_staging(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    game_id: i32,
    actor_user_id: Uuid,
    operation_id: Uuid,
    source_key: &str,
    staging_owner: Uuid,
) -> AppResult<bool> {
    Ok(sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET lease_owner = $6,
                  lease_expires_at = clock_timestamp() + make_interval(secs => $7),
                  updated_at = clock_timestamp()
            WHERE id = $1 AND game_id = $2 AND actor_user_id = $3
              AND source_kind = 0 AND source_key = $4
              AND status = 0 AND source_staged = FALSE
              AND (lease_expires_at IS NULL OR lease_expires_at <= clock_timestamp())
              AND operation_id = $5"#,
    )
    .bind(job_id)
    .bind(game_id)
    .bind(actor_user_id)
    .bind(source_key)
    .bind(operation_id)
    .bind(staging_owner)
    .bind(STAGING_LEASE_SECONDS)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected()
        == 1)
}

async fn defer_zip_staging(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    staging_owner: Uuid,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET lease_owner = NULL,
                  lease_expires_at = clock_timestamp() + interval '2 seconds',
                  updated_at = clock_timestamp()
            WHERE id = $1 AND lease_owner = $2
              AND source_staged = FALSE AND status = 0"#,
    )
    .bind(job_id)
    .bind(staging_owner)
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn fail_zip_staging(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    staging_owner: Uuid,
    error: &AppError,
) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET status = 3, error = $3, lease_owner = NULL,
                  lease_expires_at = NULL, updated_at = clock_timestamp()
            WHERE id = $1 AND lease_owner = $2
              AND source_staged = FALSE AND status = 0"#,
    )
    .bind(job_id)
    .bind(staging_owner)
    .bind(bounded_error(error))
    .execute(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn publish_zip_source(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    game_id: i32,
    source_key: &str,
    staging_owner: Uuid,
    staged: &crate::services::blob_refs::StagedBlob,
) -> AppResult<()> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(pool)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let source_file_id =
        crate::services::blob_refs::publish_staged_blob(&mut transaction, staged).await?;
    let attached = sqlx::query(
        r#"UPDATE "ChallengeImportJobs"
              SET source_file_id = $2, source_staged = TRUE,
                  lease_owner = NULL, lease_expires_at = NULL,
                  updated_at = clock_timestamp()
            WHERE id = $1 AND source_kind = 0 AND status = 0
              AND source_staged = FALSE AND source_file_id IS NULL
              AND lease_owner = $3 AND lease_expires_at > clock_timestamp()"#,
    )
    .bind(job_id)
    .bind(source_file_id)
    .bind(staging_owner)
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
