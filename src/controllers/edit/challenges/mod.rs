//! edit: challenge CRUD/attachments (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

mod attachments;
mod audit;
mod hints;
mod lifecycle;
mod review;
mod scoring;
mod workload;

pub(crate) use attachments::build_attachment;
pub use attachments::update_attachment;
pub use audit::{get_challenge_audit_meta, rebuild_challenge};
use lifecycle::{
    destroy_after_capture_fence, destroy_container_row_after_capture_fence,
    destroy_shared_container_after_capture_fence, destroy_test_container_locked,
    mark_challenge_deleting,
};
pub use review::{approve_challenge, list_pending_challenges, reject_challenge};
pub use workload::rollout_workloads;

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

    let mut engine_control = if model.challenge_type.uses_ad_engine() {
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?)
    } else {
        None
    };
    if let Some(control) = engine_control.as_mut() {
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

/// Best-effort teardown of every running container a challenge owns — the per-team
/// containers materialized as `game_instance` rows plus the challenge-owned shared
/// container (`shared_container_id`). Mirrors RSCTF
/// `GameInstanceRepository.DestroyAllContainers` / `RemoveChallenge`'s container
/// sweep, and the single-container teardown in `game::containers::destroy_container`.
/// Returns `()` and swallows every error (Docker **and** the bookkeeping DB writes)
/// so a broken daemon or a mid-sweep hiccup can never fail the operator's edit or
/// delete; the remaining rows are cleared on a best-effort basis (matches how RSCTF
/// wraps each `DestroyContainer`).
pub(crate) async fn destroy_challenge_containers(
    st: &SharedState,
    challenge: &game_challenge::Model,
) {
    // A&D services are not GameInstances. Clear their routable endpoints first;
    // only release a backend address after the local VPN policy acknowledges the
    // revocation. This phase deliberately holds no shared-container lock: nesting
    // two `acquire_provisioning` calls self-deadlocks when the provisioning gate has
    // one permit.
    if let Ok(services) = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::ChallengeId.eq(challenge.id))
        .all(&st.db)
        .await
    {
        for service in services {
            let lock_key = format!(
                "ad-service:{}:{}",
                service.participation_id, service.challenge_id
            );
            let _local = crate::utils::single_flight::coalesce(&lock_key).await;
            let distributed =
                match crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
                    st.pg(),
                    &lock_key,
                )
                .await
                {
                    Ok(lock) => lock,
                    Err(error) => {
                        tracing::warn!(challenge = challenge.id, service = service.id, %error,
                        "challenge teardown: service lock failed");
                        continue;
                    }
                };
            let current = ad_team_service::Entity::find_by_id(service.id)
                .one(&st.db)
                .await
                .ok()
                .flatten()
                .filter(|row| row.challenge_id == challenge.id);
            let container_id = current.as_ref().and_then(|row| row.container_id.clone());
            match current {
                None => {}
                Some(current) => {
                    match crate::services::ad_vpn::deactivate_team_service(&st.db, current.id).await
                    {
                        Ok(()) => {
                            if let Some(container_id) = container_id {
                                let _ = destroy_after_capture_fence(st, &container_id).await;
                            }
                        }
                        Err(error) => tracing::warn!(
                            challenge = challenge.id,
                            service = current.id,
                            %error,
                            "challenge teardown: endpoint revocation failed; retaining container"
                        ),
                    }
                }
            }
            if let Err(error) = distributed.release().await {
                tracing::warn!(challenge = challenge.id, service = service.id, %error,
                    "challenge teardown: service unlock failed");
            }
        }
    }

    // Per-team containers are serialized by participation, matching create/delete/
    // extend. Enumerate every participation owner in the game, then re-query the
    // challenge instance only after its lock is held. Including all participations
    // (not merely the pre-lock rows with a container) closes the publish-after-sweep
    // race for a first container creation.
    let participation_ids = match sqlx::query_scalar::<_, i32>(
        r#"SELECT id
             FROM "Participations"
            WHERE game_id = $1
            UNION
           SELECT participation_id
             FROM "GameInstances"
            WHERE challenge_id = $2
            ORDER BY 1"#,
    )
    .bind(challenge.game_id)
    .bind(challenge.id)
    .fetch_all(st.pg())
    .await
    {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(challenge = challenge.id, %error,
                "challenge teardown: listing participation owners failed");
            Vec::new()
        }
    };
    for participation_id in participation_ids {
        let key = format!("game-container:{participation_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let distributed = match crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
            st.pg(),
            &key,
        )
        .await
        {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(challenge = challenge.id, participation = participation_id, %error,
                        "challenge teardown: instance lock failed");
                continue;
            }
        };
        let instances = game_instance::Entity::find()
            .filter(game_instance::Column::ParticipationId.eq(participation_id))
            .filter(game_instance::Column::ChallengeId.eq(challenge.id))
            .all(&st.db)
            .await;
        match instances {
            Ok(instances) => {
                for inst in instances {
                    let Some(cuuid) = inst.container_id else {
                        continue;
                    };
                    if destroy_container_row_after_capture_fence(st, cuuid)
                        .await
                        .is_ok()
                    {
                        let mut active: game_instance::ActiveModel = inst.into();
                        active.container_id = Set(None);
                        active.is_loaded = Set(false);
                        active.last_container_operation = Set(Utc::now());
                        let _ = active.update(&st.db).await;
                    }
                }
            }
            Err(error) => {
                tracing::warn!(challenge = challenge.id, participation = participation_id, %error,
                "challenge teardown: instance re-query failed")
            }
        }
        if let Err(error) = distributed.release().await {
            tracing::warn!(challenge = challenge.id, participation = participation_id, %error,
                "challenge teardown: instance unlock failed");
        }
    }

    // KotH target publication and the shared challenge pointer use one final shared
    // phase. The pointer is intentionally loaded after lock acquisition rather than
    // trusted from the edit handler's stale challenge snapshot.
    let shared_key = format!("shared-container:{}", challenge.id);
    let _shared_flight = crate::utils::single_flight::coalesce(&shared_key).await;
    let shared_lock = match crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
        st.pg(),
        &shared_key,
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(challenge = challenge.id, %error, "challenge teardown lock failed");
            return;
        }
    };

    if let Ok(targets) = koth_target::Entity::find()
        .filter(koth_target::Column::ChallengeId.eq(challenge.id))
        .filter(koth_target::Column::ContainerId.is_not_null())
        .all(&st.db)
        .await
    {
        for target in targets {
            let Some(container_id) = target.container_id.clone() else {
                continue;
            };
            let mut active: koth_target::ActiveModel = target.into();
            active.host = Set(String::new());
            active.port = Set(0);
            active.container_id = Set(None);
            active.holder_participation_id = Set(None);
            active.held_since = Set(None);
            if active.update(&st.db).await.is_ok()
                && crate::services::ad_vpn::ensure_hub_and_sync(&st.db)
                    .await
                    .is_ok()
            {
                let _ = st.containers.destroy(&container_id).await;
            }
        }
    }

    if let Ok(Some(current_challenge)) = game_challenge::Entity::find_by_id(challenge.id)
        .one(&st.db)
        .await
    {
        if let Some(sid) = current_challenge.shared_container_id {
            if destroy_shared_container_after_capture_fence(st, sid)
                .await
                .is_ok()
            {
                let mut active: game_challenge::ActiveModel = current_challenge.into();
                active.shared_container_id = Set(None);
                let _ = active.update(&st.db).await;
            }
        }
    }
    if let Err(error) = shared_lock.release().await {
        tracing::warn!(challenge = challenge.id, %error, "challenge teardown unlock failed");
    }
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
    let mut workload_lock =
        workload::acquire_update_lock_for_model(st.pg(), id, c_id, &model).await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    let ch_type = challenge.challenge_type;
    crate::utils::scoring::validate_challenge_scoring(
        model.original_score.unwrap_or(challenge.original_score),
        model.min_score_rate.unwrap_or(challenge.min_score_rate),
        model.difficulty.unwrap_or(challenge.difficulty),
        model.submission_limit.unwrap_or(challenge.submission_limit),
    )?;
    let mut engine_control = if ch_type.uses_ad_engine() {
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?)
    } else {
        None
    };
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
    // Whether the client's hints array differs from the stored one (RSCTF
    // `hintUpdated`) — captured before `model.hints` is consumed below; drives the
    // NewHint notice further down.
    let hints_changed = model
        .hints
        .as_ref()
        .is_some_and(|h| hints::updated(challenge.hints.as_ref(), h));
    let workload_update = workload::validate_update(&challenge, &model.workload_spec)?;

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

    let mut am: game_challenge::ActiveModel = challenge.into();
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
    if was_shared_managed != now_shared_managed
        || was_ad_self_hosted != updated.ad_self_hosted
        || (model.is_enabled == Some(false) && updated.challenge_type.is_container())
    {
        destroy_challenge_containers(&st, &updated).await;
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

    // Repo push-back (RSCTF `EditController.TryPushBackAsync`): when this
    // challenge's game is repo-bound and the binding opts into `push_on_edit`,
    // regenerate `challenge.yml` from the updated row and git-push it upstream.
    // Fire-and-forget + best-effort — a slow or failed push must never extend or
    // fail the operator's edit-save round trip.
    if let Some(bid) = game.repo_binding_id {
        spawn_push_back(st.clone(), bid, updated.clone());
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

/// Spawn the fire-and-forget repo push-back for an edited challenge (RSCTF
/// `EditController.TryPushBackAsync`'s `Task.Run`). Any failure is logged and
/// swallowed — the in-DB edit is the operator's source of truth, so a git
/// push-back problem must not surface as a 5xx on the edit.
fn spawn_push_back(st: SharedState, binding_id: i32, challenge: game_challenge::Model) {
    tokio::spawn(async move {
        let (cid, title) = (challenge.id, challenge.title.clone());
        if let Err(e) = push_back(&st, binding_id, &challenge).await {
            tracing::warn!(
                binding = binding_id,
                challenge = cid,
                title = %title,
                error = %e,
                "push-back: failed (best-effort; edit already committed)"
            );
        }
    });
}

/// Regenerate `challenge.yml` from the DB row and push it to the binding repo.
/// Mirrors the body of RSCTF `TryPushBackAsync`: gate on the binding's
/// `push_on_edit` + token, sync the checkout to HEAD, locate the yaml, overwrite
/// it from [`serialize_challenge`](crate::services::git_sync::serialize_challenge),
/// then commit+push via [`push_file`](crate::services::git_sync::push_file).
async fn push_back(
    st: &SharedState,
    binding_id: i32,
    challenge: &game_challenge::Model,
) -> AppResult<()> {
    use crate::models::data::repo_binding;
    use crate::services::git_sync;

    // Binding must exist, opt into push-on-edit, and carry a token.
    let Some(binding) = repo_binding::Entity::find_by_id(binding_id)
        .one(&st.db)
        .await?
    else {
        return Ok(());
    };
    if !binding.push_on_edit {
        return Ok(());
    }
    let token = match binding.github_token.as_deref() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            tracing::info!(
                binding = binding_id,
                "push-back: binding has push_on_edit but no token; skipping"
            );
            return Ok(());
        }
    };
    let repo_url = git_sync::validate_binding_repo_url(&binding.repo_url)?;
    let git_ref = git_sync::validate_git_ref(binding.git_ref.as_deref())?;

    // Per-binding checkout dir — same convention as the scan path
    // (`{storage_root}/repos/{binding_id}`).
    let dest = std::path::PathBuf::from(&st.config.storage_root)
        .join("repos")
        .join(binding_id.to_string());
    let _checkout_lock = git_sync::lock_checkout_distributed(st.pg(), &dest).await?;

    // Ensure the checkout exists and is at HEAD before overlaying the new yaml.
    let auth_url = git_sync::GitCredentials::new(token.clone()).apply(&repo_url);
    git_sync::sync_repo(&auth_url, git_ref.as_deref(), &dest).await?;

    // Flags to serialize. A DynamicContainer plants per-instance flags at runtime
    // and carries none in the manifest — leave the yaml's `flag_template` as-is.
    let flag_texts: Vec<String> = if challenge.challenge_type == ChallengeType::DynamicContainer {
        Vec::new()
    } else {
        flag_context::Entity::find()
            .filter(flag_context::Column::ChallengeId.eq(challenge.id))
            .all(&st.db)
            .await?
            .into_iter()
            .filter_map(|f| {
                let f = f.flag.trim().to_string();
                (!f.is_empty()).then_some(f)
            })
            .collect()
    };

    // Locate the yaml in the checkout.
    let Some(yaml_abs) = locate_challenge_yaml(&dest, challenge).await else {
        tracing::warn!(
            binding = binding_id,
            challenge = challenge.id,
            "push-back: could not locate challenge.yml in checkout (operator may have moved/renamed it); skipping"
        );
        return Ok(());
    };
    let rel = yaml_abs
        .strip_prefix(&dest)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| yaml_abs.to_string_lossy().to_string());

    let yaml = git_sync::serialize_challenge(challenge, &flag_texts)?;
    tokio::fs::write(&yaml_abs, yaml)
        .await
        .map_err(|e| AppError::internal(format!("push-back: write {}: {e}", yaml_abs.display())))?;

    let msg = format!("chore: update {} from rsctf admin edit", challenge.title);
    git_sync::push_file(&dest, &rel, &repo_url, &token, &msg).await?;
    tracing::info!(
        binding = binding_id,
        challenge = challenge.id,
        yaml = %rel,
        "push-back: pushed"
    );
    Ok(())
}

