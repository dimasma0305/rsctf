//! edit: challenge CRUD/attachments (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

mod attachments;
mod audit;
mod definition_write;
mod deletion;
#[cfg(test)]
mod deletion_tests;
mod hints;
mod lifecycle;
mod repo_push;
mod review;
mod revision_effects;
#[cfg(test)]
mod revision_tests;
mod scoring;
#[cfg(test)]
mod scoring_lock_tests;
mod topology_transition;
mod workload;

pub use attachments::update_attachment;
pub(crate) use attachments::{build_attachment, validate_remote_attachment_url};
pub use audit::{
    download_challenge_audit_archive, get_challenge_audit_meta, get_challenge_build_status,
    list_challenge_build_statuses, rebuild_challenge,
};
pub(crate) use deletion::reject_pending_mutation;
pub(crate) use lifecycle::destroy_challenge_containers;
use lifecycle::destroy_test_container_locked;
#[cfg(test)]
pub(crate) use repo_push::commit_latest_to_checkout_for_test;
pub use repo_push::start as start_repo_push_worker;
pub use review::{approve_challenge, list_pending_challenges, reject_challenge};
pub use revision_effects::start as start_challenge_revision_effect_worker;
pub(crate) use workload::execute_workload_rollout_job;
pub use workload::rollout_workloads;

const INSERTABLE_GAME_SQL: &str =
    r#"SELECT NOT deletion_pending FROM "Games" WHERE id = $1 FOR SHARE"#;
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
            let mut m = ChallengeSummaryModel::from_challenge(c);
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
    let fingerprint = crate::services::mutation_operations::fingerprint(
        "challenge-create",
        &(&model.title, model.category, model.challenge_type),
    )?;

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
        let challenge_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "GameChallenges"
                 (game_id, title, content, category, "Type", is_enabled, revision,
                  submission_limit, accepted_count, submission_count, review_status,
                  build_status, original_score, min_score_rate, difficulty, score_curve,
                  network_mode, enable_traffic_capture, enable_shared_container,
                  disable_blood_bonus, ad_allow_egress, ad_allow_self_reset,
                  ad_ssh_requires_flag, ad_self_hosted)
               VALUES ($1, $2, '', $3, $4, FALSE, 1, $5, 0, 0, $6, $7,
                       $8, $9, $10, $11, $12, FALSE, FALSE, TRUE, FALSE, FALSE,
                       FALSE, FALSE)
            RETURNING id"#,
        )
        .bind(id)
        .bind(&model.title)
        .bind(model.category as i16)
        .bind(model.challenge_type as i16)
        .bind(crate::utils::scoring::DEFAULT_CHALLENGE_SUBMISSION_LIMIT)
        .bind(ChallengeReviewStatus::Active as i16)
        .bind(ChallengeBuildStatus::None as i16)
        .bind(crate::utils::scoring::DEFAULT_JEOPARDY_ORIGINAL_SCORE)
        .bind(crate::utils::scoring::DEFAULT_JEOPARDY_MIN_SCORE_RATE)
        .bind(crate::utils::scoring::DEFAULT_JEOPARDY_DIFFICULTY)
        .bind(ScoreCurve::Standard as i16)
        .bind(NetworkMode::Open as i16)
        .fetch_one(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        seed_division_configs(control.transaction_mut(), id, challenge_id).await?;
        crate::services::mutation_operations::complete(
            control.transaction_mut(),
            user.id,
            "challenge-create",
            &format!("game:{id}"),
            model.operation_id,
            &challenge_id.to_string(),
            Some(1),
        )
        .await?;
        challenge_id
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
        ChallengeEditDetailModel::from_challenge(&st, &created, Vec::new()).await?,
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
    Ok(RequestResponse::ok(
        ChallengeEditDetailModel::from_challenge(&st, &challenge, flags).await?,
    ))
}

