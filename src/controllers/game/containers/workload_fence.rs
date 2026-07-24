use rsctf_worker_protocol::ValidatedWorkloadSpec;

use super::*;

type DefinitionSnapshot = (
    game_challenge::Model,
    Option<ValidatedWorkloadSpec>,
    String,
    String,
    Option<String>,
);

/// Take the definition used by a per-team launch while ordered against a
/// concurrent workload save. The advisory guard is deliberately released
/// before the backend launch begins.
pub(super) async fn load_playable_definition_snapshot(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<DefinitionSnapshot> {
    let lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        game_id,
        challenge_id,
    )
    .await?;
    let challenge = load_playable_challenge(st, game_id, challenge_id).await?;
    let runtime = crate::services::challenge_workloads::resolve_runtime(st, &challenge)?;
    lock.release().await?;
    Ok((
        challenge,
        runtime.workload,
        runtime.identity,
        runtime.publication_fence,
        runtime.legacy_image,
    ))
}

/// Fence publication of a per-team runtime. Holding the returned lock until
/// its container row and instance link commit guarantees that a later rollout
/// query observes it.
pub(super) async fn acquire_playable_publication_lock(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    snapshot_fence: &str,
    selected_static_flag: Option<&str>,
) -> AppResult<crate::utils::single_flight::PgAdvisoryLock> {
    let mut lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        game_id,
        challenge_id,
    )
    .await?;
    let current = load_playable_challenge(st, game_id, challenge_id).await?;
    let current_runtime = crate::services::challenge_workloads::resolve_runtime(st, &current)?;
    crate::services::challenge_workloads::ensure_definition_unchanged(
        snapshot_fence,
        &current_runtime.publication_fence,
    )?;
    crate::services::challenge_workloads::ensure_selected_static_flag_current(
        &mut lock,
        challenge_id,
        selected_static_flag,
    )
    .await?;
    Ok(lock)
}

/// Shared challenges use the same fence but a stricter eligibility reload.
pub(super) async fn load_shared_definition_snapshot(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<DefinitionSnapshot> {
    let lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        game_id,
        challenge_id,
    )
    .await?;
    let challenge = load_eligible_shared_challenge(st, challenge_id).await?;
    let runtime = crate::services::challenge_workloads::resolve_runtime(st, &challenge)?;
    lock.release().await?;
    Ok((
        challenge,
        runtime.workload,
        runtime.identity,
        runtime.publication_fence,
        runtime.legacy_image,
    ))
}

/// Return the fresh challenge together with the publication lock because the
/// shared-container transaction also updates its ownership pointer.
pub(super) async fn acquire_shared_publication_lock(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    snapshot_fence: &str,
    selected_static_flag: Option<&str>,
) -> AppResult<(
    crate::utils::single_flight::PgAdvisoryLock,
    game_challenge::Model,
)> {
    let mut lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        game_id,
        challenge_id,
    )
    .await?;
    let current = load_eligible_shared_challenge(st, challenge_id).await?;
    let current_runtime = crate::services::challenge_workloads::resolve_runtime(st, &current)?;
    crate::services::challenge_workloads::ensure_definition_unchanged(
        snapshot_fence,
        &current_runtime.publication_fence,
    )?;
    crate::services::challenge_workloads::ensure_selected_static_flag_current(
        &mut lock,
        challenge_id,
        selected_static_flag,
    )
    .await?;
    Ok((lock, current))
}
