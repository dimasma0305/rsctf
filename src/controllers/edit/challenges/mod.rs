//! edit: challenge CRUD/attachments (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

mod attachments;
mod audit;
mod deletion;
#[cfg(test)]
mod deletion_tests;
mod hints;
mod lifecycle;
mod repo_push;
mod review;
mod scoring;
mod workload;

pub use attachments::update_attachment;
pub(crate) use attachments::{build_attachment, validate_remote_attachment_url};
pub use audit::{get_challenge_audit_meta, rebuild_challenge};
pub(crate) use deletion::reject_pending_mutation;
pub(crate) use lifecycle::destroy_challenge_containers;
use lifecycle::destroy_test_container_locked;
#[cfg(test)]
pub(crate) use repo_push::commit_latest_to_checkout_for_test;
pub use review::{approve_challenge, list_pending_challenges, reject_challenge};
pub use workload::rollout_workloads;

const INSERTABLE_GAME_SQL: &str =
    r#"SELECT NOT deletion_pending FROM "Games" WHERE id = $1 FOR SHARE"#;

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
    if model.challenge_type.uses_ad_engine() {
        if ad_epoch_scoring_started_locked(control.transaction_mut(), id).await? {
            return Err(AppError::bad_request(
                "A&D/KotH challenges cannot be added after epoch scoring has started.",
            ));
        }
        if model.challenge_type == ChallengeType::KingOfTheHill {
            super::games::validate_koth_game_shape_locked(control.transaction_mut(), id).await?;
        }
    }

    let am = game_challenge::ActiveModel {
        game_id: Set(id),
        title: Set(model.title),
        content: Set(String::new()),
        category: Set(model.category),
        challenge_type: Set(model.challenge_type),
        is_enabled: Set(false),
        submission_limit: Set(0),
        accepted_count: Set(0),
        submission_count: Set(0),
        review_status: Set(ChallengeReviewStatus::Active),
        build_status: Set(ChallengeBuildStatus::None),
        original_score: Set(1000),
        min_score_rate: Set(0.25),
        difficulty: Set(5.0),
        score_curve: Set(ScoreCurve::Standard),
        // RSCTF `Challenge.NetworkMode` defaults to `Open`.
        network_mode: Set(Some(NetworkMode::Open)),
        enable_traffic_capture: Set(false),
        enable_shared_container: Set(false),
        disable_blood_bonus: Set(false),
        ad_allow_egress: Set(false),
        ad_allow_self_reset: Set(false),
        ad_ssh_requires_flag: Set(false),
        ad_self_hosted: Set(false),
        ..Default::default()
    };
    let created = am.insert(&st.db).await?;
    if let Some(control) = engine_control {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    seed_division_configs(&st, id, created.id).await?;
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

/// `PUT /api/edit/games/{id}/challenges/{cId}`
pub async fn update_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
    Json(model): Json<ChallengeUpdateModel>,
) -> AppResult<RequestResponse<ChallengeEditDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    let game = load_game(&st, id).await?;
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
    let ch_type = challenge.challenge_type;
    crate::utils::scoring::validate_challenge_scoring(
        model.original_score.unwrap_or(challenge.original_score),
        model.min_score_rate.unwrap_or(challenge.min_score_rate),
        model.difficulty.unwrap_or(challenge.difficulty),
        model.submission_limit.unwrap_or(challenge.submission_limit),
    )?;
    let scoring_started = if ch_type.uses_ad_engine() {
        ad_epoch_scoring_started_locked(
            engine_control
                .as_mut()
                .expect("engine challenge holds the game control lock")
                .transaction_mut(),
            id,
        )
        .await?
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
    let final_enabled = model.is_enabled.unwrap_or(challenge.is_enabled);
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
    let active_topology_flip = challenge.is_enabled
        && final_enabled
        && (was_shared_managed != requested_shared_managed
            || was_ad_self_hosted != requested_ad_self_hosted);
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
        let fenced = sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET is_enabled = FALSE
                WHERE id = $1 AND game_id = $2
                  AND is_enabled = TRUE
                  AND deletion_pending = FALSE"#,
        )
        .bind(c_id)
        .bind(id)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if fenced.rows_affected() != 1 {
            return Err(AppError::conflict(
                "Challenge eligibility changed; retry the topology update",
            ));
        }
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
    let mut am: game_challenge::ActiveModel = update_base.into();
    if let Some(v) = model.title {
        am.title = Set(v);
    }
    if let Some(v) = model.content {
        am.content = Set(v);
    }
    if let Some(v) = model.flag_template {
        // RSCTF `GameChallenge.Update`: an empty/whitespace-only flag template is the
        // client's "clear" sentinel — store null instead of persisting an empty string.
        // Mirrors `FlagTemplate = string.IsNullOrWhiteSpace(template) ? null : template`.
        am.flag_template = Set(if v.trim().is_empty() { None } else { Some(v) });
    }
    if let Some(v) = model.category {
        am.category = Set(v);
    }
    if let Some(v) = model.hints {
        am.hints = Set(Some(serde_json::to_value(v).unwrap_or(JsonValue::Null)));
    }
    if let Some(v) = model.is_enabled {
        am.is_enabled = Set(v);
    } else if active_topology_flip {
        am.is_enabled = Set(final_enabled);
    }
    if let Some(v) = model.file_name {
        am.file_name = Set(Some(v));
    }
    if let Some(v) = model.deadline_utc {
        // RSCTF `GameChallenge.Update`: a deadline whose Unix timestamp is 0
        // (the epoch) is the client's "clear the deadline" sentinel — store null
        // instead of persisting 1970-01-01. Mirrors
        // `DeadlineUtc = time.ToUnixTimeSeconds() == 0 ? null : time`.
        am.deadline_utc = Set(if v.timestamp() == 0 { None } else { Some(v) });
    }
    if let Some(v) = model.submission_limit {
        am.submission_limit = Set(v);
    }
    if let Some(v) = model.container_image {
        // Trim: a pasted image ref with trailing whitespace would fail the pull.
        am.container_image = Set(Some(v.trim().to_string()));
    }
    if let Some(status) = invalidated_build_status {
        am.build_status = Set(status);
        am.build_image_digest = Set(None);
        am.last_build_log = Set(None);
    }
    if let Some(v) = model.memory_limit {
        am.memory_limit = Set(Some(v));
    }
    if let Some(v) = model.cpu_count {
        am.cpu_count = Set(Some(v));
    }
    if let Some(v) = model.storage_limit {
        am.storage_limit = Set(Some(v));
    }
    if let Some(v) = model.expose_port {
        am.expose_port = Set(Some(v));
    }
    if let Some(v) = workload_update {
        am.workload_spec = Set(v);
    }
    if let Some(v) = model.original_score {
        am.original_score = Set(v);
    }
    if let Some(v) = model.min_score_rate {
        am.min_score_rate = Set(v);
    }
    if let Some(v) = model.difficulty {
        am.difficulty = Set(v);
    }
    if let Some(v) = model.score_curve {
        am.score_curve = Set(v);
    }
    if let Some(v) = model.enable_traffic_capture {
        am.enable_traffic_capture = Set(v);
    }
    if let Some(v) = model.disable_blood_bonus {
        am.disable_blood_bonus = Set(v);
    }
    // Container network mode — RSCTF `Update`: `NetworkMode = model.NetworkMode ??
    // NetworkMode` (only overwrite when the client actually sent one).
    if let Some(v) = model.network_mode {
        am.network_mode = Set(Some(v));
    }
    // Shared instance is only meaningful for StaticContainer (single shared
    // static flag); force it off for every other type so a stale toggle can't
    // take effect after a retype. Mirror of RSCTF `GameChallenge.Update`.
    am.enable_shared_container = Set(ch_type == ChallengeType::StaticContainer
        && model.enable_shared_container.unwrap_or(old_shared));
    // --- Attack & Defense per-challenge knobs ---
    if let Some(v) = model.ad_checker_image {
        am.ad_checker_image = Set(Some(v.trim().to_string()));
    }
    if let Some(v) = model.ad_allow_egress {
        am.ad_allow_egress = Set(v);
    }
    if let Some(v) = model.ad_allow_self_reset {
        am.ad_allow_self_reset = Set(v);
    }
    if let Some(v) = model.ad_ssh_requires_flag {
        am.ad_ssh_requires_flag = Set(v);
    }
    if let Some(v) = model.ad_self_hosted {
        am.ad_self_hosted = Set(v);
    }
    if let Some(v) = model.ad_scoring_weight {
        am.ad_scoring_weight = Set(v);
    }

    let updated = am.update(&st.db).await?;
    workload::release_update_lock(workload_lock.take()).await?;
    if ch_type == ChallengeType::KingOfTheHill && !updated.is_enabled {
        crate::services::ad_engine::clear_challenge_control(&st.db, id, c_id).await?;
    }
    if let Some(lock) = engine_control {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if (was_ad_self_hosted || updated.ad_self_hosted)
        && (!updated.ad_self_hosted
            || !updated.is_enabled
            || updated.review_status != ChallengeReviewStatus::Active)
    {
        st.byoc.disconnect_challenge(&st.db, c_id).await?;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    // Keep the per-division challenge config table seeded (insert-if-missing so
    // divisions added after the challenge was created still get a default row).
    seed_division_configs(&st, id, c_id).await?;
    flush_game_scoreboards(&st, id).await;

    // Tear down containers stranded by this edit (RSCTF `UpdateGameChallenge`):
    //   • a shared↔per-team mode flip in EITHER direction orphans the containers
    //     created under the old mode (`wasSharedManaged != res.UsesSharedContainer`);
    //   • disabling a container challenge (`case false when res.Type.IsContainer()`)
    //     reaps its running per-team/shared containers.
    // Both route through `DestroyAllContainers`; call it once for either trigger.
    // Best-effort: teardown failures never fail the edit.
    let now_shared_managed = uses_shared_container(&updated);
    if (!active_topology_flip
        && (was_shared_managed != now_shared_managed
            || was_ad_self_hosted != updated.ad_self_hosted))
        || (model.is_enabled == Some(false) && updated.challenge_type.is_container())
    {
        let _ = destroy_challenge_containers(&st, &updated, model.is_enabled == Some(false), false)
            .await;
    }

    // A challenge going live mid-game (IsEnabled false->true) is announced as a
    // NewChallenge game notice so connected players see it appear in real time.
    // Mirrors RSCTF `EditController.UpdateGameChallenge` (the `case true` arm:
    // `AddNotice(Type = NewChallenge, Values = [res.Title], broadcast: true)`),
    // gated on `game.IsActive` exactly as RSCTF gates it.
    if updated.is_enabled && !was_enabled && game.is_active(Utc::now()) {
        let notice = game_notice::ActiveModel {
            game_id: Set(id),
            notice_type: Set(NoticeType::NewChallenge),
            values: Set(serde_json::json!([updated.title])),
            publish_time_utc: Set(Utc::now()),
            ..Default::default()
        };
        let notice = notice.insert(&st.db).await?;

        // Broadcast to the user hub's `ReceivedGameNotice` so clients refresh
        // (same envelope as blood/normal notices: type/values/id/time).
        st.publish_event(
            "ReceivedGameNotice",
            Some(id),
            serde_json::json!({
                "type": notice.notice_type,
                "values": notice.values,
                "id": notice.id,
                "time": notice.publish_time_utc,
            })
            .to_string(),
        );
    }

    // A new/changed hint on a live, enabled challenge is announced as a NewHint
    // game notice so connected players see the fresh hint in real time. Mirrors
    // RSCTF `EditController.UpdateGameChallenge`:
    // `if (game.IsActive && res.IsEnabled && hintUpdated) AddNotice(NewHint, [res.Title], broadcast)`.
    // Note the gate is the POST-update `res.IsEnabled` (not the false->true
    // transition NewChallenge keys off), so re-hinting an already-live challenge
    // still notifies.
    if game.is_active(Utc::now()) && updated.is_enabled && hints_changed {
        let notice = game_notice::ActiveModel {
            game_id: Set(id),
            notice_type: Set(NoticeType::NewHint),
            values: Set(serde_json::json!([updated.title])),
            publish_time_utc: Set(Utc::now()),
            ..Default::default()
        };
        let notice = notice.insert(&st.db).await?;

        st.publish_event(
            "ReceivedGameNotice",
            Some(id),
            serde_json::json!({
                "type": notice.notice_type,
                "values": notice.values,
                "id": notice.id,
                "time": notice.publish_time_utc,
            })
            .to_string(),
        );
    }

    if let Some(lock) = runtime_transition {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }

    // Repo push-back (RSCTF `EditController.TryPushBackAsync`): when this
    // challenge's game is repo-bound and the binding opts into `push_on_edit`,
    // regenerate `challenge.yml` from the updated row and git-push it upstream.
    // Fire-and-forget + best-effort — a slow or failed push must never extend or
    // fail the operator's edit-save round trip.
    if game.repo_binding_id.is_some() {
        repo_push::spawn(st.clone(), id, c_id);
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
