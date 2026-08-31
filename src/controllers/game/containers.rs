//! Per-team dynamic container lifecycle (create/destroy/extend).
use super::*;

mod eligibility;
use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::enums::NetworkMode;
mod deletion;
use deletion::{delete_expected_team_container_locked, DeleteContainerOutcome};
mod extension;
use eligibility::{
    authorize_on_demand_build, ineligible_container_start_error, load_eligible_shared_challenge,
    player_container_request_is_eligible, ContainerRequestMode,
};
use extension::extend_expected_team_container_locked;
mod image_repair;
pub(crate) use image_repair::{prepare_queued_image, repair_missing_legacy_image};
mod publication;
pub(crate) use publication::refresh_shared_container_lease_locked;
use publication::{
    revoke_failed_team_container_publication, revoke_published_shared_container,
    revoke_published_team_container,
};
mod policy;
use policy::{allows_practice_container, container_op_too_frequent};
mod reaping;
pub(crate) use reaping::destroy_managed_container_row;
mod shared;
pub(crate) use shared::get_or_create_shared_container_locked;
mod workload_fence;
use workload_fence::{
    acquire_playable_publication_lock, acquire_shared_publication_lock,
    load_playable_definition_snapshot, load_shared_definition_snapshot,
};

/// Immutable identity precondition for a player container lifecycle mutation.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpectedContainerQuery {
    pub expected_container_id: Uuid,
}