/// Find the `challenge.yml` in the checkout that corresponds to `challenge`.
/// Prefers the path recorded at import (`source_yaml_path`, absolute + rooted at
/// this checkout) so a title/category rename still resolves; falls back to
/// scanning every manifest and matching by challenge name (RSCTF locates purely
/// via `SourceYamlPath` — the name fallback covers rows imported before that
/// column was populated). Returns `None` when nothing matches.
async fn locate_challenge_yaml(
    dest: &std::path::Path,
    challenge: &game_challenge::Model,
) -> Option<std::path::PathBuf> {
    // 1. Recorded import path (must live inside this checkout and still exist).
    if let Some(p) = challenge
        .source_yaml_path
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        let abs = std::path::PathBuf::from(p);
        if abs.starts_with(dest) && tokio::fs::try_exists(&abs).await.unwrap_or(false) {
            return Some(abs);
        }
    }
    // 2. Fallback: discover manifests, match by challenge name.
    let manifests = crate::services::git_sync::discover_challenges(dest)
        .await
        .ok()?;
    for m in manifests {
        let Ok(raw) = tokio::fs::read_to_string(&m).await else {
            continue;
        };
        if let Ok(y) = serde_norway::from_str::<crate::services::git_sync::ChallengeYaml>(&raw) {
            if y.name.as_deref().map(str::trim) == Some(challenge.title.trim()) {
                return Some(m);
            }
        }
    }
    None
}

