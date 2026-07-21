//! Per-team dynamic container lifecycle (create/destroy/extend).
use super::*;

mod eligibility;
use eligibility::{
    load_eligible_shared_challenge, player_container_request_is_eligible, ContainerRequestMode,
};
mod publication;
pub(crate) use publication::refresh_shared_container_lease_locked;
use publication::{
    revoke_failed_team_container_publication, revoke_published_shared_container,
    revoke_published_team_container,
};
mod policy;
use policy::{
    allows_practice_container, container_op_too_frequent, CONTAINER_RENEWAL_WINDOW_MINUTES,
};
mod reaping;
pub(crate) use reaping::destroy_managed_container_row;
mod workload_fence;
use workload_fence::{
    acquire_playable_publication_lock, acquire_shared_publication_lock,
    load_playable_definition_snapshot, load_shared_definition_snapshot,
};

const CONTAINER_LIFETIME_HOURS: i64 = 2;

/// `POST /api/game/{id}/container/{challengeId}` — provision a per-team dynamic
/// container (mirrors RSCTF `GameInstanceRepository.CreateContainer`).
pub async fn create_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, cid)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ContainerInfoModel>> {
    let ctx = context_info(&st, &user, id, true).await?;

    let challenge = load_playable_challenge(&st, id, cid).await?;
    // Division may restrict viewing (hence provisioning) this challenge: lacking
    // ViewChallenge hides it as a 404, mirroring the identical gate `get_challenge`
    // uses (RSCTF `FilterChallengesByPermission` / CreateContainer visibility).
    let perm = effective_permission(&st, &ctx.participation, cid).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("The challenge was not found"));
    }
    if !challenge.challenge_type.is_container() {
        return Err(AppError::bad_request("Challenge has no container"));
    }
    // A&D / KotH challenges share `is_container()`, but their per-team service is
    // owned by the live A&D engine during the game — the jeopardy container flow
    // must not spin one up (RSCTF `CreateContainer`, GameController.cs:1947). Only a
    // practice-mode game that has already ended lets a standalone container through.
    if challenge.challenge_type.uses_ad_engine()
        && !allows_practice_container(&challenge, &ctx.game)
    {
        return Err(AppError::bad_request(
            "Container creation is not allowed for this challenge",
        ));
    }

    // Shared container: one challenge-owned container serves every team. Get-or-create
    // it (idempotent) and hand it back directly — no per-team GameInstance/flag row.
    // Mirrors RSCTF `CreateContainer` (UsesSharedContainer branch, GameController.cs:1953)
    // + `GameInstanceRepository.GetOrCreateSharedContainer`.
    if uses_shared_container(&challenge) {
        let flight_key = format!("shared-container:{}", challenge.id);
        let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &flight_key)
                .await?;
        let result = async {
            if !player_container_request_is_eligible(
                &st,
                user.id,
                id,
                ctx.participation.id,
                cid,
                ContainerRequestMode::Shared,
            )
            .await?
            {
                return Err(AppError::Forbidden);
            }
            let c = get_or_create_shared_container_locked(&st, &challenge).await?;
            // The shared backend remains a valid challenge-level resource when only
            // this caller loses eligibility, but the stale request must not receive
            // its endpoint after the potentially slow backend operation.
            if !player_container_request_is_eligible(
                &st,
                user.id,
                id,
                ctx.participation.id,
                cid,
                ContainerRequestMode::Shared,
            )
            .await?
            {
                return Err(AppError::Forbidden);
            }
            Ok(RequestResponse::ok(ContainerInfoModel::from(&c)))
        }
        .await;
        distributed.release().await?;
        return result;
    }

    // Serialize all starts for one participation. This closes both the duplicate
    // (participation, challenge) race and the cross-challenge container-cap race.
    let flight_key = format!("game-container:{}", ctx.participation.id);
    let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &flight_key)
            .await?;

    if !player_container_request_is_eligible(
        &st,
        user.id,
        id,
        ctx.participation.id,
        cid,
        ContainerRequestMode::PerTeam,
    )
    .await?
    {
        distributed.release().await?;
        return Err(AppError::Forbidden);
    }

    // Everything below uses a post-lock snapshot. In particular, do not launch an
    // image or generate a flag from the cached context that authorized the request
    // before it waited behind another lifecycle operation. Full ORM entities are
    // retained here because flag/spec construction consumes their enum-rich models;
    // the authorization decision itself remains the raw SQL predicate above.
    let participation = participation::Entity::find_by_id(ctx.participation.id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Participation not found"))?;
    let game = game::Entity::find_by_id(id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    let (challenge, workload, identity, publication_fence) =
        load_playable_definition_snapshot(&st, id, cid).await?;

    // Look up any prior instance for this challenge. A live (Running) container is a
    // hard error — RSCTF returns 400 Game_ContainerAlreadyCreated rather than handing
    // back the existing one. A stale (non-Running) container is torn down so we can
    // re-provision cleanly.
    let mut existing = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.eq(participation.id))
        .filter(game_instance::Column::ChallengeId.eq(cid))
        .one(&st.db)
        .await?;
    // Per-instance frequency gate (RSCTF `CreateContainer`, GameController.cs:1962):
    // reject a create within the cooldown of this instance's last container operation,
    // BEFORE the running/stale teardown branch. A first-ever create (no prior instance)
    // is never throttled — RSCTF's `LastContainerOperation` defaults to `MinValue`.
    if let Some(inst) = &existing {
        if let Some(err) = container_op_too_frequent(inst) {
            return Err(err);
        }
    }
    if let Some(mut inst) = existing.take() {
        if let Some(cuuid) = inst.container_id {
            if let Some(c) = container::Entity::find_by_id(cuuid).one(&st.db).await? {
                if c.status == ContainerStatus::Running
                    && crate::services::challenge_workloads::existing_runtime_is_reusable(
                        st.containers.as_ref(),
                        &c.container_id,
                        &c.image,
                        &identity,
                    )
                    .await?
                {
                    return Err(AppError::bad_request(
                        "The container of this challenge already exists",
                    ));
                }
                // The Containers row is the durable retry owner. Clear the exact
                // instance link only after capture is fenced and destroy succeeds.
                revoke_published_team_container(&st, &c.container_id, c.id, inst.id, None, None)
                    .await?;
                inst.container_id = None;
                inst.is_loaded = false;
            }
        }
        existing = Some(inst);
    }

    // Enforce the game's per-participation container cap (0 = unlimited). Count the
    // participation's other live containers; RSCTF denies creation once the team is at
    // the limit (Game_ContainerNumberLimitExceeded).
    if game.container_count_limit > 0 {
        let running = game_instance::Entity::find()
            .filter(game_instance::Column::ParticipationId.eq(participation.id))
            .filter(game_instance::Column::ContainerId.is_not_null())
            .filter(game_instance::Column::ChallengeId.ne(cid))
            .count(&st.db)
            .await?;
        if running >= game.container_count_limit as u64 {
            return Err(AppError::bad_request(format!(
                "The number of team containers cannot exceed {}",
                game.container_count_limit
            )));
        }
    }

    // Flag to inject: a DynamicContainer gets a per-team dynamic flag; a
    // StaticContainer serves the challenge's STATIC flag (identical for every
    // team — the one a player reads off the page and submits). Generating a
    // per-team flag for a static container made the submitted static flag never
    // match, so a StaticContainer solve always failed.
    let selected_static_flag = crate::services::challenge_workloads::load_selected_static_flag(
        st.pg(),
        cid,
        challenge.challenge_type,
    )
    .await?;
    let flag = if challenge.challenge_type == ChallengeType::DynamicContainer {
        let salt = flag_generator::team_hash_salt(&game.private_key);
        let team_hash = flag_generator::team_challenge_hash(&salt, cid, &participation.token);
        flag_generator::generate_flag(challenge.flag_template.as_deref(), &team_hash)
    } else {
        selected_static_flag.clone().unwrap_or_default()
    };
    let container_uuid = uuid::Uuid::new_v4();
    let operation_id = Some(format!("container:{container_uuid}"));
    let info = match workload {
        Some(spec) => {
            let spec = crate::services::challenge_workloads::with_environment(
                spec,
                "RSCTF_TEAM_ID",
                participation.team_id.to_string(),
            )?;
            st.containers
                .create_workload(spec, operation_id, Some(flag.clone()))
                .await?
        }
        None => {
            return Err(AppError::bad_request(
                "container launch requires workloadSpec; legacy single-container runtime is no longer supported",
            ));
        }
    };

    let backend_id = info.id.clone();
    match player_container_request_is_eligible(
        &st,
        user.id,
        id,
        participation.id,
        cid,
        ContainerRequestMode::PerTeam,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => {
            if let Err(error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, %error, "stale unpublished container destroy failed");
            }
            distributed.release().await?;
            return Err(AppError::Forbidden);
        }
        Err(error) => {
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "unpublished container destroy failed after authorization error");
            }
            let _ = distributed.release().await;
            return Err(error);
        }
    }

    // If Save+rollout won while the worker was launching, this runtime was not
    // visible to rollout's query. Destroy only this unpublished old generation
    // and ask the caller to retry. Otherwise retain the fence through metadata
    // publication, so a later rollout is guaranteed to discover the new row.
    let definition_lock = match acquire_playable_publication_lock(
        &st,
        id,
        cid,
        &publication_fence,
        selected_static_flag.as_deref(),
    )
    .await
    {
        Ok(lock) => lock,
        Err(error) => {
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "unpublished stale-definition container destroy failed");
            }
            distributed.release().await?;
            return Err(error);
        }
    };
    let mut created_flag_id = None;
    let mut created_instance_id = None;
    let existing_instance_id = existing.as_ref().map(|instance| instance.id);
    let persisted: AppResult<(container::Model, chrono::DateTime<Utc>)> = async {
        let now = Utc::now();
        let stop_at = now + chrono::Duration::hours(CONTAINER_LIFETIME_HOURS);

        // Only a DynamicContainer needs a per-team FlagContext + an instance flag_id;
        // static containers use the challenge's shared static flag row.
        let dyn_flag_id = if challenge.challenge_type == ChallengeType::DynamicContainer {
            let flag_row = flag_context::ActiveModel {
                flag: Set(flag),
                is_occupied: Set(true),
                attachment_id: Set(None),
                challenge_id: Set(Some(cid)),
                exercise_id: Set(None),
                ..Default::default()
            }
            .insert(&st.db)
            .await?;
            created_flag_id = Some(flag_row.id);
            Some(flag_row.id)
        } else {
            None
        };

        let instance = match existing {
            Some(inst) => inst,
            None => {
                let instance = game_instance::ActiveModel {
                    challenge_id: Set(cid),
                    participation_id: Set(participation.id),
                    is_loaded: Set(true),
                    last_container_operation: Set(now),
                    flag_id: Set(dyn_flag_id),
                    container_id: Set(None),
                    ..Default::default()
                }
                .insert(&st.db)
                .await?;
                created_instance_id = Some(instance.id);
                instance
            }
        };

        let is_proxy = st.containers.requires_proxy()
            || crate::controllers::admin::container_port_mapping(&st).await == "PlatformProxy";
        let c = container::ActiveModel {
            id: Set(container_uuid),
            image: Set(identity),
            container_id: Set(info.id),
            status: Set(ContainerStatus::Running),
            started_at: Set(now),
            expect_stop_at: Set(stop_at),
            is_proxy: Set(is_proxy),
            ip: Set(info.ip),
            port: Set(info.port),
            public_ip: Set(None),
            public_port: Set(None),
            game_instance_id: Set(Some(instance.id)),
            exercise_instance_id: Set(None),
            ad_team_service_id: Set(None),
        }
        .insert(&st.db)
        .await?;

        let mut inst_am: game_instance::ActiveModel = instance.into();
        inst_am.container_id = Set(Some(container_uuid));
        inst_am.flag_id = Set(dyn_flag_id);
        inst_am.is_loaded = Set(true);
        inst_am.last_container_operation = Set(now);
        inst_am.update(&st.db).await?;
        Ok((c, now))
    }
    .await;
    definition_lock.release().await?;

    let (c, now) = match persisted {
        Ok(value) => value,
        Err(err) => {
            if let Err(cleanup_error) = revoke_failed_team_container_publication(
                &st,
                &backend_id,
                container_uuid,
                created_instance_id.or(existing_instance_id),
                created_instance_id,
                created_flag_id,
            )
            .await
            {
                tracing::error!(
                    %backend_id,
                    %cleanup_error,
                    "team container publication rollback failed; retaining durable owner for retry"
                );
                return Err(AppError::internal(format!(
                    "{err}; container rollback failed: {cleanup_error}"
                )));
            }
            return Err(err);
        }
    };

    // Publication itself is not instantaneous. Re-check once more after every DB
    // link exists: if a team/game/challenge teardown swept before those rows became
    // visible, this request now owns enough information to revoke its own late publish.
    let stale_error = match player_container_request_is_eligible(
        &st,
        user.id,
        id,
        participation.id,
        cid,
        ContainerRequestMode::PerTeam,
    )
    .await
    {
        Ok(true) => None,
        Ok(false) => Some(AppError::Forbidden),
        Err(error) => Some(error),
    };
    if let Some(error) = stale_error {
        let cleanup = match c.game_instance_id {
            Some(instance_id) => {
                revoke_published_team_container(
                    &st,
                    &backend_id,
                    container_uuid,
                    instance_id,
                    created_instance_id,
                    created_flag_id,
                )
                .await
            }
            None => {
                tracing::warn!(
                    backend_id = %backend_id,
                    container_id = %container_uuid,
                    "team container publication missing instance owner; using fallback cleanup"
                );
                revoke_failed_team_container_publication(
                    &st,
                    &backend_id,
                    container_uuid,
                    None,
                    created_instance_id,
                    created_flag_id,
                )
                .await
            }
        };
        let unlock = distributed
            .release()
            .await
            .map_err(|unlock_error| AppError::internal(unlock_error.to_string()));
        cleanup?;
        unlock?;
        return Err(error);
    }

    distributed.release().await?;

    // Surface container activity on the monitor `/events` feed. RSCTF emits a
    // ContainerStart GameEvent with Values = [challengeId, challengeTitle]; the team is
    // carried on the event's TeamId/UserId, not the values array (see Monitor Events.tsx).
    let event = game_event::ActiveModel {
        game_id: Set(id),
        event_type: Set(crate::utils::enums::EventType::ContainerStart),
        values: Set(serde_json::json!([cid.to_string(), challenge.title])),
        publish_time_utc: Set(now),
        user_id: Set(Some(user.id)),
        team_id: Set(participation.team_id),
        ..Default::default()
    }
    .insert(&st.db)
    .await;
    if let Err(err) = event {
        tracing::warn!(game = id, challenge = cid, error = %err, "container start event persist failed");
    }

    Ok(RequestResponse::ok(ContainerInfoModel::from(&c)))
}