/// `POST /api/game/{id}/container/{challengeId}` — provision a per-team dynamic
/// container (mirrors RSCTF `GameInstanceRepository.CreateContainer`).
pub async fn create_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, cid)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<ContainerInfoModel>> {
    let ctx = context_info(&st, &user, id, true).await?;
    let caller = LiveParticipationIdentity {
        user_id: user.id,
        expected_security_stamp: &user.security_stamp,
        game_id: id,
        team_id: ctx.participation.team_id,
        participation_id: ctx.participation.id,
    };

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
    if authorize_on_demand_build(&st, caller, &challenge).await? {
        image_repair::prepare_queued_image(&st, &challenge).await?;
    }

    // Shared container: one challenge-owned container serves every team. Get-or-create
    // it (idempotent) and hand it back directly — no per-team GameInstance/flag row.
    // Mirrors RSCTF `CreateContainer` (UsesSharedContainer branch, GameController.cs:1953)
    // + `GameInstanceRepository.GetOrCreateSharedContainer`.
    if uses_shared_container(&challenge) {
        let flight_key = format!("shared-container:{}", challenge.id);
        let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
        let roster_key = crate::services::live_roster::lock_key(ctx.participation.team_id);
        let mut distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning_below_shared(
                st.pg(),
                &[roster_key],
                &flight_key,
            )
            .await?;
        let result = async {
            if !player_container_request_is_eligible(
                distributed.transaction_mut(),
                caller,
                cid,
                ContainerRequestMode::Shared,
            )
            .await?
            {
                return Err(ineligible_container_start_error(&st, &challenge));
            }
            let c = get_or_create_shared_container_locked(
                &st,
                &challenge,
                ctx.game.vpn_access_required,
                None,
            )
            .await?;
            // The shared backend remains a valid challenge-level resource when only
            // this caller loses eligibility, but the stale request must not receive
            // its endpoint after the potentially slow backend operation.
            if !player_container_request_is_eligible(
                distributed.transaction_mut(),
                caller,
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
    let roster_key = crate::services::live_roster::lock_key(ctx.participation.team_id);
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning_below_shared(
            st.pg(),
            &[roster_key],
            &flight_key,
        )
        .await?;

    if !player_container_request_is_eligible(
        distributed.transaction_mut(),
        caller,
        cid,
        ContainerRequestMode::PerTeam,
    )
    .await?
    {
        let error = ineligible_container_start_error(&st, &challenge);
        distributed.release().await?;
        return Err(error);
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
    let (challenge, workload, identity, publication_fence, legacy_image) =
        load_playable_definition_snapshot(&st, id, cid).await?;
    let container_policy =
        crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;

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
                        legacy_image.is_some(),
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
        flag_generator::generate_flag_checked(challenge.flag_template.as_deref(), &team_hash)?
    } else {
        selected_static_flag.clone().unwrap_or_default()
    };
    let game_kind = crate::services::container::game_kind_for_challenge(challenge.challenge_type);
    let platform_proxy =
        crate::controllers::admin::container_port_mapping(&st).await == "PlatformProxy";
    let is_proxy = crate::services::container::should_use_platform_proxy(
        game_kind,
        st.containers.requires_proxy(),
        platform_proxy,
        game.vpn_access_required,
    );
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
                .create_workload(spec, operation_id, Some(flag.clone()), is_proxy)
                .await?
        }
        None => {
            st.containers
                .create(ContainerSpec {
                    game_kind,
                    image: legacy_image
                        .clone()
                        .expect("a legacy definition has an immutable launch image"),
                    memory_limit: challenge.memory_limit.unwrap_or(64),
                    cpu_count: challenge.cpu_count.unwrap_or(1),
                    storage_limit: crate::services::container::storage_limit_or_default(
                        challenge.storage_limit,
                    ),
                    expose_port: challenge.expose_port.unwrap_or(80),
                    publish_port: true,
                    proxy_only: is_proxy,
                    env: vec![("RSCTF_TEAM_ID".into(), participation.team_id.to_string())],
                    flag: Some(flag.clone()),
                    ad_network: None,
                    allow_egress: challenge.network_mode.unwrap_or(NetworkMode::Open)
                        == NetworkMode::Open,
                    control_plane_callback_ports: Vec::new(),
                    network_mode: challenge.network_mode.unwrap_or(NetworkMode::Open),
                    operation_id,
                })
                .await?
        }
    };

    let backend_id = info.id.clone();
    match player_container_request_is_eligible(
        distributed.transaction_mut(),
        caller,
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
        let stop_at = now + chrono::Duration::minutes(i64::from(container_policy.default_lifetime));

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
        distributed.transaction_mut(),
        caller,
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
    let values = serde_json::json!([cid.to_string(), challenge.title]);
    if let Err(err) = crate::services::game_event_feed::persist_and_publish(
        &st,
        crate::services::game_event_feed::NewGameEvent {
            game_id: id,
            event_type: crate::utils::enums::EventType::ContainerStart,
            values: &values,
            publish_time: now,
            user_id: Some(user.id),
            team_id: participation.team_id,
        },
    )
    .await
    {
        tracing::warn!(game = id, challenge = cid, error = %err, "container start event persist failed");
    }

    Ok(RequestResponse::ok(ContainerInfoModel::from(&c)))
}

/// `DELETE /api/game/{id}/container/{challengeId}` — tear down the team's container.
pub async fn delete_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, cid)): Path<(i32, i32)>,
    Query(query): Query<ExpectedContainerQuery>,
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
    let roster_key = crate::services::live_roster::lock_key(ctx.participation.team_id);
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning_below_shared(
            st.pg(),
            &[roster_key],
            &flight_key,
        )
        .await?;
    if !crate::services::live_roster::participation_caller_is_live_on(
        &mut **distributed.transaction_mut(),
        user.id,
        &user.security_stamp,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        true,
    )
    .await?
    {
        distributed.release().await?;
        return Err(AppError::Forbidden);
    }
    let outcome = delete_expected_team_container_locked(
        &st,
        ctx.participation.id,
        cid,
        query.expected_container_id,
    )
    .await?;
    let DeleteContainerOutcome::Destroyed {
        audit_id: destroy_id,
    } = outcome
    else {
        distributed.release().await?;
        return Ok(StatusCode::OK);
    };

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
    let values = serde_json::json!([cid.to_string(), challenge_title]);
    let event_id = crate::services::game_event_feed::insert_on(
        distributed.transaction_mut(),
        crate::services::game_event_feed::NewGameEvent {
            game_id: id,
            event_type: crate::utils::enums::EventType::ContainerDestroy,
            values: &values,
            publish_time: Utc::now(),
            user_id: Some(user.id),
            team_id: ctx.participation.team_id,
        },
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    distributed.release().await?;
    if let Err(error) = crate::services::game_event_feed::publish_committed(&st, &[event_id]).await
    {
        tracing::warn!(event_id, %error, "container destroy event publish failed");
    }

    Ok(StatusCode::OK)
}

