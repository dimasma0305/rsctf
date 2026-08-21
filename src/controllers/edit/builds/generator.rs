//! Coordinated builder for trusted repository `generator/Dockerfile` sources.

use super::*;

const GENERATOR_FINGERPRINT_SQL: &str = r#"SELECT challenge.original_archive_blob_path,
              challenge.variant_generator_build_context_subdir,
              challenge.variant_generator_build_status,
              challenge.variant_generator_image,
              challenge.variant_generator_digest
         FROM "GameChallenges" challenge
         JOIN "Games" game ON game.id = challenge.game_id
        WHERE challenge.id = $1
          AND challenge.deletion_pending = FALSE
          AND game.deletion_pending = FALSE"#;

const PUBLISH_GENERATOR_BUILD_SQL: &str = r#"UPDATE "GameChallenges" challenge
      SET variant_generator_build_status = $2,
          variant_generator_last_build_log = $3,
          variant_generator_image = $4,
          variant_generator_digest = $5
    WHERE challenge.id = $1
      AND challenge.deletion_pending = FALSE
      AND challenge.original_archive_blob_path IS NOT DISTINCT FROM $6
      AND challenge.variant_generator_build_context_subdir IS NOT DISTINCT FROM $7
      AND challenge.variant_generator_build_status IN (2, 3, 5, 6)
      AND EXISTS (
            SELECT 1 FROM "Games" game
             WHERE game.id = challenge.game_id
               AND game.deletion_pending = FALSE
               AND clock_timestamp() < game.start_time_utc
      )"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct GeneratorFingerprint {
    archive_path: Option<String>,
    context_subdir: Option<String>,
}

fn generator_tag(game_id: i32, challenge_id: i32) -> String {
    format!(
        "rsctf/{}/variant-generator-{}:latest",
        game_id, challenge_id
    )
}

fn failed(log: impl Into<String>) -> BuildOutcome {
    BuildOutcome {
        status: ChallengeBuildStatus::Failed,
        log: Some(log.into()),
        image_digest: None,
    }
}

fn append_log(outcome: &mut BuildOutcome, message: &str) {
    let log = match outcome.log.take() {
        Some(log) if !log.is_empty() => format!("{log}\n{message}"),
        _ => message.to_string(),
    };
    outcome.log = cap_build_log(log);
}

