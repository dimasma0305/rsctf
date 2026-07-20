use super::*;

pub(super) const BUILD_FINGERPRINT_SQL: &str = r#"SELECT challenge.container_image,
              challenge.original_archive_blob_path,
              challenge.build_context_subdir
         FROM "GameChallenges" challenge
         JOIN "Games" game ON game.id = challenge.game_id
        WHERE challenge.id = $1
          AND challenge.deletion_pending = FALSE
          AND game.deletion_pending = FALSE"#;

pub(super) const PUBLISH_BUILD_OUTCOME_SQL: &str = r#"UPDATE "GameChallenges" challenge
      SET build_status = $2,
          last_build_log = $3,
          build_image_digest = $4
    WHERE challenge.id = $1
      AND challenge.deletion_pending = FALSE
      AND EXISTS (
            SELECT 1 FROM "Games" game
             WHERE game.id = challenge.game_id
               AND game.deletion_pending = FALSE
      )
      AND challenge.container_image IS NOT DISTINCT FROM $5
      AND challenge.original_archive_blob_path IS NOT DISTINCT FROM $6
      AND challenge.build_context_subdir IS NOT DISTINCT FROM $7"#;

pub(super) const UPSERT_IMAGE_OWNERSHIP_SQL: &str = r#"INSERT INTO "BuildImageOwnerships"
 (installation_scope, canonical_ref, image_id, updated_at_utc)
 VALUES ($1, $2, $3, clock_timestamp())
 ON CONFLICT (installation_scope, canonical_ref) DO UPDATE
 SET image_id=EXCLUDED.image_id, updated_at_utc=clock_timestamp()"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BuildFingerprint {
    pub(super) container_image: Option<String>,
    pub(super) original_archive_blob_path: Option<String>,
    pub(super) build_context_subdir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct BuildImageOwnership {
    pub(super) installation_scope: String,
    pub(super) canonical_ref: String,
    pub(super) image_id: String,
}

impl BuildFingerprint {
    pub(super) fn from_challenge(challenge: &game_challenge::Model) -> Self {
        Self {
            container_image: challenge.container_image.clone(),
            original_archive_blob_path: challenge.original_archive_blob_path.clone(),
            build_context_subdir: challenge.build_context_subdir.clone(),
        }
    }
}

pub(super) fn superseded_build_outcome(message: &str) -> BuildOutcome {
    BuildOutcome {
        status: ChallengeBuildStatus::Failed,
        log: Some(message.to_string()),
        image_digest: None,
    }
}

/// Publish the result only while ordered against every runtime-definition
/// writer. The slow Docker/blob work has already completed before this lock is
/// acquired, so the advisory transaction remains short.
pub(super) async fn publish_build_outcome(
    st: &SharedState,
    challenge: &game_challenge::Model,
    requested: &BuildFingerprint,
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
    let result = sqlx::query(PUBLISH_BUILD_OUTCOME_SQL)
        .bind(challenge.id)
        .bind(outcome.status as i16)
        .bind(outcome.log.clone())
        .bind(outcome.image_digest.clone())
        .bind(&requested.container_image)
        .bind(&requested.original_archive_blob_path)
        .bind(&requested.build_context_subdir)
        .execute(&mut **definition_lock.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let rows_affected = result.rows_affected();
    if rows_affected == 1 {
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
    Ok(rows_affected)
}
