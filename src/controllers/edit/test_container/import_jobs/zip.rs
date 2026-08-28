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
        Err(AppError::ServiceUnavailable(_)) => return Ok(busy()),
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
            if matches!(
                &error,
                AppError::ServiceUnavailable(_)
                    | AppError::RetryableUnavailable { .. }
                    | AppError::TooManyRequests { .. }
            ) {
                defer_zip_staging(st.pg(), job_id, staging_owner).await?;
            } else {
                fail_zip_staging(st.pg(), job_id, staging_owner, &error).await?;
            }
            return Ok(accepted(load_job_model(st, job_id).await?));
        }
    };
    match publish_zip_source(
        st.pg(),
        job_id,
        game_id,
        &source_key,
        staging_owner,
        &staged,
    )
    .await
    {
        Ok(_) => {}
        Err(error) => fail_zip_staging(st.pg(), job_id, staging_owner, &error).await?,
    }
    Ok(accepted(load_job_model(st, job_id).await?))
}

pub(super) async fn claim_zip_staging(
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
) -> AppResult<bool> {
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
        transaction
            .rollback()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(false);
    }
    let _ = coalesce_revision(&mut transaction, job_id, game_id, SOURCE_ZIP, source_key).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use sqlx::postgres::PgPoolOptions;

    use super::*;
    use crate::storage::{BlobStorage, StoredBlob};

    #[derive(Default)]
    struct CountingStorage {
        stores: AtomicUsize,
    }

    #[async_trait]
    impl BlobStorage for CountingStorage {
        async fn store(&self, name: &str, bytes: &[u8]) -> AppResult<StoredBlob> {
            self.stores.fetch_add(1, Ordering::SeqCst);
            Ok(StoredBlob {
                hash: sha256_hex(bytes),
                size: bytes.len() as i64,
                name: name.to_string(),
            })
        }

        async fn load(&self, _hash: &str) -> AppResult<Vec<u8>> {
            Err(AppError::not_found("blob not found"))
        }

        async fn delete(&self, _hash: &str) -> AppResult<()> {
            Ok(())
        }

        async fn exists(&self, _hash: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn lost_zip_staging_owner_is_resumed_once_by_exact_replay() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("zip_replay_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "Games" (id INTEGER PRIMARY KEY);
               CREATE TABLE "AspNetUsers" (id UUID PRIMARY KEY);
               CREATE TABLE "Files" (
                   id SERIAL PRIMARY KEY,
                   hash VARCHAR(64) NOT NULL UNIQUE,
                   upload_time_utc TIMESTAMPTZ NOT NULL,
                   file_size BIGINT NOT NULL,
                   name TEXT NOT NULL,
                   reference_count BIGINT NOT NULL
               );
               CREATE TABLE "GameChallenges" (
                   id INTEGER PRIMARY KEY,
                   game_id INTEGER NOT NULL
               );"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(crate::migrations::CHALLENGE_IMPORT_JOBS_SQL)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(crate::migrations::CHALLENGE_IMPORT_STAGING_SQL)
            .execute(&pool)
            .await
            .unwrap();
        crate::services::blob_refs::test_support::install_operation_tables(&pool).await;

        let bytes = b"one immutable ZIP";
        let actor = Uuid::new_v4();
        let operation = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let old_owner = Uuid::new_v4();
        let source_key = format!("zip:{}:trusted", sha256_hex(bytes));
        sqlx::query(r#"INSERT INTO "Games" (id) VALUES (7)"#)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(r#"INSERT INTO "AspNetUsers" (id) VALUES ($1)"#)
            .bind(actor)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO "ChallengeImportJobs"
                  (id, game_id, actor_user_id, operation_id, source_kind,
                   import_policy, source_key, source_staged, lease_owner,
                   lease_expires_at)
                VALUES ($1, 7, $2, $3, 0, 0, $4, FALSE, $5,
                        clock_timestamp() - interval '1 second')"#,
        )
        .bind(job_id)
        .bind(actor)
        .bind(operation)
        .bind(&source_key)
        .bind(old_owner)
        .execute(&pool)
        .await
        .unwrap();

        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();
        let (claim_a, claim_b) = tokio::join!(
            claim_zip_staging(&pool, job_id, 7, actor, operation, &source_key, owner_a),
            claim_zip_staging(&pool, job_id, 7, actor, operation, &source_key, owner_b)
        );
        let claim_a = claim_a.unwrap();
        let claim_b = claim_b.unwrap();
        assert_ne!(claim_a, claim_b, "exactly one replay owns staging");
        let winner = if claim_a { owner_a } else { owner_b };
        assert!(
            !claim_zip_staging(
                &pool,
                job_id,
                7,
                actor,
                Uuid::new_v4(),
                &source_key,
                Uuid::new_v4()
            )
            .await
            .unwrap(),
            "a different operation identity cannot claim the reservation"
        );

        let storage = CountingStorage::default();
        let staged = crate::services::blob_refs::stage_blob(
            &pool,
            &storage,
            crate::services::blob_refs::scoped_operation_id(operation, "challenge-import-zip", 0),
            &format!("challenge-import-zip:7:{operation}"),
            Some(actor),
            "challenge-import.zip",
            bytes,
        )
        .await
        .unwrap();
        assert!(
            publish_zip_source(&pool, job_id, 7, &source_key, winner, &staged)
                .await
                .unwrap()
        );
        let replayed_stage = crate::services::blob_refs::stage_blob(
            &pool,
            &storage,
            crate::services::blob_refs::scoped_operation_id(operation, "challenge-import-zip", 0),
            &format!("challenge-import-zip:7:{operation}"),
            Some(actor),
            "challenge-import.zip",
            bytes,
        )
        .await
        .unwrap();
        assert_eq!(staged.blob.hash, replayed_stage.blob.hash);
        assert_eq!(storage.stores.load(Ordering::SeqCst), 1);
        assert!(
            !publish_zip_source(&pool, job_id, 7, &source_key, old_owner, &replayed_stage)
                .await
                .unwrap(),
            "the lost owner cannot publish over the recovered owner"
        );

        let (jobs, staged_source, lease_owner, references): (i64, bool, Option<Uuid>, i64) =
            sqlx::query_as(
                r#"SELECT (
                           SELECT COUNT(*)::bigint
                             FROM "ChallengeImportJobs" replay
                            WHERE replay.game_id = job.game_id
                              AND replay.actor_user_id = job.actor_user_id
                              AND replay.operation_id = job.operation_id
                       ), job.source_staged, job.lease_owner, file.reference_count
                 FROM "ChallengeImportJobs" job
                 JOIN "Files" file ON file.id = job.source_file_id
                WHERE job.id = $1"#,
            )
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(jobs, 1);
        assert!(staged_source);
        assert_eq!(lease_owner, None);
        assert_eq!(references, 1);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