/// Whether a challenge is in shared-container mode (RSCTF
/// `GameChallenge.UsesSharedContainer`): a `StaticContainer` with the shared
/// toggle on and a usable image + exposed port. Used to detect a shared↔per-team
/// mode flip that would strand the containers created under the old mode.
fn uses_shared_container(c: &game_challenge::Model) -> bool {
    c.challenge_type == ChallengeType::StaticContainer
        && c.enable_shared_container
        && crate::services::challenge_workloads::has_runtime(c)
}

fn challenge_scoring_fields_changed(
    model: &ChallengeUpdateModel,
    challenge: &game_challenge::Model,
) -> bool {
    let deadline_changed = model.deadline_utc.is_some_and(|deadline| {
        let requested = (deadline.timestamp() != 0).then_some(deadline);
        requested != challenge.deadline_utc
    });
    let flag_template_changed = model.flag_template.as_ref().is_some_and(|template| {
        let requested = (!template.trim().is_empty()).then_some(template.as_str());
        requested != challenge.flag_template.as_deref()
    });
    deadline_changed
        || flag_template_changed
        || model
            .submission_limit
            .is_some_and(|value| value != challenge.submission_limit)
        || model
            .original_score
            .is_some_and(|value| value != challenge.original_score)
        || model
            .min_score_rate
            .is_some_and(|value| value != challenge.min_score_rate)
        || model
            .difficulty
            .is_some_and(|value| value != challenge.difficulty)
        || model
            .score_curve
            .is_some_and(|value| value != challenge.score_curve)
        || model
            .disable_blood_bonus
            .is_some_and(|value| value != challenge.disable_blood_bonus)
        || model
            .ad_scoring_weight
            .is_some_and(|value| (value - challenge.ad_scoring_weight).abs() > f64::EPSILON)
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
    let mut workload_lock =
        workload::acquire_update_lock_for_model(st.pg(), id, c_id, &model).await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    deletion::reject_pending_mutation(st.pg(), id, c_id).await?;
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
    let control = engine_control
        .as_mut()
        .expect("challenge update holds the game control lock");
    let competition_scoring_started =
        competition_scoring_started_locked(control.transaction_mut(), id).await?;
    crate::utils::scoring::lock_jeopardy_flags_exclusive(control.transaction_mut(), c_id).await?;
    if competition_scoring_started && challenge_scoring_fields_changed(&model, &challenge) {
        return Err(AppError::bad_request(
            "Challenge scoring settings are locked after competition scoring has started.",
        ));
    }
    let scoring_started = if ch_type.uses_ad_engine() {
        ad_epoch_scoring_started_locked(control.transaction_mut(), id).await?
    } else {
        false
    };
    let old_shared = challenge.enable_shared_container;
    let was_ad_self_hosted = challenge.ad_self_hosted;
    // Capture the pre-update enabled flag so we can detect a false->true
    // transition and announce the newly-live challenge (mirror below).
    let was_enabled = challenge.is_enabled;
    // Capture the pre-update shared-container mode (full `UsesSharedContainer`
    // predicate) so a shared↔per-team flip can be detected after the write and the
    // now-orphaned containers torn down (RSCTF `wasSharedManaged`).
    let was_shared_managed = uses_shared_container(&challenge);
    let requested_final_enabled = model.is_enabled.unwrap_or(challenge.is_enabled);
    let requested_ad_self_hosted = model.ad_self_hosted.unwrap_or(was_ad_self_hosted);
    // Whether the client's hints array differs from the stored one (RSCTF
    // `hintUpdated`) — captured before `model.hints` is consumed below; drives the
    // NewHint notice further down.
    let hints_changed = model
        .hints
        .as_ref()
        .is_some_and(|h| hints::updated(challenge.hints.as_ref(), h));
    let workload_update = workload::validate_update(&challenge, &model.workload_spec)?;
    let projected_workload_present = workload_update
        .as_ref()
        .map_or(challenge.workload_spec.is_some(), Option::is_some);
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
        && (was_shared_managed != requested_shared_managed
            || was_ad_self_hosted != requested_ad_self_hosted);
    let topology_transition = topology_transition::resolve(
        engine_control
            .as_mut()
            .expect("challenge update holds the game control lock")
            .transaction_mut(),
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
    let notice_was_enabled = if resuming_topology_transition {
        final_enabled
    } else {
        was_enabled
    };
    let transition_definition = if active_topology_flip {
        Some(lifecycle::runtime_definition_snapshot(st.pg(), c_id, challenge.challenge_type).await?)
    } else {
        None
    };

    // Guard: enabling a non-dynamic challenge with no flags is rejected.
    if model.is_enabled == Some(true) && !challenge.is_enabled && !ch_type.is_dynamic() {
        let flags = load_flags(&st, c_id).await?;
        if flags.is_empty() {
            return Err(AppError::bad_request(
                "Cannot enable a challenge that has no flag",
            ));
        }
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
        && (checker_changed || enabled_changed || hosting_changed)
    {
        return Err(AppError::bad_request(
            "A&D/KotH checker, enabled state, and hosting topology are locked after epoch scoring has started.",
        ));
    }
    if ch_type == ChallengeType::KingOfTheHill && image_changed && scoring_started {
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
    // RSCTF `UpdateGameChallenge`: a non-blank flag template on a DynamicContainer
    // must carry enough randomness or every team receives the SAME flag. RSCTF's
    // `DynamicFlagGenerator.IsValid` treats a template as sufficiently random only
    // when it contains a `[GUID]` or `[TEAM_HASH]` placeholder — reject otherwise
    // with 400 `Challenge_FlagTooTrivial`. (rsctf's `flag_generator::generate_flag`
    // also expands `[UUID]`, but RSCTF's validator recognizes only the two tokens
    // above, so we match RSCTF here.)
    if let Some(t) = model.flag_template.as_deref() {
        if !t.trim().is_empty()
            && ch_type == ChallengeType::DynamicContainer
            && !(t.contains("[GUID]") || t.contains("[TEAM_HASH]"))
        {
            return Err(AppError::bad_request(
                "Flag template is too trivial: it must contain a [GUID] or [TEAM_HASH] placeholder",
            ));
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
        workload::release_update_lock(workload_lock.take()).await?;
        if let Some(lock) = engine_control.take() {
            lock.release()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
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
        {
            return Err(AppError::conflict(
                "A&D/KotH scoring started while the topology transition was draining; the challenge remains disabled",
            ));
        }
        engine_control = Some(reacquired_engine);
        workload_lock = workload::acquire_update_lock_for_model(st.pg(), id, c_id, &model).await?;
    }

    let update_base = if active_topology_flip {
        let current = load_challenge(&st, id, c_id).await?;
        if current.is_enabled {
            return Err(AppError::conflict(
                "Challenge topology fence changed during cleanup; retry the update",
            ));
        }
        let current_definition =
            lifecycle::runtime_definition_snapshot(st.pg(), c_id, current.challenge_type).await?;
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
    let control = engine_control
        .as_mut()
        .expect("challenge update holds the game control lock");
    let replay = crate::services::mutation_operations::claim(
        control.transaction_mut(),
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
        workload::release_update_lock(workload_lock.take()).await?;
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
        control.transaction_mut(),
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
            || was_shared_managed != uses_shared_container(&updated)
            || was_ad_self_hosted != updated.ad_self_hosted
            || model.is_enabled == Some(false),
        "newChallengeNotice": updated.is_enabled && !notice_was_enabled && game.is_active(Utc::now()),
        "newHintNotice": game.is_active(Utc::now()) && updated.is_enabled && hints_changed,
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
    workload::release_update_lock(workload_lock.take()).await?;
    if let Some(lock) = engine_control {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
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
    // Share the hard-deletion admission domain with whole-game deletion before
    // retaining the outer runtime-transition transaction.
    let deletion_admission = super::deletion_locks::acquire_hard_deletion_admission().await?;
    // Take the same transition -> game -> definition order as false -> true
    // edits. The transition fence remains held through physical teardown so no
    // replica can re-enable the challenge behind a stale cleanup snapshot.
    let runtime_transition =
        crate::services::challenge_workloads::acquire_runtime_transition_lock(st.pg(), c_id)
            .await?;
    let mut engine_control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    let mut definition_lock = deletion::acquire_definition_lock(st.pg(), id, c_id).await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    if challenge.challenge_type.uses_ad_engine()
        && ad_epoch_scoring_started_locked(engine_control.transaction_mut(), id).await?
    {
        return Err(AppError::bad_request(
            "A&D/KotH challenges cannot be deleted after epoch scoring has started.",
        ));
    }

    // The JFLG-exclusive predicate and the durable disabled marker share the
    // definition-lock transaction. This preserves Jeopardy history once play
    // could have started and closes an in-flight-submit TOCTOU. Committing the
    // short definition mutation before runtime I/O also keeps the pool bounded.
    deletion::fence_challenge_deletion(definition_lock.transaction_mut(), id, c_id).await?;
    definition_lock.release().await?;

    // Revoke A&D/KotH routes before any backing address can be freed.
    if challenge.challenge_type.uses_ad_engine() {
        if challenge.challenge_type == ChallengeType::KingOfTheHill {
            crate::services::ad_engine::clear_challenge_control(&st.db, id, c_id).await?;
        }
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    }
    engine_control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    // Tear down every running per-team + shared container this challenge owns
    // BEFORE its rows vanish — otherwise they run orphaned until the idle reaper.
    // Mirrors RSCTF `RemoveChallenge`'s container sweep (gated on container type).
    if challenge.challenge_type.is_container() {
        destroy_challenge_containers(&st, &challenge, false, true).await?;
    }
    if challenge.ad_self_hosted {
        st.byoc.disconnect_challenge(&st.db, c_id).await?;
    }

    // Reacquire game control before the test/definition gates. Engine writers
    // that do not touch a participation row still serialize with the final
    // evidence predicate and physical delete, and the established game -> test
    // -> definition order avoids cross-replica lock inversion.
    let final_locks =
        super::deletion_locks::acquire_game_test_deletion_locks(&st.db, id, deletion_admission)
            .await?;

    // Re-query under the shared game/test lock stack so a test created during
    // the earlier sweep cannot publish behind challenge deletion.
    destroy_test_container_locked(&st, c_id).await?;

    // Reacquire definition only after the slow provisioning sweeps. Test
    // creation uses test-lifecycle -> definition, so taking the same order here
    // avoids inversion while making the final attachment snapshot and physical
    // delete indivisible with every flag/attachment/repository definition edit.
    let mut final_definition_lock = deletion::acquire_definition_lock(st.pg(), id, c_id).await?;
    deletion::fence_challenge_deletion(final_definition_lock.transaction_mut(), id, c_id).await?;
    let deleted_artifacts = crate::services::blob_refs::delete_challenge_locked(
        final_definition_lock.transaction_mut(),
        c_id,
    )
    .await?;
    final_definition_lock.release().await?;
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
    flush_game_scoreboards(&st, id).await;
    Ok(MessageResponse::ok(""))
}

/// Outcome of the image-build seam: the terminal build status plus a captured
/// (and length-capped) log to surface on the challenge row.
pub(crate) struct BuildOutcome {
    pub(crate) status: ChallengeBuildStatus,
    pub(crate) log: Option<String>,
    /// Exact runtime reference produced by this attempt. A successful outcome
    /// always carries either a portable repository digest or, for a verified
    /// single-Docker-daemon topology, a daemon-local image id.
    pub(crate) image_digest: Option<String>,
}