/// `DELETE /api/edit/games/{id}/challenges/{cId}` — void.
pub async fn delete_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, c_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, id).await?;
    let challenge = load_challenge(&st, id, c_id).await?;
    let mut engine_control = if challenge.challenge_type.uses_ad_engine() {
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?)
    } else {
        None
    };
    if challenge.challenge_type.uses_ad_engine()
        && ad_epoch_scoring_started_locked(
            engine_control
                .as_mut()
                .expect("engine challenge holds the game control lock")
                .transaction_mut(),
            id,
        )
        .await?
    {
        return Err(AppError::bad_request(
            "A&D/KotH challenges cannot be deleted after epoch scoring has started.",
        ));
    }

    // Establish a durable deny-new-creates marker before any teardown snapshot.
    // The row is about to be deleted, so disabling non-A&D challenges here has no
    // user-visible intermediate state and closes publish-after-sweep races.
    mark_challenge_deleting(&st, c_id).await?;
    // Revoke A&D/KotH routes before any backing address can be freed.
    if challenge.challenge_type.uses_ad_engine() {
        if challenge.challenge_type == ChallengeType::KingOfTheHill {
            crate::services::ad_engine::clear_challenge_control(&st.db, id, c_id).await?;
        }
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    }
    if let Some(lock) = engine_control {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }

    // Collect the flags' hand-out attachments before deleting the rows so we can
    // release their ref-counted blobs afterwards.
    let flags = flag_context::Entity::find()
        .filter(flag_context::Column::ChallengeId.eq(c_id))
        .all(&st.db)
        .await?;
    let flag_attachment_ids: Vec<i32> = flags.iter().filter_map(|f| f.attachment_id).collect();
    let challenge_attachment_id = challenge.attachment_id;

    // Tear down every running per-team + shared container this challenge owns
    // BEFORE its rows vanish — otherwise they run orphaned until the idle reaper.
    // Mirrors RSCTF `RemoveChallenge`'s container sweep (gated on container type).
    // Best-effort: never fail the delete.
    if challenge.challenge_type.is_container() {
        destroy_challenge_containers(&st, &challenge).await;
    }
    if challenge.ad_self_hosted {
        st.byoc.disconnect_challenge(&st.db, c_id).await?;
    }

    // Take the game's single test lifecycle gate only after all other provisioning
    // phases released their permits. Re-query under it so a test created during
    // the earlier sweep cannot publish behind challenge deletion.
    let test_key = format!("test-containers-game:{id}");
    let _test_local = crate::utils::single_flight::coalesce(&test_key).await;
    let test_lock =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &test_key)
            .await?;
    destroy_test_container_locked(&st, c_id).await?;
    let _deleted_artifacts =
        crate::services::blob_refs::delete_challenge(st.pg(), st.storage.as_ref(), c_id).await?;
    test_lock.release().await?;

    // Release the now-orphaned attachment blobs (clear-FK-first: rows above are
    // already gone).
    for aid in flag_attachment_ids {
        delete_attachment(&st, aid).await?;
    }
    if let Some(aid) = challenge_attachment_id {
        delete_attachment(&st, aid).await?;
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
