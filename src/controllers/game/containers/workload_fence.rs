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
async fn load_playable_definition_snapshot_once(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<DefinitionSnapshot> {
    let challenge = load_playable_challenge(st, game_id, challenge_id).await?;
    let mut lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        game_id,
        challenge_id,
    )
    .await?;
    ensure_publication_definition_current(
        lock.transaction_mut(),
        game_id,
        challenge_id,
        &challenge,
        None,
    )
    .await?;
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

pub(super) async fn load_playable_definition_snapshot(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<DefinitionSnapshot> {
    for _ in 0..3 {
        let candidate = load_playable_challenge(st, game_id, challenge_id).await?;
        if super::image_repair::prepare_queued_image(st, &candidate).await? {
            continue;
        }
        let snapshot = load_playable_definition_snapshot_once(st, game_id, challenge_id).await?;
        let repaired = match snapshot.4.as_deref() {
            Some(image) => {
                super::image_repair::repair_missing_legacy_image(st, &snapshot.0, image).await?
            }
            None => false,
        };
        if repaired {
            // A rebuild may publish a new immutable ID. Retake the complete
            // definition snapshot instead of launching the stale missing ID.
            continue;
        }
        if let Some(image) = snapshot.4.as_deref() {
            if crate::services::image_storage::reserve_runtime_image(st, &snapshot.0, image).await?
                == crate::services::image_storage::RuntimeImageReservation::Missing
            {
                // Cleanup won the image lock just before this start reserved
                // it. Retake the snapshot and let the repair path restore it.
                continue;
            }
        }
        return Ok(snapshot);
    }
    Err(AppError::unavailable(
        "The repaired challenge image could not be verified on this container host.",
    ))
}

/// Recheck the exact launch definition on the caller's publication
/// transaction. The caller first takes `definition_lock_key` on this same
/// connection, so a later save/rollout must observe the published owner.
pub(super) async fn ensure_publication_definition_current(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
    snapshot: &game_challenge::Model,
    selected_static_flag: Option<&str>,
) -> AppResult<()> {
    let unchanged: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "GameChallenges"
                WHERE id = $1 AND game_id = $2
                  AND is_enabled = TRUE AND deletion_pending = FALSE
                  AND review_status = $3 AND "Type" = $4
                  AND workload_spec IS NOT DISTINCT FROM $5
                  AND build_status = $6
                  AND build_image_digest IS NOT DISTINCT FROM $7
                  AND memory_limit IS NOT DISTINCT FROM $8
                  AND storage_limit IS NOT DISTINCT FROM $9
                  AND cpu_count IS NOT DISTINCT FROM $10
                  AND expose_port IS NOT DISTINCT FROM $11
                  AND enable_shared_container = $12
                  AND ad_self_hosted = $13
                  AND ad_allow_egress = $14
                  AND network_mode IS NOT DISTINCT FROM $15
                  AND flag_template IS NOT DISTINCT FROM $16
           )"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(snapshot.challenge_type as i16)
    .bind(snapshot.workload_spec.clone())
    .bind(snapshot.build_status as i16)
    .bind(snapshot.build_image_digest.as_deref())
    .bind(snapshot.memory_limit)
    .bind(snapshot.storage_limit)
    .bind(snapshot.cpu_count)
    .bind(snapshot.expose_port)
    .bind(snapshot.enable_shared_container)
    .bind(snapshot.ad_self_hosted)
    .bind(snapshot.ad_allow_egress)
    .bind(snapshot.network_mode.map(|mode| mode as i16))
    .bind(snapshot.flag_template.as_deref())
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !unchanged {
        return Err(AppError::conflict(
            "challenge workload changed while the container was starting; retry",
        ));
    }
    let selected_flag_still_exists = match selected_static_flag {
        Some(flag) => sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                   SELECT 1 FROM "FlagContexts"
                    WHERE challenge_id = $1 AND flag = $2
               )"#,
        )
        .bind(challenge_id)
        .bind(flag)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?,
        None => true,
    };
    if !selected_flag_still_exists {
        return Err(AppError::conflict(
            "the selected static flag changed while the container was starting; retry",
        ));
    }
    Ok(())
}

/// Shared challenges use the same fence but a stricter eligibility reload.
async fn load_shared_definition_snapshot_once(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<DefinitionSnapshot> {
    let challenge = load_eligible_shared_challenge(st, challenge_id).await?;
    let mut lock = crate::services::challenge_workloads::acquire_definition_lock(
        st.pg(),
        game_id,
        challenge_id,
    )
    .await?;
    ensure_publication_definition_current(
        lock.transaction_mut(),
        game_id,
        challenge_id,
        &challenge,
        None,
    )
    .await?;
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

pub(super) async fn load_shared_definition_snapshot(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<DefinitionSnapshot> {
    for _ in 0..3 {
        let candidate = load_eligible_shared_challenge(st, challenge_id).await?;
        if super::image_repair::prepare_queued_image(st, &candidate).await? {
            continue;
        }
        let snapshot = load_shared_definition_snapshot_once(st, game_id, challenge_id).await?;
        let repaired = match snapshot.4.as_deref() {
            Some(image) => {
                super::image_repair::repair_missing_legacy_image(st, &snapshot.0, image).await?
            }
            None => false,
        };
        if repaired {
            continue;
        }
        if let Some(image) = snapshot.4.as_deref() {
            if crate::services::image_storage::reserve_runtime_image(st, &snapshot.0, image).await?
                == crate::services::image_storage::RuntimeImageReservation::Missing
            {
                continue;
            }
        }
        return Ok(snapshot);
    }
    Err(AppError::unavailable(
        "The repaired shared challenge image could not be verified on this container host.",
    ))
}
