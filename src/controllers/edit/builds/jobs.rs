//! Durable image-build job admission and execution.
use super::*;

// The outer executor drops the complete Bollard build/pull future before this
// lease expires. Dropping those response streams closes the Engine request;
// Docker's build and image-pull APIs cancel work when that connection closes.
const DURABLE_BUILD_RESOURCE_LEASE_SECONDS: u64 = 15 * 60;
const _: () = assert!(
    crate::services::control_jobs::CONTROL_JOB_EXECUTION_BUDGET_SECONDS
        < DURABLE_BUILD_RESOURCE_LEASE_SECONDS
);

pub(crate) async fn enqueue_challenge_build_job(
    st: &SharedState,
    challenge: &game_challenge::Model,
    trigger: &str,
    attempt: i32,
    operation_id: Uuid,
) -> AppResult<crate::services::control_jobs::ControlJobModel> {
    super::super::challenges::reject_pending_mutation(st.pg(), challenge.game_id, challenge.id)
        .await?;
    let trigger = match trigger {
        "Manual" | "AutoRetry" | "Bulk" => trigger,
        _ => return Err(AppError::bad_request("unsupported build trigger")),
    };
    let fingerprint = BuildFingerprint::from_challenge(challenge).identity();
    let input = serde_json::json!({
        "challengeId": challenge.id,
        "trigger": trigger,
        "attempt": attempt.max(1),
    });
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::ChallengeBuild,
        &format!("challenge:{}", challenge.id),
        challenge.game_id,
        Some(challenge.id),
        operation_id,
        &fingerprint,
        input,
    )
    .await?;
    sqlx::query(
        r#"UPDATE "GameChallenges"
              SET build_status = $2
            WHERE id = $1 AND deletion_pending = FALSE AND build_status <> $3"#,
    )
    .bind(challenge.id)
    .bind(ChallengeBuildStatus::Queued as i16)
    .bind(ChallengeBuildStatus::Building as i16)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    crate::services::control_jobs::kick(st.clone());
    Ok(job)
}

