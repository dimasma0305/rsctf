//! Durable image-build job admission and execution.
use super::*;

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
    job: &crate::services::control_jobs::ControlJobModel,
) -> AppResult<serde_json::Value> {
    const PAGE_SIZE: i64 = 64;
    const MAX_CANDIDATES: usize = 256;
    let mut cursor = 0;
    let mut enqueued = 0usize;
    let mut skipped = 0usize;
    while enqueued + skipped < MAX_CANDIDATES {
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
    let acquired = crate::services::control_jobs::try_acquire_resource(
        st.pg(),
        &resource_key,
        job_id,
        std::time::Duration::from_secs(15 * 60),
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
        Ok(Some((
            ref container_image,
            ref original_archive_blob_path,
            ref build_context_subdir,
        ))) if (BuildFingerprint {
            container_image: container_image.clone(),
            original_archive_blob_path: original_archive_blob_path.clone(),
            build_context_subdir: build_context_subdir.clone(),
        }) == requested =>
        {
            build_challenge_image(st, challenge).await
        }
        Ok(Some(_)) => superseded_build_outcome(
            "Build cancelled because the challenge definition changed while it was queued.",
        ),
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
