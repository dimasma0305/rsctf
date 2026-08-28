//! Durable challenge deletion without retaining database locks across teardown.

use super::*;

fn delete_fence_keys(game_id: i32, challenge_id: i32) -> [String; 3] {
    [
        crate::services::challenge_workloads::runtime_transition_lock_key(challenge_id),
        crate::services::ad_engine::game_lock_key(game_id),
        crate::services::challenge_workloads::definition_lock_key(game_id, challenge_id),
    ]
}

async fn acquire_delete_fences(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    for key in delete_fence_keys(game_id, challenge_id) {
        crate::utils::single_flight::acquire_transaction_advisory_lock(transaction, &key)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

async fn fence_for_external_teardown(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<game_challenge::Model> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    acquire_delete_fences(&mut transaction, game_id, challenge_id).await?;
    let challenge = load_challenge_locked(&mut transaction, game_id, challenge_id).await?;
    if challenge.challenge_type.uses_ad_engine()
        && ad_epoch_scoring_started_locked(&mut transaction, game_id).await?
    {
        return Err(AppError::bad_request(
            "A&D/KotH challenges cannot be deleted after epoch scoring has started.",
        ));
    }
    deletion::fence_challenge_deletion(&mut transaction, game_id, challenge_id).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(challenge)
}

async fn delete_fenced_row(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<crate::services::blob_refs::DeletedChallengeArtifacts> {
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    acquire_delete_fences(&mut transaction, game_id, challenge_id).await?;
    deletion::fence_challenge_deletion(&mut transaction, game_id, challenge_id).await?;
    let deleted =
        crate::services::blob_refs::delete_challenge_locked(&mut transaction, challenge_id).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(deleted)
}

/// `DELETE /api/edit/games/{id}/challenges/{cId}` — void.
pub async fn delete_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    delete_challenge_core(st, id, c_id, true).await
}

pub(crate) async fn delete_challenge_core(
    st: SharedState,
    id: i32,
    c_id: i32,
    reconcile_scoreboards: bool,
) -> AppResult<MessageResponse> {
    // The permit bounds Docker/VPN cleanup, but it owns no database connection.
    let _deletion_admission =
        super::super::deletion_locks::acquire_hard_deletion_admission().await?;
    // Commit the deny-new-work tombstone while all three writer domains are
    // held on one transaction. No pooled connection survives this phase.
    let challenge = fence_for_external_teardown(&st, id, c_id).await?;

    // Every slow or externally controlled action runs after the durable fence.
    // An in-flight test launch must reacquire the definition fence before it can
    // publish, observes deletion_pending, and destroys its unpublished backend.
    if challenge.challenge_type.uses_ad_engine() {
        if challenge.challenge_type == ChallengeType::KingOfTheHill {
            crate::services::ad_engine::clear_challenge_control(&st.db, id, c_id).await?;
        }
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    }
    if challenge.challenge_type.is_container() {
        destroy_challenge_containers(&st, &challenge, false, true).await?;
    }
    if challenge.ad_self_hosted {
        st.byoc.disconnect_challenge(&st.db, c_id).await?;
    }
    super::lifecycle::destroy_fenced_test_container(&st, c_id).await?;

    // Recheck all immutable-evidence predicates and delete atomically under the
    // same transaction-scoped writer domains. Nothing external runs here.
    let deleted_artifacts = delete_fenced_row(&st, id, c_id).await?;
    crate::services::blob_refs::purge_deleted_challenge_artifacts(
        st.pg(),
        st.storage.as_ref(),
        &deleted_artifacts,
    )
    .await;
    for attachment_id in deleted_artifacts.attachment_ids {
        if let Err(error) = delete_attachment(&st, attachment_id).await {
            tracing::warn!(%error, attachment_id, "deleted challenge attachment cleanup deferred");
        }
    }
    if reconcile_scoreboards {
        flush_game_scoreboards(&st, id).await;
    }
    Ok(MessageResponse::ok(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_fence_order_is_transition_then_game_then_definition() {
        let game_id = 7;
        let challenge_id = 11;
        let keys = delete_fence_keys(game_id, challenge_id);
        assert!(keys[0].contains("runtime-transition"));
        assert!(keys[1].contains("game"));
        assert!(keys[2].contains("workload-rollout"));
    }
}