pub(crate) async fn execute_build_batch_job(
    st: &SharedState,
    claimed: &crate::services::control_jobs::ClaimedControlJob,
) -> AppResult<serde_json::Value> {
    const PAGE_SIZE: i64 = 64;
    const MAX_CANDIDATES: usize = 256;
    let job = &claimed.model;
    let total = sqlx::query_scalar::<_, i64>(
        r#"SELECT LEAST(COUNT(*), $2) FROM "GameChallenges"
            WHERE game_id = $1 AND deletion_pending = FALSE
              AND build_status IN ($3, $4)"#,
    )
    .bind(job.game_id)
    .bind(i64::try_from(MAX_CANDIDATES).unwrap_or(i64::MAX))
    .bind(ChallengeBuildStatus::Failed as i16)
    .bind(ChallengeBuildStatus::MissingDockerfile as i16)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let progress_total = i32::try_from(total.max(1)).unwrap_or(i32::MAX);
    crate::services::control_jobs::set_progress(
        st.pg(),
        job.id,
        claimed.lease_token,
        0,
        progress_total,
    )
    .await?;
    let mut cursor = 0;
    let mut enqueued = 0usize;
    let mut skipped = 0usize;
    while enqueued + skipped < MAX_CANDIDATES {
        if crate::services::control_jobs::cancellation_requested(
            st.pg(),
            job.id,
            claimed.lease_token,
        )
        .await?
        {
            break;
        }
        let remaining = MAX_CANDIDATES - enqueued - skipped;
        let page = sqlx::query_scalar::<_, i32>(
            r#"SELECT id FROM "GameChallenges"
                WHERE game_id = $1 AND id > $2 AND deletion_pending = FALSE
                  AND build_status IN ($3, $4)
                ORDER BY id LIMIT $5"#,
        )
        .bind(job.game_id)
        .bind(cursor)
        .bind(ChallengeBuildStatus::Failed as i16)
        .bind(ChallengeBuildStatus::MissingDockerfile as i16)
        .bind(PAGE_SIZE.min(i64::try_from(remaining).unwrap_or(PAGE_SIZE)))
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if page.is_empty() {
            break;
        }
        for challenge_id in page {
            cursor = challenge_id;
            let Some(challenge) = game_challenge::Entity::find_by_id(challenge_id)
                .one(&st.db)
                .await?
                .filter(|challenge| challenge.game_id == job.game_id)
            else {
                skipped += 1;
                continue;
            };
            let attempt: i32 = sqlx::query_scalar(
                r#"SELECT COALESCE(MAX(attempt), 0) + 1
                     FROM "BuildRecords" WHERE challenge_id = $1"#,
            )
            .bind(challenge_id)
            .fetch_one(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            let seed = super::super::control_jobs::fingerprint(&format!(
                "{}:{challenge_id}",
                job.operation_id
            ))?;
            let operation_id = Uuid::parse_str(&seed[..32])
                .map_err(|error| AppError::internal(error.to_string()))?;
            let child =
                enqueue_challenge_build_job(st, &challenge, "Bulk", attempt, operation_id).await?;
            if child.operation_id == operation_id {
                enqueued += 1;
            } else {
                skipped += 1;
            }
            let current = i32::try_from(enqueued.saturating_add(skipped))
                .unwrap_or(progress_total)
                .min(progress_total);
            crate::services::control_jobs::set_progress(
                st.pg(),
                job.id,
                claimed.lease_token,
                current,
                progress_total,
            )
            .await?;
        }
    }
    Ok(serde_json::json!({
        "enqueued": enqueued,
        "skipped": skipped,
        "candidateLimit": MAX_CANDIDATES,
    }))
}

pub(crate) async fn execute_challenge_build_job(
    st: &SharedState,
    job: &crate::services::control_jobs::ControlJobModel,
    input: &serde_json::Value,
) -> AppResult<serde_json::Value> {
    let challenge_id = input
        .get("challengeId")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| AppError::internal("build job has no valid challenge id"))?;
    let trigger = input
        .get("trigger")
        .and_then(serde_json::Value::as_str)
        .filter(|value| matches!(*value, "Manual" | "AutoRetry" | "Bulk"))
        .ok_or_else(|| AppError::internal("build job has no valid trigger"))?;
    let attempt = input
        .get("attempt")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::internal("build job has no valid attempt"))?;
    let challenge = game_challenge::Entity::find_by_id(challenge_id)
        .one(&st.db)
        .await?
        .filter(|challenge| challenge.game_id == job.game_id)
        .ok_or_else(|| AppError::not_found("Challenge for build job not found"))?;
    if BuildFingerprint::from_challenge(&challenge).identity() != job.fingerprint {
        return Err(AppError::conflict(
            "Challenge build definition changed after this job was queued",
        ));
    }
    let (outcome, record) =
        run_durable_challenge_build(st, &challenge, trigger, attempt, job.id).await;
    if outcome.status == ChallengeBuildStatus::Queued {
        return Err(AppError::unavailable(
            outcome
                .log
                .unwrap_or_else(|| "Container runtime is unavailable".to_string()),
        ));
    }
    if outcome.status != ChallengeBuildStatus::Success {
        return Err(AppError::conflict(
            outcome
                .log
                .clone()
                .unwrap_or_else(|| "Challenge image build failed".to_string()),
        ));
    }
    Ok(serde_json::json!({
        "buildStatus": outcome.status,
        "lastBuildLog": outcome.log,
        "imageDigest": outcome.image_digest,
        "auditId": record.map(|record| record.id),
    }))
}