async fn publish_generator_build(
    st: &SharedState,
    challenge: &game_challenge::Model,
    requested: &GeneratorFingerprint,
    outcome: &BuildOutcome,
    ownership: Option<&BuildImageOwnership>,
) -> AppResult<u64> {
    let mut definition_lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        challenge.game_id,
        challenge.id,
    )
    .await?;
    super::super::challenges::reject_pending_mutation(
        &mut **definition_lock.transaction_mut(),
        challenge.game_id,
        challenge.id,
    )
    .await?;
    let immutable = (outcome.status == ChallengeBuildStatus::Success)
        .then(|| outcome.image_digest.clone())
        .flatten();
    let result = sqlx::query(PUBLISH_GENERATOR_BUILD_SQL)
        .bind(challenge.id)
        .bind(outcome.status as i16)
        .bind(outcome.log.clone())
        .bind(immutable.clone())
        .bind(immutable)
        .bind(&requested.archive_path)
        .bind(&requested.context_subdir)
        .execute(&mut **definition_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let rows = result.rows_affected();
    if rows == 1 {
        if let Some(ownership) = ownership {
            sqlx::query(UPSERT_IMAGE_OWNERSHIP_SQL)
                .bind(&ownership.installation_scope)
                .bind(&ownership.canonical_ref)
                .bind(&ownership.image_id)
                .execute(&mut **definition_lock.transaction_mut())
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }
    definition_lock.release().await?;
    Ok(rows)
}

/// Build and contract-test a queued repository generator after the caller has
/// released its broad per-game configuration lock.
pub(crate) async fn run_variant_generator_build(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> BuildOutcome {
    let requested = GeneratorFingerprint {
        archive_path: challenge.original_archive_blob_path.clone(),
        context_subdir: challenge.variant_generator_build_context_subdir.clone(),
    };
    if requested.context_subdir.as_deref()
        != Some(crate::services::git_sync::GENERATOR_CONTEXT_SUBDIR)
    {
        return failed("Challenge has no repository generator build context.");
    }

    let tag = generator_tag(challenge.game_id, challenge.id);
    let lock_key = image_build_lock_key(Some(&tag));
    let mut build_lock = match crate::utils::single_flight::PgAdvisoryLock::acquire_build(
        st.pg(),
        &lock_key,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => return failed(format!("Generator build coordination failed: {error}")),
    };
    let current = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<String>,
            i16,
            Option<String>,
            Option<String>,
        ),
    >(GENERATOR_FINGERPRINT_SQL)
    .bind(challenge.id)
    .fetch_optional(build_lock.connection_mut())
    .await;
    let (archive_path, context_subdir, status, image, digest) = match current {
        Ok(Some(current)) => current,
        Ok(None) => {
            let _ = build_lock.release().await;
            return failed("Generator build was cancelled because the challenge was deleted.");
        }
        Err(error) => {
            drop(build_lock);
            return failed(format!("Generator build fingerprint read failed: {error}"));
        }
    };
    if requested
        != (GeneratorFingerprint {
            archive_path,
            context_subdir,
        })
    {
        let _ = build_lock.release().await;
        return failed(
            "Generator build was superseded because its source archive or context changed.",
        );
    }
    if status == ChallengeBuildStatus::Success as i16 {
        if let (Some(image), Some(digest)) = (image, digest) {
            if image == digest && st.containers.image_exists(&image).await {
                let _ = build_lock.release().await;
                return BuildOutcome {
                    status: ChallengeBuildStatus::Success,
                    log: Some("Generator was already built by another scan.".to_string()),
                    image_digest: Some(image),
                };
            }
        }
        let _ = build_lock.release().await;
        return failed("Published generator identity is unavailable; rescan to queue a repair.");
    }
    if status != ChallengeBuildStatus::Queued as i16 {
        let _ = build_lock.release().await;
        return failed("Generator build is no longer queued for this source definition.");
    }

    let mut build_definition = challenge.clone();
    build_definition.challenge_type = ChallengeType::StaticAttachment;
    build_definition.workload_spec = None;
    build_definition.container_image = Some(tag);
    build_definition.build_context_subdir = requested.context_subdir.clone();
    build_definition.build_status = ChallengeBuildStatus::Queued;
    build_definition.build_image_digest = None;
    let mut outcome = build_challenge_image(st, &build_definition).await;
    if outcome.status == ChallengeBuildStatus::Success {
        let immutable = outcome.image_digest.as_deref();
        if !immutable.is_some_and(crate::services::challenge_images::is_local_image_id) {
            outcome = failed(
                "Repository generator build did not resolve to a daemon-local immutable image id.",
            );
        } else if let Some(immutable) = immutable {
            match crate::services::event_security::validate_built_variant_generator(
                st, immutable, immutable,
            )
            .await
            {
                Ok(()) => append_log(
                    &mut outcome,
                    "Generator contract and deterministic replay checks passed.",
                ),
                Err(error) => {
                    outcome = failed(format!("Generator contract check failed: {error}"));
                }
            }
        }
    }
    let ownership = if outcome.status == ChallengeBuildStatus::Success {
        match resolve_build_image_ownership(&build_definition, &outcome).await {
            Ok(ownership) => ownership,
            Err(error) => {
                outcome = failed(format!(
                    "Generator built, but its managed image ownership could not be proven: {error}"
                ));
                None
            }
        }
    } else {
        None
    };
    let published =
        publish_generator_build(st, challenge, &requested, &outcome, ownership.as_ref()).await;
    let unlocked = build_lock.release().await;
    match (published, unlocked) {
        (Ok(1), Ok(())) => outcome,
        (Ok(1), Err(error)) => {
            tracing::warn!(challenge = challenge.id, %error, "generator image lock release failed");
            outcome
        }
        (Ok(_), unlock_result) => {
            if let Err(error) = unlock_result {
                tracing::warn!(challenge = challenge.id, %error, "superseded generator unlock failed");
            }
            failed(
                "Generator result was discarded because the source changed or the event started while it was building.",
            )
        }
        (Err(error), unlock_result) => {
            if let Err(unlock_error) = unlock_result {
                tracing::warn!(challenge = challenge.id, %unlock_error, "failed generator publication unlock failed");
            }
            failed(format!("Generator result could not be published: {error}"))
        }
    }
}

/// Repository/archive import adapter with one compact error surface for every
/// caller that drains build jobs after releasing its game lock.
pub(crate) async fn run_import_variant_generator_build(
    st: &SharedState,
    challenge_id: i32,
) -> Result<(), String> {
    // The builder consumes the complete enum-rich challenge model. Retain this
    // single primary-key ORM hydration rather than duplicating every model
    // column in a fragile raw-SQL row mapping.
    let challenge = game_challenge::Entity::find_by_id(challenge_id)
        .one(&st.db)
        .await
        .map_err(|error| format!("generator build lookup failed: {error}"))?
        .ok_or_else(|| "challenge disappeared before its generator build".to_string())?;
    let outcome = run_variant_generator_build(st, &challenge).await;
    if outcome.status == ChallengeBuildStatus::Success {
        Ok(())
    } else {
        Err(outcome
            .log
            .unwrap_or_else(|| format!("{:?}", outcome.status)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_tags_are_separate_from_challenge_runtime_tags() {
        assert_eq!(generator_tag(7, 11), "rsctf/7/variant-generator-11:latest");
    }
}