/// `POST /api/game/{id}/container/{challengeId}/extend` — extend the lifetime.
pub async fn extend_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, cid)): Path<(i32, i32)>,
    Query(query): Query<ExpectedContainerQuery>,
) -> AppResult<RequestResponse<ContainerInfoModel>> {
    let ctx = context_info(&st, &user, id, true).await?;
    let caller = LiveParticipationIdentity {
        user_id: user.id,
        expected_security_stamp: &user.security_stamp,
        game_id: id,
        team_id: ctx.participation.team_id,
        participation_id: ctx.participation.id,
    };
    let guard_challenge = load_playable_challenge(&st, id, cid).await?;
    let container_policy =
        crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;

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
        let roster_key = crate::services::live_roster::lock_key(ctx.participation.team_id);
        let mut distributed =
            crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning_below_shared(
                st.pg(),
                &[roster_key],
                &flight_key,
            )
            .await?;
        let result = async {
            if !player_container_request_is_eligible(
                distributed.transaction_mut(),
                caller,
                cid,
                ContainerRequestMode::Shared,
            )
            .await?
            {
                return Err(AppError::Forbidden);
            }
            // The reaper uses the same lock and may have removed or refreshed this
            // pointer while this request waited. Never extend a pre-lock snapshot.
            let current_challenge = game_challenge::Entity::find_by_id(guard_challenge.id)
                .one(&st.db)
                .await?
                .ok_or_else(|| AppError::not_found("Challenge not found"))?;
            let sid = current_challenge
                .shared_container_id
                .ok_or_else(|| AppError::bad_request("No running container"))?;
            if sid != query.expected_container_id {
                return Err(AppError::conflict(
                    "The challenge instance changed; refresh and retry.",
                ));
            }
            let shared = container::Entity::find_by_id(sid)
                .one(&st.db)
                .await?
                .ok_or_else(|| AppError::bad_request("No running container"))?;
            if shared.expect_stop_at - Utc::now()
                > chrono::Duration::minutes(i64::from(container_policy.renewal_window))
            {
                return Err(AppError::bad_request(
                    "The container is not yet eligible for extension",
                ));
            }
            let stop_at = shared.expect_stop_at
                + chrono::Duration::minutes(i64::from(container_policy.extension_duration));
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
    let roster_key = crate::services::live_roster::lock_key(ctx.participation.team_id);
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning_below_shared(
            st.pg(),
            &[roster_key],
            &flight_key,
        )
        .await?;
    let result = async {
        if !player_container_request_is_eligible(
            distributed.transaction_mut(),
            caller,
            cid,
            ContainerRequestMode::PerTeam,
        )
        .await?
        {
            return Err(AppError::Forbidden);
        }
        // Creation, deletion, and the reaper all use this participation lock. The
        // helper re-reads and checks the immutable link after acquisition, so a
        // delayed request for runtime A can never extend replacement B.
        let response = extend_expected_team_container_locked(
            &st,
            ctx.participation.id,
            cid,
            query.expected_container_id,
            &container_policy,
        )
        .await?;
        Ok(RequestResponse::ok(response))
    }
    .await;
    distributed.release().await?;
    result
}

#[cfg(test)]
#[path = "containers/delete_tests.rs"]
mod delete_tests;
#[cfg(test)]
#[path = "containers/reaping_tests.rs"]
mod reaping_tests;