/// `DELETE /api/game/{id}/container/{challengeId}` — tear down the team's container.
pub async fn delete_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, cid)): Path<(i32, i32)>,
) -> AppResult<StatusCode> {
    let ctx = context_info(&st, &user, id, false).await?;
    let guard_challenge = load_scoped_challenge(&st, id, cid).await?;
    // Shared container is a shared resource — a single player must not tear it down for
    // everyone. Only admins stop it (challenge disable / game end / admin action). Mirrors
    // RSCTF `DeleteContainer` (UsesSharedContainer branch, GameController.cs:2106); pinned
    // to 403 Forbidden here (RSCTF returns 400 at that line). Checked BEFORE the per-team
    // instance lookup, since a shared challenge never has a per-team instance.
    if uses_shared_container(&guard_challenge) {
        return Err(AppError::Coded {
            http: StatusCode::FORBIDDEN,
            code: 403,
            title: "Shared containers can only be stopped by an administrator.".into(),
        });
    }
    // A&D / KotH per-team services are engine-owned, not jeopardy containers — the
    // teardown endpoint refuses them (RSCTF `DeleteContainer`, GameController.cs:2100).
    if guard_challenge.challenge_type.uses_ad_engine()
        && !allows_practice_container(&guard_challenge, &ctx.game)
    {
        return Err(AppError::bad_request(
            "Container creation is not allowed for this challenge",
        ));
    }
    let flight_key = format!("game-container:{}", ctx.participation.id);
    let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &flight_key)
            .await?;
    let instance = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.eq(ctx.participation.id))
        .filter(game_instance::Column::ChallengeId.eq(cid))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("No instance for this challenge"))?;
    let Some(cuuid) = instance.container_id else {
        return Err(AppError::bad_request("No running container"));
    };
    // Per-instance frequency gate (RSCTF `DeleteContainer`, GameController.cs:2113):
    // reject a teardown within the cooldown of this instance's last container operation,
    // AFTER the ContainerNotCreated check and BEFORE actually destroying the container.
    if let Some(err) = container_op_too_frequent(&instance) {
        return Err(err);
    }
    let c = container::Entity::find_by_id(cuuid)
        .one(&st.db)
        .await?
        .ok_or_else(|| {
            AppError::conflict("container bookkeeping is missing; retry after reconciliation")
        })?;
    let destroy_id = format!("<{}> {}", &c.id.simple().to_string()[..12], c.container_id);
    revoke_published_team_container(&st, &c.container_id, c.id, instance.id, None, None).await?;

    let team_name = team::Entity::find_by_id(ctx.participation.team_id)
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .map(|t| t.name)
        .unwrap_or_default();
    let challenge_title = game_challenge::Entity::find_by_id(cid)
        .one(&st.db)
        .await
        .ok()
        .flatten()
        .map(|c| c.title)
        .unwrap_or_default();
    crate::services::audit::info(
        &st,
        "GameController",
        Some(user.name.clone()),
        None,
        format!(
            "{team_name} has destroyed container [{destroy_id}] of challenge {challenge_title}"
        ),
    )
    .await;

    // Mirror RSCTF: emit a ContainerDestroy GameEvent (Values = [challengeId, title]) so
    // the monitor `/events` feed reflects the teardown alongside the ContainerStart.
    game_event::ActiveModel {
        game_id: Set(id),
        event_type: Set(crate::utils::enums::EventType::ContainerDestroy),
        values: Set(serde_json::json!([cid.to_string(), challenge_title])),
        publish_time_utc: Set(Utc::now()),
        user_id: Set(Some(user.id)),
        team_id: Set(ctx.participation.team_id),
        ..Default::default()
    }
    .insert(&st.db)
    .await?;
    distributed.release().await?;

    Ok(StatusCode::OK)
}