async fn run_durable_challenge_build(
    st: &SharedState,
    challenge: &game_challenge::Model,
    trigger: &str,
    attempt: i32,
    job_id: Uuid,
) -> (BuildOutcome, Option<build_record::Model>) {
    let started = Utc::now();
    let resource_key = build_lock_key(challenge);
    let canonical_ref = challenge
        .container_image
        .as_deref()
        .and_then(canonical_managed_image_tag);
    let acquired = try_acquire_durable_build_resource(
        st.pg(),
        canonical_ref.as_deref(),
        &resource_key,
        job_id,
    )
    .await;
    match acquired {
        Ok(true) => {}
        Ok(false) => {
            let outcome = superseded_build_outcome(
                "Another bounded image job owns this build resource; retry after it completes.",
            );
            let record = record_build(st, challenge, trigger, attempt, started, &outcome).await;
            return (outcome, record);
        }
        Err(error) => {
            let outcome = superseded_build_outcome(&format!(
                "Build resource admission is unavailable: {error}"
            ));
            let record = record_build(st, challenge, trigger, attempt, started, &outcome).await;
            return (outcome, record);
        }
    }
    let requested = BuildFingerprint::from_challenge(challenge);
    let current = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
        BUILD_FINGERPRINT_SQL,
    )
    .bind(challenge.id)
    .fetch_optional(st.pg())
    .await;
    let mut outcome = match current {
        Ok(Some((container_image, original_archive_blob_path, build_context_subdir))) => {
            let current = BuildFingerprint {
                container_image,
                original_archive_blob_path,
                build_context_subdir,
            };
            if current == requested {
                build_challenge_image(st, challenge).await
            } else {
                superseded_build_outcome(
                    "Build cancelled because the challenge definition changed while it was queued.",
                )
            }
        }
        Ok(None) => superseded_build_outcome(
            "Build cancelled because the challenge or event is being deleted.",
        ),
        Err(error) => superseded_build_outcome(&format!(
            "Build definition could not be revalidated: {error}"
        )),
    };
    let ownership = if outcome.status == ChallengeBuildStatus::Success {
        match resolve_build_image_ownership(challenge, &outcome).await {
            Ok(ownership) => ownership,
            Err(error) => {
                outcome = superseded_build_outcome(&format!(
                    "The image completed but ownership validation failed: {error}"
                ));
                None
            }
        }
    } else {
        None
    };
    match publish_build_outcome(st, challenge, &requested, &outcome, ownership.as_ref()).await {
        Ok(1) => {}
        Ok(_) => {
            outcome = superseded_build_outcome(
                "Build result discarded because the challenge definition changed while it ran.",
            );
        }
        Err(error) => {
            outcome = superseded_build_outcome(&format!(
                "The image completed but its status could not be published: {error}"
            ));
        }
    }
    if let Err(error) =
        crate::services::control_jobs::release_resource(st.pg(), &resource_key, job_id).await
    {
        tracing::warn!(%job_id, %error, "build resource lease release failed");
    }
    let record = record_build(st, challenge, trigger, attempt, started, &outcome).await;
    (outcome, record)
}

