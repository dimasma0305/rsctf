//! edit: challenge CRUD/attachments (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

mod attachments;
mod audit;
mod bulk;
mod definition_write;
mod deletion;
#[cfg(test)]
mod deletion_tests;
mod hints;
mod lifecycle;
mod mutation_sql;
#[cfg(test)]
mod simple_create_tests;
use crate::services::repository_push as repo_push;
mod review;
mod revision_effects;
#[cfg(test)]
mod revision_tests;
#[cfg(test)]
mod runtime_profile_tests;
mod scoring;
#[cfg(test)]
mod scoring_lock_tests;
mod topology_transition;
mod update_policy;
mod workload;

#[cfg(test)]
use update_policy::scoring_fields_changed as challenge_scoring_fields_changed;

pub use attachments::update_attachment;
pub(crate) use attachments::validate_remote_attachment_url;
pub use audit::{
    download_challenge_audit_archive, get_challenge_audit_meta, get_challenge_build_status,
    list_challenge_build_statuses, rebuild_challenge,
};
pub use bulk::mutate_challenges_bulk;
pub(crate) use deletion::reject_pending_mutation;
pub use hints::{release_next as release_next_hint, unrelease_last as unrelease_last_hint};
pub(crate) use lifecycle::destroy_challenge_containers;
pub use repo_push::start as start_repo_push_worker;
pub use review::{approve_challenge, list_pending_challenges, reject_challenge};
pub use revision_effects::start as start_challenge_revision_effect_worker;
pub use workload::rollout_workloads;

use crate::services::challenge_mutations::INSERTABLE_GAME_SQL;
const MAX_EDIT_CHALLENGES: u64 = 2_048;

// ============================================================================
//  Game challenges
// ============================================================================

/// `GET /api/edit/games/{id}/challenges` — Active challenges only.
pub async fn get_challenges(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<Vec<ChallengeSummaryModel>>> {
    manager_or_admin(&st, &user, id).await?;
    let configuration_revision = sqlx::query_scalar::<_, i64>(
        r#"SELECT challenge_configuration_revision FROM "Games" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(id))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .order_by_asc(game_challenge::Column::Id)
        .limit(MAX_EDIT_CHALLENGES)
        .all(&st.db)
        .await?;

    let solved_count = scoring::eligible_dynamic_solve_counts(&st, id).await?;

    let data = challenges
        .iter()
        .map(|c| {
            let mut m = ChallengeSummaryModel::from_challenge(c, configuration_revision);
            // Mirror the scoreboard cell exactly (RSCTF `GenScoreboard`): A&D /
            // KotH are live-scored (0), every other challenge shows the current
            // dynamic-decayed score at its distinct-solve count.
            m.score = scoring::summary_score(
                c.challenge_type,
                c.original_score,
                c.min_score_rate,
                c.difficulty,
                c.score_curve,
                solved_count.get(&c.id).copied().unwrap_or(0),
            );
            m
        })
        .collect();
    Ok(RequestResponse::ok(data))
}

/// `POST /api/edit/games/{id}/challenges`
pub async fn add_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<ChallengeInfoModel>,
) -> AppResult<RequestResponse<ChallengeEditDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    load_game(&st, id).await?;
    mutation_sql::validate_create(&model)?;
    let fingerprint = if model.content.is_none()
        && model.initial_flag.is_none()
        && model.original_score.is_none()
    {
        crate::services::mutation_operations::fingerprint(
            "challenge-create",
            &(&model.title, model.category, model.challenge_type),
        )?
    } else {
        crate::services::mutation_operations::fingerprint(
            "challenge-create-simple",
            &(
                &model.title,
                model.category,
                model.challenge_type,
                &model.content,
                &model.initial_flag,
                model.original_score,
            ),
        )?
    };

    // Every challenge kind shares the game deletion/control domain. A game
    // whose hard-delete fence committed must not gain a new child while its
    // external teardown is running.
    let mut engine_control =
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?);
    let control = engine_control
        .as_mut()
        .expect("new challenge holds the game control lock");
    let game_accepts_children = sqlx::query_scalar::<_, bool>(INSERTABLE_GAME_SQL)
        .bind(id)
        .fetch_optional(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    if !game_accepts_children {
        return Err(AppError::conflict("Game is being deleted"));
    }
    if model.challenge_type == ChallengeType::KingOfTheHill {
        super::games::validate_koth_game_shape_locked(control.transaction_mut(), id).await?;
    }

    let replay = crate::services::mutation_operations::claim(
        control.transaction_mut(),
        user.id,
        "challenge-create",
        &format!("game:{id}"),
        model.operation_id,
        fingerprint,
    )
    .await?;
    let challenge_id = if let Some(replay) = replay {
        replay
            .result_id
            .parse::<i32>()
            .map_err(|_| AppError::internal("invalid retained challenge result identity"))?
    } else {
        let created =
            mutation_sql::insert_challenge_locked(control.transaction_mut(), id, &model).await?;
        seed_division_configs(control.transaction_mut(), id, created.id).await?;
        crate::services::mutation_operations::complete(
            control.transaction_mut(),
            user.id,
            "challenge-create",
            &format!("game:{id}"),
            model.operation_id,
            &created.id.to_string(),
            Some(created.revision),
        )
        .await?;
        created.id
    };
    if let Some(control) = engine_control {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let created = load_challenge(&st, id, challenge_id).await?;
    flush_game_scoreboards(&st, id).await;
    Ok(RequestResponse::ok(
        ChallengeEditDetailModel::from_challenge(
            &st,
            &created,
            load_flags(&st, challenge_id).await?,
        )
        .await?,
    ))
}

/// `GET /api/edit/games/{id}/challenges/{cId}`
pub async fn get_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ChallengeEditDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    let flags = if challenge.challenge_type == ChallengeType::DynamicContainer {
        Vec::new()
    } else {
        load_flags(&st, c_id).await?
    };
    let mut detail = ChallengeEditDetailModel::from_challenge(&st, &challenge, flags).await?;
    if !challenge.is_enabled && challenge.challenge_type.is_container() {
        detail.runtime_transition_pending = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "ChallengeDefinitionTransitions" WHERE challenge_id = $1)"#,
        ).bind(c_id).fetch_one(st.pg()).await.map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(RequestResponse::ok(detail))
}

