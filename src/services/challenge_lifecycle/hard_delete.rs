//! Ordered, recoverable hard deletion after organizer authorization.

use super::{deletion, destroy_challenge_containers, destroy_test_container_locked};
use crate::app_state::SharedState;
use crate::services::ad::koth_capability_cache::{
    begin_game_epoch_mutation_if, finish_game_epoch_mutation_if_any,
};
use crate::services::asset_invalidation::delete_attachment;
use crate::services::challenge_mutations::{
    ad_epoch_scoring_started_locked, load_challenge_locked,
};
use crate::services::scoreboard_invalidation::flush_game_scoreboards;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};

pub(crate) async fn delete_challenge_core(
    st: SharedState,
    id: i32,
    c_id: i32,
    reconcile_scoreboards: bool,
    reconcile_vpn: bool,
) -> AppResult<()> {
    // Share the hard-deletion admission domain with whole-game deletion before
    // retaining the outer runtime-transition transaction.
    let deletion_admission =
        crate::services::deletion_locks::acquire_hard_deletion_admission().await?;
    // Take the same transition -> game -> definition order as false -> true
    // edits. The transition fence remains held through physical teardown so no
    // replica can re-enable the challenge behind a stale cleanup snapshot.
    let runtime_transition =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(st.pg(), c_id)
            .await?;
    let mut engine_control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        engine_control.transaction_mut(),
        &crate::services::challenge_workloads::definition_lock_key(id, c_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let challenge = load_challenge_locked(engine_control.transaction_mut(), id, c_id).await?;
    if challenge.challenge_type.uses_ad_engine()
        && ad_epoch_scoring_started_locked(engine_control.transaction_mut(), id).await?
    {
        return Err(AppError::bad_request(
            "A&D/KotH challenges cannot be deleted after epoch scoring has started.",
        ));
    }
    let cache_mutation = begin_game_epoch_mutation_if(
        st.cache.as_ref(),
        id,
        challenge.challenge_type == ChallengeType::KingOfTheHill,
    )
    .await?;

    // The JFLG-exclusive predicate and the durable disabled marker share the
    // definition-lock transaction. This preserves Jeopardy history once play
    // could have started and closes an in-flight-submit TOCTOU. Committing the
    // short definition mutation before runtime I/O also keeps the pool bounded.
    if let Err(error) =
        deletion::fence_challenge_deletion(engine_control.transaction_mut(), id, c_id).await
    {
        // This transaction has not attempted commit, so every error rolls back.
        finish_game_epoch_mutation_if_any(st.cache.as_ref(), id, cache_mutation).await;
        return Err(error);
    }

    // Revoke A&D/KotH routes before any backing address can be freed.
    if challenge.challenge_type.uses_ad_engine()
        && challenge.challenge_type == ChallengeType::KingOfTheHill
    {
        crate::services::ad_engine::clear_challenge_control_locked(
            &mut **engine_control.transaction_mut(),
            id,
            c_id,
        )
        .await?;
    }
    engine_control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // The durable disabled tombstone is now visible. Later projection/runtime
    // cleanup cannot make this challenge eligible to a token reader again.
    finish_game_epoch_mutation_if_any(st.cache.as_ref(), id, cache_mutation).await;
    if challenge.challenge_type.uses_ad_engine() && reconcile_vpn {
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    }

    // Tear down every running per-team + shared container this challenge owns
    // BEFORE its rows vanish — otherwise they run orphaned until the idle reaper.
    // Mirrors RSCTF `RemoveChallenge`'s container sweep (gated on container type).
    if challenge.challenge_type.is_container() {
        destroy_challenge_containers(&st, &challenge, false, true).await?;
    }
    if challenge.ad_self_hosted {
        if reconcile_vpn {
            st.byoc.disconnect_challenge(&st.db, c_id).await?;
        } else {
            st.byoc
                .disconnect_challenge_deferred_vpn(&st.db, c_id)
                .await?;
        }
    }

    // Reacquire game control before the test/definition gates. Engine writers
    // that do not touch a participation row still serialize with the final
    // evidence predicate and physical delete, and the established game -> test
    // -> definition order avoids cross-replica lock inversion.
    let mut final_locks = crate::services::deletion_locks::acquire_game_test_deletion_locks(
        &st.db,
        id,
        deletion_admission,
    )
    .await?;

    // Re-query under the shared game/test lock stack so a test created during
    // the earlier sweep cannot publish behind challenge deletion.
    destroy_test_container_locked(&st, c_id).await?;

    // Reacquire definition on the game/test guard's existing connection. Test
    // creation uses test-lifecycle -> definition, so taking the same order here
    // avoids inversion while making the final attachment snapshot and physical
    // delete indivisible without reserving a nested pool checkout.
    crate::utils::single_flight::acquire_transaction_advisory_lock(
        final_locks.game_transaction_mut(),
        &crate::services::challenge_workloads::definition_lock_key(id, c_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    deletion::fence_challenge_deletion(final_locks.game_transaction_mut(), id, c_id).await?;
    let deleted_artifacts = crate::services::blob_refs::delete_challenge_locked(
        final_locks.game_transaction_mut(),
        c_id,
    )
    .await?;
    final_locks.release().await?;
    runtime_transition.release().await?;

    crate::services::blob_refs::purge_deleted_challenge_artifacts(
        st.pg(),
        st.storage.as_ref(),
        &deleted_artifacts,
    )
    .await;

    // Release the now-orphaned attachment blobs (clear-FK-first: rows above are
    // already gone).
    for aid in deleted_artifacts.attachment_ids {
        if let Err(error) = delete_attachment(&st, aid).await {
            tracing::warn!(%error, attachment_id = aid, "deleted challenge attachment cleanup deferred");
        }
    }
    if reconcile_scoreboards {
        flush_game_scoreboards(&st, id).await;
    }
    Ok(())
}