async fn try_acquire_durable_build_resource(
    pool: &sqlx::PgPool,
    canonical_ref: Option<&str>,
    resource_key: &str,
    job_id: Uuid,
) -> AppResult<bool> {
    // Take the same image lock as interactive builds and cleanup finalization.
    // The durable resource lease is installed before releasing it, closing the
    // gap where cleanup could otherwise start removing a tag being rebuilt.
    let mut image_lock =
        crate::utils::single_flight::PgAdvisoryLock::acquire_build(pool, resource_key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    if let Err(error) =
        ensure_cleanup_not_finalizing(image_lock.connection_mut(), canonical_ref).await
    {
        drop(image_lock);
        return Err(error);
    }
    let acquired = crate::services::control_jobs::try_acquire_resource_on(
        image_lock.connection_mut(),
        resource_key,
        job_id,
        std::time::Duration::from_secs(DURABLE_BUILD_RESOURCE_LEASE_SECONDS),
    )
    .await;
    if let Err(error) = image_lock.release().await {
        // The connection is close-on-drop, so the session lock is gone even if
        // the explicit unlock response was lost. The durable lease remains the
        // authoritative exclusion fence.
        tracing::warn!(%job_id, %error, "durable build image-lock release failed");
    }
    acquired
}

#[cfg(test)]
mod tests {
    use super::super::identity::FINALIZING_IMAGE_CLAIM_SQL;
    use super::*;

    #[test]
    fn durable_build_admission_checks_the_live_finalizing_phase() {
        assert!(FINALIZING_IMAGE_CLAIM_SQL.contains("cleanup_removal_started = TRUE"));
        assert!(FINALIZING_IMAGE_CLAIM_SQL.contains("cleanup_claim_until > clock_timestamp()"));
        let interactive = include_str!("../builds.rs");
        assert!(interactive.contains("ensure_cleanup_not_finalizing("));
        let generator = include_str!("generator.rs");
        assert!(generator.contains("ensure_cleanup_not_finalizing("));
    }

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn durable_build_lease_accepts_a_preclaim_but_refuses_finalizing_cleanup() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with(crate::migrations::test_pg_connect_options(&database_url))
            .await
            .unwrap();
        let schema = format!("durable_build_cleanup_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(
                crate::migrations::test_pg_connect_options(&database_url)
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"CREATE TABLE "BuildImageOwnerships" (
                 installation_scope TEXT NOT NULL,
                 canonical_ref TEXT NOT NULL,
                 image_id TEXT NOT NULL,
                 cleanup_claim_token UUID NULL,
                 cleanup_claim_until TIMESTAMPTZ NULL,
                 cleanup_removal_started BOOLEAN NOT NULL DEFAULT FALSE,
                 PRIMARY KEY (installation_scope, canonical_ref),
                 CHECK (NOT cleanup_removal_started OR
                        (cleanup_claim_token IS NOT NULL AND cleanup_claim_until IS NOT NULL))
               );
               CREATE TABLE "ControlPlaneResourceLeases" (
                 resource_key TEXT PRIMARY KEY,
                 owner_job_id UUID NOT NULL,
                 lease_expires_at_utc TIMESTAMPTZ NOT NULL
               )"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let scope = crate::services::container::docker_installation_scope();
        let canonical = "docker.io/rsctf/game/durable:latest";
        let resource_key = image_build_lock_key(Some(canonical));
        let cleanup_token = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO "BuildImageOwnerships"
                 (installation_scope, canonical_ref, image_id,
                  cleanup_claim_token, cleanup_claim_until)
               VALUES ($1, $2, $3, $4, clock_timestamp() + interval '2 minutes')"#,
        )
        .bind(&scope)
        .bind(canonical)
        .bind(format!("sha256:{}", "a".repeat(64)))
        .bind(cleanup_token)
        .execute(&pool)
        .await
        .unwrap();

        let preclaim_job = Uuid::new_v4();
        assert!(try_acquire_durable_build_resource(
            &pool,
            Some(canonical),
            &resource_key,
            preclaim_job,
        )
        .await
        .unwrap());
        crate::services::control_jobs::release_resource(&pool, &resource_key, preclaim_job)
            .await
            .unwrap();

        sqlx::query(
            r#"UPDATE "BuildImageOwnerships"
                  SET cleanup_removal_started = TRUE
                WHERE installation_scope = $1 AND canonical_ref = $2"#,
        )
        .bind(&scope)
        .bind(canonical)
        .execute(&pool)
        .await
        .unwrap();
        let blocked_job = Uuid::new_v4();
        let error =
            try_acquire_durable_build_resource(&pool, Some(canonical), &resource_key, blocked_job)
                .await
                .unwrap_err();
        assert!(error.to_string().contains("cleanup is finalizing"));
        let leaked: i64 = sqlx::query_scalar(
            r#"SELECT COUNT(*) FROM "ControlPlaneResourceLeases"
                WHERE resource_key = $1 AND owner_job_id = $2"#,
        )
        .bind(&resource_key)
        .bind(blocked_job)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(leaked, 0);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
    }
}