/// `POST /api/game/{id}/container/{challengeId}/extend` — extend the lifetime.
pub async fn extend_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, cid)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ContainerInfoModel>> {
    let ctx = context_info(&st, &user, id, true).await?;
    let guard_challenge = load_playable_challenge(&st, id, cid).await?;

    let perm = effective_permission(&st, &ctx.participation, cid).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }

    // Shared container: extend the challenge-owned container's lifetime (keeps it alive
    // while teams are still using it). Mirrors RSCTF `ExtendContainerLifetime`
    // (UsesSharedContainer branch, GameController.cs:2031). Checked BEFORE the per-team
    // instance lookup — a shared challenge has no per-team instance.
    if uses_shared_container(&guard_challenge) {
        let flight_key = format!("shared-container:{}", guard_challenge.id);
        let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
        let distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &flight_key)
                .await?;
        let result = async {
            // The reaper uses the same lock and may have removed or refreshed this
            // pointer while this request waited. Never extend a pre-lock snapshot.
            let current_challenge = game_challenge::Entity::find_by_id(guard_challenge.id)
                .one(&st.db)
                .await?
                .ok_or_else(|| AppError::not_found("Challenge not found"))?;
            let sid = current_challenge
                .shared_container_id
                .ok_or_else(|| AppError::bad_request("No running container"))?;
            let shared = container::Entity::find_by_id(sid)
                .one(&st.db)
                .await?
                .ok_or_else(|| AppError::bad_request("No running container"))?;
            if shared.expect_stop_at - Utc::now()
                > chrono::Duration::minutes(CONTAINER_RENEWAL_WINDOW_MINUTES)
            {
                return Err(AppError::bad_request(
                    "The container is not yet eligible for extension",
                ));
            }
            let stop_at = shared.expect_stop_at + chrono::Duration::hours(CONTAINER_LIFETIME_HOURS);
            let mut am: container::ActiveModel = shared.into();
            am.expect_stop_at = Set(stop_at);
            let shared = am.update(&st.db).await?;
            Ok(RequestResponse::ok(ContainerInfoModel::from(&shared)))
        }
        .await;
        distributed.release().await?;
        return result;
    }

    // A&D / KotH per-team services are engine-owned, not jeopardy containers — the
    // extend endpoint refuses them (RSCTF `ExtendContainerLifetime`,
    // GameController.cs:2025).
    if guard_challenge.challenge_type.uses_ad_engine()
        && !allows_practice_container(&guard_challenge, &ctx.game)
    {
        return Err(AppError::bad_request(
            "Container creation is not allowed for this challenge",
        ));
    }
    let flight_key = format!("game-container:{}", ctx.participation.id);
    let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &flight_key)
            .await?;
    let result = async {
        // Creation, deletion, and the reaper all use this participation lock. Re-read
        // both links after acquisition so an expired pre-lock row is never revived.
        let instance = game_instance::Entity::find()
            .filter(game_instance::Column::ParticipationId.eq(ctx.participation.id))
            .filter(game_instance::Column::ChallengeId.eq(cid))
            .one(&st.db)
            .await?
            .ok_or_else(|| AppError::not_found("No instance for this challenge"))?;
        let cuuid = instance
            .container_id
            .ok_or_else(|| AppError::bad_request("No running container"))?;
        let c = container::Entity::find_by_id(cuuid)
            .one(&st.db)
            .await?
            .ok_or_else(|| AppError::not_found("Container not found"))?;

        // Proximity gate: RSCTF ExtendContainerLifetime only permits renewal once the
        // container is within the RenewalWindow (10 min) of its expiry — otherwise it
        // returns 400 Game_ContainerExtensionNotAvailable.
        if c.expect_stop_at - Utc::now()
            > chrono::Duration::minutes(CONTAINER_RENEWAL_WINDOW_MINUTES)
        {
            return Err(AppError::bad_request(
                "The container is not yet eligible for extension",
            ));
        }

        let stop_at = c.expect_stop_at + chrono::Duration::hours(CONTAINER_LIFETIME_HOURS);
        let mut am: container::ActiveModel = c.into();
        am.expect_stop_at = Set(stop_at);
        let c = am.update(&st.db).await?;
        Ok(RequestResponse::ok(ContainerInfoModel::from(&c)))
    }
    .await;
    distributed.release().await?;
    result
}

