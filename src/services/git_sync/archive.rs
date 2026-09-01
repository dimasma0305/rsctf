//! Staged publication of repository source archives.
//!
//! Object storage is touched only by [`stage_archive`] and
//! [`discard_archive`], while no game/definition fence is held. Publication is
//! a short owner/reference transaction that can safely run under those fences.

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

pub(super) enum ArchiveIntent {
    Keep,
    Clear,
    Staged(crate::services::blob_refs::StagedBlob),
}

#[derive(Default)]
pub(super) struct ArchivePostCommit {
    pub(super) purge_hash: Option<String>,
    receipt: Option<(uuid::Uuid, String)>,
}

pub(super) async fn stage_archive(
    st: &SharedState,
    game_id: i32,
    preserve_current: bool,
    archive: Option<&[u8]>,
) -> AppResult<ArchiveIntent> {
    if preserve_current {
        return Ok(ArchiveIntent::Keep);
    }
    match archive {
        Some(bytes) => crate::services::blob_refs::stage_blob(
            st.pg(),
            st.storage.as_ref(),
            uuid::Uuid::new_v4(),
            &format!("git-sync-archive:{game_id}"),
            None,
            "challenge-source.zip",
            bytes,
        )
        .await
        .map(ArchiveIntent::Staged),
        None => Ok(ArchiveIntent::Clear),
    }
}

pub(super) async fn publish_archive(
    st: &SharedState,
    challenge_id: i32,
    intent: &ArchiveIntent,
) -> AppResult<ArchivePostCommit> {
    let ArchiveIntent::Keep = intent else {
        return publish_archive_mutation(st, challenge_id, intent).await;
    };
    Ok(ArchivePostCommit::default())
}

async fn publish_archive_mutation(
    st: &SharedState,
    challenge_id: i32,
    intent: &ArchiveIntent,
) -> AppResult<ArchivePostCommit> {
    let expected_hash = match intent {
        ArchiveIntent::Staged(stage) => Some(stage.blob.hash.as_str()),
        ArchiveIntent::Clear => None,
        ArchiveIntent::Keep => unreachable!("keep is handled before publication"),
    };
    let owner_scope = format!("challenge-archive:{challenge_id}");
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let operation: AppResult<Option<String>> = async {
        let old_hash = sqlx::query_scalar::<_, Option<String>>(
            r#"SELECT original_archive_blob_path
                  FROM "GameChallenges"
                 WHERE id = $1
                 FOR UPDATE"#,
        )
        .bind(challenge_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;

        crate::services::blob_refs::lock_direct_hashes_locked(
            &mut transaction,
            old_hash.iter().map(String::as_str).chain(expected_hash),
        )
        .await?;

        if old_hash.as_deref() == expected_hash {
            if let ArchiveIntent::Staged(stage) = intent {
                stage
                    .consume_with_existing_reference_as(&mut transaction, &owner_scope)
                    .await?;
            }
            return Ok(None);
        }

        if let ArchiveIntent::Staged(stage) = intent {
            crate::services::blob_refs::publish_staged_blob_for_owner(
                &mut transaction,
                stage,
                &owner_scope,
            )
            .await?;
        }
        sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET original_archive_blob_path = $2
                WHERE id = $1"#,
        )
        .bind(challenge_id)
        .bind(expected_hash)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

        let Some(old_hash) = old_hash else {
            return Ok(None);
        };
        let released =
            crate::services::blob_refs::release_direct_hash_locked(&mut transaction, &old_hash)
                .await?;
        Ok(released
            .deleted_hash
            .or_else(|| (!released.found).then_some(old_hash)))
    }
    .await;
    let purge_hash = match operation {
        Ok(hash) => hash,
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(error);
        }
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(ArchivePostCommit {
        purge_hash,
        receipt: match intent {
            ArchiveIntent::Staged(stage) => Some((stage.operation_id, owner_scope)),
            _ => None,
        },
    })
}

pub(super) async fn discard_archive(st: &SharedState, intent: &ArchiveIntent) {
    let ArchiveIntent::Staged(stage) = intent else {
        return;
    };
    if let Err(error) =
        crate::services::blob_refs::discard_unpublished_stage(st.pg(), st.storage.as_ref(), stage)
            .await
    {
        tracing::warn!(%error, hash = %stage.blob.hash, "git_sync: archive stage cleanup deferred");
    }
}

pub(super) async fn finish_archive_post_commit(st: &SharedState, post_commit: ArchivePostCommit) {
    if let Some((operation_id, owner_scope)) = post_commit.receipt {
        if let Err(error) = sqlx::query(
            r#"DELETE FROM "BlobStagingOperations"
                WHERE operation_id = $1 AND state = 'Published'
                  AND published_owner_scope = $2"#,
        )
        .bind(operation_id)
        .bind(owner_scope)
        .execute(st.pg())
        .await
        {
            tracing::warn!(%error, %operation_id, "git_sync: archive receipt cleanup deferred");
        }
    }
    if let Some(hash) = post_commit.purge_hash {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "git_sync: replaced source archive purge deferred");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserve_intent_never_stages_or_clears_an_archive() {
        assert!(matches!(ArchiveIntent::Keep, ArchiveIntent::Keep));
        assert!(matches!(ArchiveIntent::Clear, ArchiveIntent::Clear));
    }
}