/// `PUT /api/edit/games/{id}/challenges/{cId}`
pub async fn update_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    Json(model): Json<ChallengeUpdateModel>,
) -> AppResult<RequestResponse<ChallengeEditDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    let game = load_game(&st, id).await?;
    if !(1..=9_007_199_254_740_990).contains(&model.expected_revision) {
        return Err(AppError::bad_request(
            "expectedRevision must be a positive safe integer",
        ));
    }
    let operation_scope = format!("game:{id}:challenge:{c_id}");
    let mut fingerprint_model = model.clone();
    fingerprint_model.operation_id = Uuid::nil();
    let fingerprint =
        crate::services::mutation_operations::fingerprint("challenge-update", &fingerprint_model)?;
    if let Some(replay) = crate::services::mutation_operations::find_completed(
        st.pg(),
        user.id,
        "challenge-update",
        &operation_scope,
        model.operation_id,
        fingerprint,
    )
    .await?
    {
        if replay.result_id != c_id.to_string() {
            return Err(AppError::conflict(
                "operationId belongs to a different challenge",
            ));
        }
        let challenge = load_challenge(&st, id, c_id).await?;
        let flags = if challenge.challenge_type == ChallengeType::DynamicContainer {
            Vec::new()
        } else {
            load_flags(&st, c_id).await?
        };
        return Ok(RequestResponse::ok(
            ChallengeEditDetailModel::from_challenge(&st, &challenge, flags).await?,
        ));
    }
    // Every runtime eligibility/topology mutation and its possible cleanup
    // shares this outer challenge fence. Cleanup may take per-runtime
    // provisioning locks, so this gate deliberately sits outside that bounded
    // semaphore. The global order is transition -> game -> definition -> runtime.
    let runtime_transition = if workload::update_changes_runtime_definition(&model) {
        Some(
            crate::services::challenge_workloads::acquire_runtime_transition_lock(st.pg(), c_id)
                .await?,
        )
    } else {
        None
    };
    let mut engine_control =
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?);
    let control = engine_control
        .as_mut()
        .expect("challenge update holds the game control lock");
    if workload::update_changes_runtime_definition(&model) {
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            control.transaction_mut(),
            &crate::services::challenge_workloads::definition_lock_key(id, c_id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let challenge = load_challenge_locked(control.transaction_mut(), id, c_id).await?;
    deletion::reject_pending_mutation(&mut **control.transaction_mut(), id, c_id).await?;
    // Reject an already-stale editor before any topology drain or external
    // runtime teardown. The final SQL CAS remains authoritative for writers
    // that race after this early safety check.
    if challenge.revision != model.expected_revision {
        return Err(AppError::conflict(
            "Challenge revision changed; reload and retry the edit",
        ));
    }
    let ch_type = challenge.challenge_type;
    crate::utils::scoring::validate_challenge_scoring(
        model.original_score.unwrap_or(challenge.original_score),
        model.min_score_rate.unwrap_or(challenge.min_score_rate),
        model.difficulty.unwrap_or(challenge.difficulty),
        model.submission_limit.unwrap_or(challenge.submission_limit),
    )?;
    if let Some(storage_limit) = model.storage_limit {
        crate::services::container::validate_storage_limit_value(storage_limit)?;
    }
    let requested_network_mode = model
        .network_mode
        .or(challenge.network_mode)
        .unwrap_or(NetworkMode::Open);
    crate::services::container::validate_network_mode_value(ch_type, requested_network_mode)?;
    // A normal submit locks Games before its challenge-scoped grading lock.
    // Preserve that order here so the global boundary and this challenge's
    // policy are both linearizable without a lock inversion.
    let competition_scoring_started =
        competition_scoring_started_locked(control.transaction_mut(), id).await?;
    crate::utils::scoring::lock_jeopardy_flags_exclusive(control.transaction_mut(), c_id).await?;
    if competition_scoring_started && update_policy::scoring_fields_changed(&model, &challenge) {
        return Err(AppError::bad_request(
            "Challenge scoring settings are locked after competition scoring has started.",
        ));
    }
    let scoring_started = if ch_type.uses_ad_engine() {
        ad_epoch_scoring_started_locked(control.transaction_mut(), id).await?
    } else {
        false
    };
    let maintenance_edit = scoring_started
        && crate::services::ad::maintenance::editing_allowed(control.transaction_mut(), id).await?;
    if scoring_started
        && !maintenance_edit
        && update_policy::operational_fields_changed(&model, &challenge)
    {
        return Err(AppError::conflict(
            "Request a maintenance window before changing live A&D/KotH operational settings",
        ));
    }
    if maintenance_edit && update_policy::operational_fields_changed(&model, &challenge) {
        sqlx::query(
            r#"UPDATE "CompetitionMaintenanceWindows"
            SET touched_challenge_ids = array_append(touched_challenge_ids, $2)
            WHERE game_id = $1 AND state = 'Editing'
              AND NOT ($2 = ANY(touched_challenge_ids))"#,
        )
        .bind(id)
        .bind(c_id)
        .execute(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let old_shared = challenge.enable_shared_container;
    let was_ad_self_hosted = challenge.ad_self_hosted;
    // Capture the pre-update enabled flag so we can detect a false->true
    // transition and announce the newly-live challenge (mirror below).
    let was_enabled = challenge.is_enabled;
    // Capture the pre-update shared-container mode (full `UsesSharedContainer`
    // predicate) so a shared↔per-team flip can be detected after the write and the
    // now-orphaned containers torn down (RSCTF `wasSharedManaged`).
    let was_shared_managed = update_policy::uses_shared_container(&challenge);
    let requested_final_enabled = model.is_enabled.unwrap_or(challenge.is_enabled);
    let requested_ad_self_hosted = model.ad_self_hosted.unwrap_or(was_ad_self_hosted);
    let workload_update = workload::validate_update(&challenge, &model.workload_spec)?;
    let projected_workload_present = workload_update
        .as_ref()
        .map_or(challenge.workload_spec.is_some(), Option::is_some);
    let next_profile = model
        .runtime_profile
        .as_ref()
        .unwrap_or(&challenge.runtime_profile);
    let profile_changed = next_profile != &challenge.runtime_profile;
    if profile_changed || model.replace_runtime_instances {
        if !ch_type.is_container() || requested_ad_self_hosted {
            return Err(AppError::bad_request(
                "Runtime profiles apply only to platform-hosted container challenges",
            ));
        }
        if ch_type.uses_ad_engine() && scoring_started && !maintenance_edit {
            return Err(AppError::bad_request("A&D/KotH runtime profiles are locked after official scoring starts; select the runtime before starting the competition"));
        }
    }
    if requested_final_enabled || profile_changed || model.replace_runtime_instances {
        crate::services::container::profiles::policy::validate_selection(
            &st,
            next_profile.as_deref(),
            ch_type,
            requested_ad_self_hosted,
            projected_workload_present,
            model
                .enable_traffic_capture
                .unwrap_or(challenge.enable_traffic_capture),
        )?;
    }
    if profile_changed || model.replace_runtime_instances {
        if model.replace_runtime_instances
            && model
                .container_image
                .as_ref()
                .is_some_and(|image| Some(image.as_str()) != challenge.container_image.as_deref())
        {
            return Err(AppError::bad_request(
                "Save and build the new image successfully before replacing instances",
            ));
        }
        let mut projected = challenge.clone();
        projected.runtime_profile = next_profile.clone();
        if let Some(workload) = &workload_update {
            projected.workload_spec = workload.clone();
        }
        projected.enable_traffic_capture = model
            .enable_traffic_capture
            .unwrap_or(challenge.enable_traffic_capture);
        // Validate portability before any destructive transition. Registry digests
        // work across backends; daemon-local image IDs cannot move to Kubernetes.
        crate::services::challenge_workloads::resolve_runtime(&st, &projected)?;
    }
    let projected_image_present = model.container_image.as_deref().map_or_else(
        || {
            challenge
                .container_image
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        },
        |value| !value.trim().is_empty(),
    );
    let requested_shared_managed = ch_type == ChallengeType::StaticContainer
        && model.enable_shared_container.unwrap_or(old_shared)
        && (projected_workload_present || projected_image_present);
    let requested_topology_flip = challenge.is_enabled
        && requested_final_enabled
        && (model.replace_runtime_instances
            || was_shared_managed != requested_shared_managed
            || was_ad_self_hosted != requested_ad_self_hosted);
    let topology_transition = topology_transition::resolve(
        control.transaction_mut(),
        c_id,
        user.id,
        model.operation_id,
        fingerprint,
        model.expected_revision,
        requested_topology_flip,
        requested_final_enabled,
    )
    .await?;
    let active_topology_flip = topology_transition.is_some();
    let final_enabled = topology_transition
        .as_ref()
        .map_or(requested_final_enabled, |transition| {
            transition.final_enabled
        });
    let resuming_topology_transition = topology_transition
        .as_ref()
        .is_some_and(|transition| transition.resuming);
    // A failed teardown deliberately leaves the challenge disabled. Only the
    // retained, byte-identical transition may resume that replacement.
    if model.replace_runtime_instances && !challenge.is_enabled && !resuming_topology_transition {
        return Err(AppError::bad_request(
            "Enable the challenge before requesting instance replacement",
        ));
    }
    if model.replace_runtime_instances && !final_enabled {
        return Err(AppError::bad_request(
            "Instance replacement requires the challenge to remain enabled",
        ));
    }
    let notice_was_enabled = if resuming_topology_transition {
        final_enabled
    } else {
        was_enabled
    };
    let transition_definition = if active_topology_flip {
        Some(
            lifecycle::runtime_definition_snapshot_locked(
                control.transaction_mut(),
                c_id,
                challenge.challenge_type,
            )
            .await?,
        )
    } else {
        None
    };

    // Guard: enabling a non-dynamic challenge with no flags is rejected.
    if model.is_enabled == Some(true) && !challenge.is_enabled && !ch_type.is_dynamic() {
        super::flags::ensure_static_flag_can_enable_locked(control.transaction_mut(), c_id).await?;
    }
    if model.enable_traffic_capture == Some(true) && !ch_type.is_container() {
        return Err(AppError::bad_request(
            "Traffic capture is only allowed for container challenges",
        ));
    }
    let checker_changed = model.ad_checker_image.as_ref().is_some_and(|value| {
        value.trim() != challenge.ad_checker_image.as_deref().unwrap_or("").trim()
    });
    let enabled_changed = model
        .is_enabled
        .is_some_and(|enabled| enabled != challenge.is_enabled);
    let hosting_changed = model
        .ad_self_hosted
        .is_some_and(|value| value != challenge.ad_self_hosted);
    let image_changed = model.container_image.as_ref().is_some_and(|value| {
        value.trim() != challenge.container_image.as_deref().unwrap_or("").trim()
    });
    let invalidated_build_status = image_changed.then(|| {
        super::builds::invalidated_build_status(
            model.container_image.as_deref(),
            challenge.original_archive_blob_path.as_deref(),
            challenge.build_context_subdir.as_deref(),
        )
    });
    if model.ad_self_hosted == Some(true) && ch_type != ChallengeType::AttackDefense {
        return Err(AppError::bad_request(
            "Self-hosted/BYOC mode is available only for Attack-Defense challenges.",
        ));
    }
    if ch_type.uses_ad_engine()
        && scoring_started
        && !maintenance_edit
        && (checker_changed || enabled_changed || hosting_changed)
    {
        return Err(AppError::bad_request(
            "A&D/KotH checker, enabled state, and hosting topology are locked after epoch scoring has started.",
        ));
    }
    if ch_type == ChallengeType::KingOfTheHill
        && image_changed
        && scoring_started
        && !maintenance_edit
    {
        return Err(AppError::bad_request(
            "The KotH challenge image is locked after official scoring has started.",
        ));
    }
    if let Some(weight) = model.ad_scoring_weight {
        if !weight.is_finite() || !(0.8..=1.2).contains(&weight) {
            return Err(AppError::bad_request(
                "Engine challenge scoring weight must be between 0.8 and 1.2.",
            ));
        }
        if (weight - challenge.ad_scoring_weight).abs() > f64::EPSILON && scoring_started {
            return Err(AppError::bad_request(
                "A&D/KotH challenge weights are locked after epoch scoring has started.",
            ));
        }
    }
    if let Some(name) = &model.file_name {
        if name.trim().is_empty() {
            return Err(AppError::bad_request(
                "Dynamic attachment file name cannot be empty",
            ));
        }
    }
    // A dynamic template must remain submittable after every production
    // placeholder occurrence expands. Blank input clears the template and uses
    // the bounded default generator.
    if let Some(t) = model.flag_template.as_deref() {
        if !crate::utils::flag_policy::is_blank(t) && ch_type == ChallengeType::DynamicContainer {
            crate::utils::flag_policy::validate_dynamic_template(t)
                .map_err(|error| AppError::bad_request(error.to_string()))?;
        }
    }

    if active_topology_flip {
        // A live topology change is a durable two-phase transition. First make
        // every runtime publisher ineligible while the existing
        // transition/game/definition hierarchy is held. Release the short DB
        // locks before external teardown; publishers that began earlier either
        // finish first or fail their final definition/eligibility CAS. A crash
        // or teardown failure leaves the challenge disabled, never half-old and
        // half-new while still playable.
        topology_transition::begin(
            engine_control
                .as_mut()
                .expect("challenge update holds the game control lock")
                .transaction_mut(),
            c_id,
            id,
            user.id,
            model.operation_id,
            fingerprint,
            model.expected_revision,
            topology_transition
                .as_ref()
                .expect("active topology flip has durable transition state"),
        )
        .await?;
        if let Some(lock) = engine_control.take() {
            crate::services::ad::koth_capability_cache::release_game_control(
                lock,
                st.cache.as_ref(),
                id,
                ch_type == ChallengeType::KingOfTheHill,
            )
            .await?;
        }
        if was_ad_self_hosted {
            st.byoc.disconnect_challenge(&st.db, c_id).await?;
        }
        destroy_challenge_containers(&st, &challenge, true, true).await?;

        // Re-enter the canonical transition -> game -> definition order and
        // publish the new topology together with restored eligibility below.
        let mut reacquired_engine =
            crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
        if ch_type.uses_ad_engine()
            && ad_epoch_scoring_started_locked(reacquired_engine.transaction_mut(), id).await?
            && !crate::services::ad::maintenance::editing_allowed(
                reacquired_engine.transaction_mut(),
                id,
            )
            .await?
        {
            return Err(AppError::conflict(
                "A&D/KotH scoring started while the topology transition was draining; the challenge remains disabled",
            ));
        }
        if model.replace_runtime_instances {
            crate::utils::single_flight::acquire_transaction_advisory_lock(
                reacquired_engine.transaction_mut(),
                &format!("test-containers-game:{id}"),
            )
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            lifecycle::destroy_test_container_locked(&st, c_id).await?;
        }
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            reacquired_engine.transaction_mut(),
            &crate::services::challenge_workloads::definition_lock_key(id, c_id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        engine_control = Some(reacquired_engine);
    }

    let update_base = if active_topology_flip {
        let control = engine_control
            .as_mut()
            .expect("reacquired challenge update holds game control");
        let current = load_challenge_locked(control.transaction_mut(), id, c_id).await?;
        if current.is_enabled {
            return Err(AppError::conflict(
                "Challenge topology fence changed during cleanup; retry the update",
            ));
        }
        let current_definition = lifecycle::runtime_definition_snapshot_locked(
            control.transaction_mut(),
            c_id,
            current.challenge_type,
        )
        .await?;
        if transition_definition.as_ref() != Some(&current_definition) {
            return Err(AppError::conflict(
                "Challenge runtime definition changed during cleanup; review the repository update and retry. The challenge remains disabled",
            ));
        }
        current
    } else {
        challenge
    };
    let current_variant_policy = (
        update_base.variant_mode,
        update_base.variant_generator_image.clone(),
        update_base.variant_generator_digest.clone(),
        update_base.solve_receipt_mode,
        update_base.receipt_verifier_identity.clone(),
    );
    let next_variant_mode = model.variant_mode.unwrap_or(current_variant_policy.0);
    let current_generator_is_managed = update_base
        .variant_generator_build_context_subdir
        .as_deref()
        == Some(crate::services::git_sync::GENERATOR_CONTEXT_SUBDIR);
    let generator_identity_supplied =
        model.variant_generator_image.is_some() || model.variant_generator_digest.is_some();
    let retain_managed_generator = current_generator_is_managed
        && !generator_identity_supplied
        && next_variant_mode == ChallengeVariantMode::PerParticipation;
    let leaving_managed_generator = current_generator_is_managed && !retain_managed_generator;
    let next_generator_image = model
        .variant_generator_image
        .as_ref()
        .map(|value| value.trim())
        .and_then(|value| (!value.is_empty()).then_some(value))
        .or_else(|| {
            (!leaving_managed_generator)
                .then_some(current_variant_policy.1.as_deref())
                .flatten()
        });
    let next_generator_digest = model
        .variant_generator_digest
        .as_ref()
        .map(|value| value.trim())
        .and_then(|value| (!value.is_empty()).then_some(value))
        .or_else(|| {
            (!leaving_managed_generator)
                .then_some(current_variant_policy.2.as_deref())
                .flatten()
        });
    let next_receipt_mode = model.solve_receipt_mode.unwrap_or(current_variant_policy.3);
    let next_verifier_identity = model
        .receipt_verifier_identity
        .as_ref()
        .map(|value| value.trim())
        .map(|value| (!value.is_empty()).then_some(value))
        .unwrap_or(current_variant_policy.4.as_deref());
    if retain_managed_generator {
        crate::services::event_security::validate_challenge_provenance_modes(
            ch_type,
            next_variant_mode,
            next_receipt_mode,
            next_verifier_identity,
            st.config.as_ref(),
        )?;
    } else {
        crate::services::event_security::validate_challenge_provenance_policy(
            ch_type,
            next_variant_mode,
            next_generator_image,
            next_generator_digest,
            next_receipt_mode,
            next_verifier_identity,
            st.config.as_ref(),
        )?;
    }
    let variant_policy_changed = model
        .variant_mode
        .is_some_and(|value| value != current_variant_policy.0)
        || model
            .variant_generator_image
            .as_ref()
            .is_some_and(|value| Some(value.trim()) != current_variant_policy.1.as_deref())
        || model
            .variant_generator_digest
            .as_ref()
            .is_some_and(|value| Some(value.trim()) != current_variant_policy.2.as_deref())
        || model
            .solve_receipt_mode
            .is_some_and(|value| value != current_variant_policy.3)
        || model
            .receipt_verifier_identity
            .as_ref()
            .is_some_and(|value| Some(value.trim()) != current_variant_policy.4.as_deref());
    if variant_policy_changed {
        let policy_frozen: bool = sqlx::query_scalar(
            r#"SELECT clock_timestamp() >= start_time_utc
                 FROM "Games" WHERE id = $1 FOR SHARE"#,
        )
        .bind(id)
        .fetch_one(
            engine_control
                .as_mut()
                .expect("challenge update holds the game control lock")
                .transaction_mut()
                .as_mut(),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if policy_frozen {
            return Err(AppError::bad_request(
                "Challenge variant and solve-receipt settings are frozen at event start.",
            ));
        }
    }
    let replay = crate::services::mutation_operations::claim(
        engine_control
            .as_mut()
            .expect("challenge update holds the game control lock")
            .transaction_mut(),
        user.id,
        "challenge-update",
        &operation_scope,
        model.operation_id,
        fingerprint,
    )
    .await?;
    if let Some(replay) = replay {
        if replay.result_id != c_id.to_string() {
            return Err(AppError::conflict(
                "operationId belongs to a different challenge",
            ));
        }
        if let Some(lock) = engine_control.take() {
            lock.release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
        if let Some(lock) = runtime_transition {
            lock.release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
        }
        let challenge = load_challenge(&st, id, c_id).await?;
        let flags = if challenge.challenge_type == ChallengeType::DynamicContainer {
            Vec::new()
        } else {
            load_flags(&st, c_id).await?
        };
        return Ok(RequestResponse::ok(
            ChallengeEditDetailModel::from_challenge(&st, &challenge, flags).await?,
        ));
    }
    let updated = definition_write::update(
        engine_control
            .as_mut()
            .expect("challenge update holds the game control lock")
            .transaction_mut(),
        id,
        c_id,
        model.expected_revision,
        &update_base,
        &model,
        definition_write::DefinitionWriteOptions {
            active_topology_flip,
            final_enabled,
            workload_update,
            invalidated_build_status,
            registry_image_hotfix: maintenance_edit && image_changed,
            leaving_managed_generator,
        },
    )
    .await?;
    seed_division_configs(
        engine_control
            .as_mut()
            .expect("challenge update holds the game control lock")
            .transaction_mut(),
        id,
        c_id,
    )
    .await?;
    if ch_type == ChallengeType::KingOfTheHill && !updated.is_enabled {
        crate::services::ad_engine::clear_challenge_control_locked(
            &mut **engine_control
                .as_mut()
                .expect("challenge update holds the game control lock")
                .transaction_mut(),
            id,
            c_id,
        )
        .await?;
    }
    crate::services::mutation_operations::complete(
        engine_control
            .as_mut()
            .expect("challenge update holds the game control lock")
            .transaction_mut(),
        user.id,
        "challenge-update",
        &operation_scope,
        model.operation_id,
        &c_id.to_string(),
        Some(updated.revision),
    )
    .await?;
    let effects = serde_json::json!({
        "title": updated.title.clone(),
        "scoreboard": true,
        "vpn": true,
        "repoPush": game.repo_binding_id.is_some(),
        "runtime": active_topology_flip
            || was_shared_managed != update_policy::uses_shared_container(&updated)
            || was_ad_self_hosted != updated.ad_self_hosted
            || model.is_enabled == Some(false),
        "newChallengeNotice": updated.is_enabled && !notice_was_enabled && game.is_active(Utc::now()),
        "newHintNotice": false,
    });
    sqlx::query(
        r#"INSERT INTO "ChallengeRevisionEffects"
             (game_id, challenge_id, revision, effects)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (challenge_id, revision) DO NOTHING"#,
    )
    .bind(id)
    .bind(c_id)
    .bind(updated.revision)
    .bind(effects)
    .execute(
        &mut **engine_control
            .as_mut()
            .expect("challenge update holds the game control lock")
            .transaction_mut(),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if active_topology_flip {
        topology_transition::complete(
            engine_control
                .as_mut()
                .expect("challenge update holds the game control lock")
                .transaction_mut(),
            c_id,
            user.id,
            model.operation_id,
        )
        .await?;
    }
    if let Some(lock) = engine_control {
        crate::services::ad::koth_capability_cache::release_game_control(
            lock,
            st.cache.as_ref(),
            id,
            ch_type == ChallengeType::KingOfTheHill,
        )
        .await?;
    }
    if let Some(lock) = runtime_transition {
        if let Err(error) = lock.release().await {
            tracing::warn!(%error, challenge = c_id, "post-commit runtime fence release failed");
        }
    }

    let flags = if updated.challenge_type == ChallengeType::DynamicContainer {
        Vec::new()
    } else {
        load_flags(&st, c_id).await?
    };
    Ok(RequestResponse::ok(
        ChallengeEditDetailModel::from_challenge(&st, &updated, flags).await?,
    ))
}

/// `DELETE /api/edit/games/{id}/challenges/{cId}` — void.
pub async fn delete_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    delete_challenge_core(st, id, c_id, true, true).await?;
    Ok(MessageResponse::ok(""))
}

use crate::services::challenge_lifecycle::hard_delete::delete_challenge_core;