/// Port of RSCTF `GameInstanceRepository.GetOrCreateSharedContainer`. The caller must
/// hold `shared-container:{challenge_id}` until the returned endpoint is published or
/// handed to the player. Unlike RSCTF (`Flag = null`, static flag baked into the image),
/// rsctf injects the challenge's static flag as env.
pub(crate) async fn get_or_create_shared_container_locked(
    st: &SharedState,
    challenge: &game_challenge::Model,
) -> AppResult<container::Model> {
    let game_id = challenge.game_id;
    let (challenge, workload, identity, publication_fence) =
        load_shared_definition_snapshot(st, game_id, challenge.id).await?;

    // Reuse the shared container ONLY if its docker container is actually alive — a
    // hill/shared container that died must be recreated, not handed back as a dead
    // endpoint (which read Offline forever).
    if let Some(sid) = challenge.shared_container_id {
        if let Some(existing) = container::Entity::find_by_id(sid).one(&st.db).await? {
            if crate::services::challenge_workloads::existing_runtime_is_reusable(
                st.containers.as_ref(),
                &existing.container_id,
                &existing.image,
                &identity,
            )
            .await?
            {
                let current = load_eligible_shared_challenge(st, challenge.id).await?;
                if current.shared_container_id != Some(sid) {
                    return Err(AppError::bad_request(
                        "Shared container ownership changed during provisioning",
                    ));
                }
                let stop_at = Utc::now() + chrono::Duration::hours(CONTAINER_LIFETIME_HOURS);
                let mut am: container::ActiveModel = existing.into();
                am.expect_stop_at = Set(stop_at);
                let existing = am.update(&st.db).await?;
                return Ok(existing);
            }
            // Dead → revoke the published route, fence capture, then destroy.
            // Retain the exact challenge/container owners if any step fails so a
            // later request can retry instead of publishing over an orphan runtime.
            revoke_published_shared_container(
                st,
                challenge.id,
                existing.id,
                &existing.container_id,
            )
            .await?;
        }
        // Dangling pointer (row reaped / dead): fall through and recreate.
    }

    // A StaticContainer serves the challenge's shared static flag (identical for every
    // team) — the same flag_context the submit path grades against.
    let selected_static_flag = crate::services::challenge_workloads::load_selected_static_flag(
        st.pg(),
        challenge.id,
        challenge.challenge_type,
    )
    .await?;
    let flag = selected_static_flag.clone().unwrap_or_default();

    // KotH hills join the A&D services network (rsctf-ad) so they're reachable over
    // the team VPN *and* by the sandboxed checker via internal IP (the checker's
    // egress firewall only allows the services/VPN CIDRs, so a public published port
    // would be unreachable to it). Plain shared jeopardy (StaticContainer) keep the
    // public published port so teams reach them directly.
    let ad_network = matches!(challenge.challenge_type, ChallengeType::KingOfTheHill)
        .then(crate::services::ad_vpn::services_network);
    let backend_requires_proxy = ad_network.is_none() && st.containers.requires_proxy();
    let container_uuid = uuid::Uuid::new_v4();
    let operation_id = Some(format!("container:{container_uuid}"));
    let info = match workload {
        Some(spec) => {
            st.containers
                .create_workload(spec, operation_id, Some(flag))
                .await?
        }
        None => {
            return Err(AppError::bad_request(
                "shared container launch requires workloadSpec; legacy single-container runtime is no longer supported",
            ));
        }
    };

    let backend_id = info.id.clone();
    let (definition_lock, challenge) = match acquire_shared_publication_lock(
        st,
        game_id,
        challenge.id,
        &publication_fence,
        selected_static_flag.as_deref(),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "stale unpublished shared container destroy failed");
            }
            return Err(error);
        }
    };
    let now = Utc::now();
    let stop_at = now + chrono::Duration::hours(CONTAINER_LIFETIME_HOURS);
    let is_proxy = backend_requires_proxy
        || crate::controllers::admin::container_port_mapping(st).await == "PlatformProxy";
    let persisted: AppResult<container::Model> = async {
        // Publish the bookkeeping row and its challenge owner atomically. The
        // destroy path discovers its lock key through this relationship, so exposing
        // either half first creates a window where an admin/reaper can destroy the
        // backend without taking the shared-container lock.
        let txn = crate::utils::database::begin_seaorm_transaction(&st.db).await?;
        let c = container::ActiveModel {
            id: Set(container_uuid),
            image: Set(identity),
            container_id: Set(info.id),
            status: Set(ContainerStatus::Running),
            started_at: Set(now),
            expect_stop_at: Set(stop_at),
            is_proxy: Set(is_proxy),
            ip: Set(info.ip),
            port: Set(info.port),
            public_ip: Set(None),
            public_port: Set(None),
            // Challenge-owned, not team-owned: no GameInstance link.
            game_instance_id: Set(None),
            exercise_instance_id: Set(None),
            ad_team_service_id: Set(None),
        }
        .insert(&txn)
        .await?;

        // Store the pointer on the challenge so every team's get-or-create reuses this one.
        let cam = game_challenge::ActiveModel {
            id: Set(challenge.id),
            shared_container_id: Set(Some(container_uuid)),
            ..Default::default()
        };
        cam.update(&txn).await?;
        txn.commit().await?;
        Ok(c)
    }
    .await;
    definition_lock.release().await?;

    let c = match persisted {
        Ok(c) => c,
        Err(err) => {
            if let Err(cleanup_error) =
                revoke_published_shared_container(st, challenge.id, container_uuid, &backend_id)
                    .await
            {
                tracing::error!(
                    %backend_id,
                    %cleanup_error,
                    "shared container publication rollback failed; retaining durable owner for retry"
                );
                return Err(AppError::internal(format!(
                    "{err}; shared container rollback failed: {cleanup_error}"
                )));
            }
            return Err(err);
        }
    };

    let stale_error = match load_eligible_shared_challenge(st, challenge.id).await {
        Ok(current) if current.shared_container_id == Some(container_uuid) => None,
        Ok(_) => Some(AppError::bad_request(
            "Shared container ownership changed during publication",
        )),
        Err(error) => Some(error),
    };
    if let Some(error) = stale_error {
        revoke_published_shared_container(st, challenge.id, container_uuid, &backend_id).await?;
        return Err(error);
    }

    Ok(c)
}

#[cfg(test)]
#[path = "containers/reaping_tests.rs"]
mod reaping_tests;
