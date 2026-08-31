//! Per-team dynamic container lifecycle (create/destroy/extend).
use super::*;
use axum::http::HeaderMap;

mod eligibility;
use crate::services::live_roster::LiveParticipationIdentity;
use crate::utils::enums::NetworkMode;
mod deletion;
use deletion::{delete_expected_team_container_locked, DeleteContainerOutcome};
mod extension;
use eligibility::{
    authorize_on_demand_build, ineligible_container_start_error, load_eligible_shared_challenge,
    player_container_request_is_eligible, player_request_is_eligible_now, ContainerRequestMode,
};
use extension::extend_expected_team_container_locked;
mod image_repair;
pub(crate) use image_repair::{prepare_queued_image, repair_missing_legacy_image};
mod publication;
pub(crate) use publication::refresh_shared_container_lease_locked;
use publication::{
    publish_team_container_locked, revoke_published_shared_container,
    revoke_published_team_container, TeamPublication,
};
mod operations;
mod policy;
pub(crate) use operations::sweep as sweep_container_operations;
use policy::{allows_practice_container, container_op_too_frequent};
mod reaping;
pub(crate) use reaping::destroy_managed_container_row;
mod shared;
pub(crate) use shared::get_or_create_shared_container_locked;
mod workload_fence;
use workload_fence::{
    ensure_publication_definition_current, load_playable_definition_snapshot,
    load_shared_definition_snapshot,
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
    headers: HeaderMap,
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
    let requested_operation = operations::operation_request(&headers)?;
    let shared = uses_shared_container(&challenge);
    let operation_scope = if shared {
        format!("shared-challenge:{cid}")
    } else {
        format!("participation:{}", ctx.participation.id)
    };
    let expected_publication_id = if shared {
        challenge.shared_container_id
    } else {
        sqlx::query_scalar::<_, Option<Uuid>>(
            r#"SELECT container_id FROM "GameInstances"
                WHERE participation_id = $1 AND challenge_id = $2"#,
        )
        .bind(ctx.participation.id)
        .bind(cid)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .flatten()
    };
    // This is a pure definition projection. Queued lazy-build challenges bind
    // their immutable fence after the build; already-resolved challenges bind
    // it in the admission row so a concurrent save cannot change the intent.
    let expected_definition_fence =
        crate::services::challenge_workloads::resolve_runtime(&st, &challenge)
            .ok()
            .map(|runtime| runtime.publication_fence);
    let operation = match operations::claim_create(
        st.pg(),
        requested_operation.operation_id,
        &operation_scope,
        user.id,
        id,
        (!shared).then_some(ctx.participation.id),
        cid,
        expected_publication_id,
        expected_definition_fence.as_deref(),
        requested_operation.may_adopt_stale,
    )
    .await?
    {
        operations::ClaimOutcome::Recovered(model) => return Ok(RequestResponse::ok(model)),
        operations::ClaimOutcome::Following => {
            let model =
                operations::wait_for_result(st.pg(), requested_operation.operation_id).await?;
            return Ok(RequestResponse::ok(model));
        }
        operations::ClaimOutcome::Owned(operation) => operation,
    };
    let owner_st = st.clone();
    let owner_user = user.clone();
    let owner_operation = operation.clone();
    let owner = operations::spawn_owner(st.pg().clone(), operation, async move {
        perform_create_container(owner_st, owner_user, id, cid, owner_operation).await
    });
    let model = operations::await_owner(owner).await?;
    Ok(RequestResponse::ok(model))
}

async fn perform_create_container(
    st: SharedState,
    user: CurrentUser,
    id: i32,
    cid: i32,
    operation: operations::ClaimedOperation,
) -> AppResult<ContainerInfoModel> {
    let ctx = context_info(&st, &user, id, true).await?;
    let caller = LiveParticipationIdentity {
        user_id: user.id,
        expected_security_stamp: &user.security_stamp,
        game_id: id,
        team_id: ctx.participation.team_id,
        participation_id: ctx.participation.id,
    };
    let challenge = load_playable_challenge(&st, id, cid).await?;
    let perm = effective_permission(&st, &ctx.participation, cid).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("The challenge was not found"));
    }
    if !challenge.challenge_type.is_container() {
        return Err(AppError::bad_request("Challenge has no container"));
    }
    if challenge.challenge_type.uses_ad_engine()
        && !allows_practice_container(&challenge, &ctx.game)
    {
        return Err(AppError::bad_request(
            "Container creation is not allowed for this challenge",
        ));
    }
    let shared = uses_shared_container(&challenge);
    let on_demand_build = match authorize_on_demand_build(&st, caller, &challenge).await {
        Ok(value) => value,
        Err(error) => return Err(error),
    };
    if on_demand_build {
        image_repair::prepare_queued_image(&st, &challenge).await?;
    }

    // Shared container: one challenge-owned container serves every team. Get-or-create
    // it (idempotent) and hand it back directly — no per-team GameInstance/flag row.
    // Mirrors RSCTF `CreateContainer` (UsesSharedContainer branch, GameController.cs:1953)
    // + `GameInstanceRepository.GetOrCreateSharedContainer`.
    if shared {
        if !player_request_is_eligible_now(&st, caller, cid, ContainerRequestMode::Shared).await? {
            return Err(ineligible_container_start_error(&st, &challenge));
        }
        let c = get_or_create_shared_container_locked(
            &st,
            &challenge,
            ctx.game.vpn_access_required,
            Some(caller),
            operation.operation_id,
            operation.publication_id,
            None,
        )
        .await?;
        // The shared backend remains valid for other teams if this caller lost
        // eligibility during runtime work, but this stale waiter gets no endpoint.
        if !player_request_is_eligible_now(&st, caller, cid, ContainerRequestMode::Shared).await? {
            return Err(AppError::Forbidden);
        }
        return Ok(ContainerInfoModel::from(&c));
    }

    // The durable active-scope row serializes player lifecycle intent. This
    // short authorization transaction is released before every runtime call.
    if !player_request_is_eligible_now(&st, caller, cid, ContainerRequestMode::PerTeam).await? {
        return Err(ineligible_container_start_error(&st, &challenge));
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
    operations::bind_definition(st.pg(), &operation, &publication_fence).await?;
    let container_policy =
        crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;

    // Validate and derive the retry-stable flag before any stale runtime is
    // revoked. A malformed legacy template must be a non-destructive failure.
    let selected_static_flag = crate::services::challenge_workloads::load_selected_static_flag(
        st.pg(),
        cid,
        challenge.challenge_type,
    )
    .await?;
    let flag = if challenge.challenge_type == ChallengeType::DynamicContainer {
        let salt = flag_generator::team_hash_salt(&game.private_key);
        let team_hash = flag_generator::team_challenge_hash(&salt, cid, &participation.token);
        flag_generator::generate_retryable_flag_checked(
            challenge.flag_template.as_deref(),
            &team_hash,
            &operation.operation_id.to_string(),
        )?
    } else {
        selected_static_flag.clone().unwrap_or_default()
    };

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
                    if c.id == operation.publication_id {
                        return Ok(ContainerInfoModel::from(&c));
                    }
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

    let game_kind = crate::services::container::game_kind_for_challenge(challenge.challenge_type);
    let platform_proxy =
        crate::controllers::admin::container_port_mapping(&st).await == "PlatformProxy";
    let is_proxy = crate::services::container::should_use_platform_proxy(
        game_kind,
        st.containers.requires_proxy(),
        platform_proxy,
        game.vpn_access_required,
    );
    let container_uuid = operation.publication_id;
    let operation_id = Some(format!("player-container:{}", operation.operation_id));
    operations::mark_runtime_started(st.pg(), &operation).await?;
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
    operations::record_backend(st.pg(), &operation, &info.id).await?;

    let backend_id = info.id.clone();
    // Reacquire the legacy publication/reaper fence only after runtime I/O has
    // completed. It is retained solely for authorization and database publish.
    let flight_key = format!("game-container:{}", ctx.participation.id);
    let roster_key = crate::services::live_roster::lock_key(ctx.participation.team_id);
    let mut distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning_below_shared(
            st.pg(),
            &[roster_key],
            &flight_key,
        )
        .await?;
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
            distributed.release().await?;
            if let Err(error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, %error, "stale unpublished container destroy failed");
            }
            return Err(AppError::Forbidden);
        }
        Err(error) => {
            let _ = distributed.release().await;
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "unpublished container destroy failed after authorization error");
            }
            return Err(error);
        }
    }

    // Save/rollout and publication share this exact transaction and connection.
    // No second pool checkout or ORM write is allowed while these fences live.
    if let Err(error) = distributed
        .acquire_additional(&crate::services::challenge_workloads::definition_lock_key(
            id, cid,
        ))
        .await
    {
        distributed.rollback().await?;
        if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
            tracing::warn!(%backend_id, error = %destroy_error, "unpublished definition-lock container destroy failed");
        }
        return Err(AppError::internal(error.to_string()));
    }
    if let Err(error) = ensure_publication_definition_current(
        distributed.transaction_mut(),
        id,
        cid,
        &challenge,
        selected_static_flag.as_deref(),
    )
    .await
    {
        distributed.rollback().await?;
        if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
            tracing::warn!(%backend_id, error = %destroy_error, "unpublished stale-definition container destroy failed");
        }
        return Err(error);
    }
    let existing_instance_id = existing.as_ref().map(|instance| instance.id);
    let now = Utc::now();
    let stop_at = now + chrono::Duration::minutes(i64::from(container_policy.default_lifetime));
    let dynamic_flag =
        (challenge.challenge_type == ChallengeType::DynamicContainer).then_some(flag.as_str());
    let c = match publish_team_container_locked(
        distributed.transaction_mut(),
        TeamPublication {
            container_id: container_uuid,
            backend_id: &backend_id,
            image: &identity,
            is_proxy,
            ip: &info.ip,
            port: info.port,
            participation_id: participation.id,
            challenge_id: cid,
            existing_instance_id,
            dynamic_flag,
            started_at: now,
            expect_stop_at: stop_at,
        },
    )
    .await
    {
        Ok(container) => container,
        Err(error) => {
            distributed.rollback().await?;
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "unpublished container destroy failed after database rejection");
            }
            return Err(error);
        }
    };

    // The runtime owner and its recoverable operation receipt become visible
    // in one commit. A crash after this point cannot make an exact retry launch
    // a second backend merely because the HTTP acknowledgement was lost.
    let operation_result = ContainerInfoModel::from(&c);
    if let Err(error) =
        operations::complete_locked(distributed.transaction_mut(), &operation, &operation_result)
            .await
    {
        distributed.rollback().await?;
        if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
            tracing::warn!(%backend_id, error = %destroy_error, "unpublished container destroy failed after receipt rejection");
        }
        return Err(error);
    }

    let still_eligible = match player_container_request_is_eligible(
        distributed.transaction_mut(),
        caller,
        cid,
        ContainerRequestMode::PerTeam,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            distributed.rollback().await?;
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "unpublished container destroy failed after final authorization error");
            }
            return Err(error);
        }
    };
    if !still_eligible {
        distributed.rollback().await?;
        if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
            tracing::warn!(%backend_id, error = %destroy_error, "stale unpublished container destroy failed");
        }
        return Err(AppError::Forbidden);
    }
    if let Err(commit_error) = distributed.release().await {
        let recovered = container::Entity::find_by_id(container_uuid)
            .one(&st.db)
            .await?
            .filter(|published| published.container_id == backend_id);
        let Some(recovered) = recovered else {
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "ambiguous unpublished container destroy failed");
            }
            return Err(AppError::internal(commit_error.to_string()));
        };
        return Ok(ContainerInfoModel::from(&recovered));
    }

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

    Ok(operation_result)
}

/// `DELETE /api/game/{id}/container/{challengeId}` — tear down the team's container.
pub async fn delete_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
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
    let operation_request = operations::operation_request(&headers)?;
    let scope = format!("participation:{}", ctx.participation.id);
    let operation = match operations::claim_delete(
        st.pg(),
        operation_request.operation_id,
        &scope,
        user.id,
        id,
        ctx.participation.id,
        cid,
        query.expected_container_id,
        operation_request.may_adopt_stale,
    )
    .await?
    {
        operations::ClaimOutcome::Recovered(()) => return Ok(StatusCode::OK),
        operations::ClaimOutcome::Following => {
            operations::wait_for_result::<()>(st.pg(), operation_request.operation_id).await?;
            return Ok(StatusCode::OK);
        }
        operations::ClaimOutcome::Owned(operation) => operation,
    };
    let owner_st = st.clone();
    let owner_user = user.clone();
    let owner = operations::spawn_owner(st.pg().clone(), operation, async move {
        perform_delete_container(owner_st, owner_user, id, cid, query.expected_container_id).await
    });
    operations::await_owner(owner).await?;
    Ok(StatusCode::OK)
}

async fn perform_delete_container(
    st: SharedState,
    user: CurrentUser,
    id: i32,
    cid: i32,
    expected_container_id: Uuid,
) -> AppResult<()> {
    let ctx = context_info(&st, &user, id, false).await?;
    let guard_challenge = load_scoped_challenge(&st, id, cid).await?;
    if uses_shared_container(&guard_challenge) {
        return Err(AppError::Coded {
            http: StatusCode::FORBIDDEN,
            code: 403,
            title: "Shared containers can only be stopped by an administrator.".into(),
        });
    }
    if guard_challenge.challenge_type.uses_ad_engine()
        && !allows_practice_container(&guard_challenge, &ctx.game)
    {
        return Err(AppError::bad_request(
            "Container creation is not allowed for this challenge",
        ));
    }
    let mut authorization = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let live = crate::services::live_roster::participation_caller_is_live_on(
        &mut *authorization,
        user.id,
        &user.security_stamp,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        true,
    )
    .await?;
    authorization
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !live {
        return Err(AppError::Forbidden);
    }
    let outcome = delete_expected_team_container_locked(
        &st,
        ctx.participation.id,
        cid,
        expected_container_id,
    )
    .await?;
    let DeleteContainerOutcome::Destroyed {
        audit_id: destroy_id,
    } = outcome
    else {
        return Ok(());
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
    if let Err(error) = crate::services::game_event_feed::persist_and_publish(
        &st,
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
    {
        tracing::warn!(%error, "container destroy event persist/publish failed");
    }

    Ok(())
}

/// `POST /api/game/{id}/container/{challengeId}/extend` — extend the lifetime.
pub async fn extend_container(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path((id, cid)): Path<(i32, i32)>,
    Query(query): Query<ExpectedContainerQuery>,
) -> AppResult<RequestResponse<ContainerInfoModel>> {
    let ctx = context_info(&st, &user, id, true).await?;
    let guard_challenge = load_playable_challenge(&st, id, cid).await?;
    let perm = effective_permission(&st, &ctx.participation, cid).await?;
    if !perm.contains(GamePermission::VIEW_CHALLENGE) {
        return Err(AppError::not_found("Challenge not found"));
    }
    let shared = uses_shared_container(&guard_challenge);
    if !shared
        && guard_challenge.challenge_type.uses_ad_engine()
        && !allows_practice_container(&guard_challenge, &ctx.game)
    {
        return Err(AppError::bad_request(
            "Container creation is not allowed for this challenge",
        ));
    }
    let operation_request = operations::operation_request(&headers)?;
    let scope = if shared {
        format!("shared-challenge:{cid}")
    } else {
        format!("participation:{}", ctx.participation.id)
    };
    let operation = match operations::claim_extend(
        st.pg(),
        operation_request.operation_id,
        &scope,
        user.id,
        id,
        (!shared).then_some(ctx.participation.id),
        cid,
        query.expected_container_id,
        operation_request.may_adopt_stale,
    )
    .await?
    {
        operations::ClaimOutcome::Recovered(model) => return Ok(RequestResponse::ok(model)),
        operations::ClaimOutcome::Following => {
            let model =
                operations::wait_for_result(st.pg(), operation_request.operation_id).await?;
            return Ok(RequestResponse::ok(model));
        }
        operations::ClaimOutcome::Owned(operation) => operation,
    };
    let owner_st = st.clone();
    let owner_user = user.clone();
    let owner_operation = operation.clone();
    let owner = operations::spawn_owner(st.pg().clone(), operation, async move {
        perform_extend_container(
            owner_st,
            owner_user,
            id,
            cid,
            query.expected_container_id,
            owner_operation,
        )
        .await
    });
    let model = operations::await_owner(owner).await?;
    Ok(RequestResponse::ok(model))
}

async fn perform_extend_container(
    st: SharedState,
    user: CurrentUser,
    id: i32,
    cid: i32,
    expected_container_id: Uuid,
    operation: operations::ClaimedOperation,
) -> AppResult<ContainerInfoModel> {
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
            // The reaper uses the same lock and may have replaced this pointer
            // while this request waited. Read and update on the lock owner.
            let sid = sqlx::query_scalar::<_, Option<Uuid>>(
                r#"SELECT shared_container_id FROM "GameChallenges" WHERE id = $1"#,
            )
            .bind(guard_challenge.id)
            .fetch_optional(&mut **distributed.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::not_found("Challenge not found"))?
            .ok_or_else(|| AppError::bad_request("No running container"))?;
            if sid != expected_container_id {
                return Err(AppError::conflict(
                    "The challenge instance changed; refresh and retry.",
                ));
            }
            let shared = sqlx::query_as::<
                _,
                (
                    DateTime<Utc>,
                    DateTime<Utc>,
                    i16,
                    bool,
                    String,
                    i32,
                    Option<String>,
                    Option<i32>,
                ),
            >(
                r#"SELECT started_at, expect_stop_at, status, is_proxy, ip, port,
                          public_ip, public_port
                     FROM "Containers" WHERE id = $1"#,
            )
            .bind(sid)
            .fetch_optional(&mut **distributed.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .ok_or_else(|| AppError::bad_request("No running container"))?;
            if shared.1 - Utc::now()
                > chrono::Duration::minutes(i64::from(container_policy.renewal_window))
            {
                return Err(AppError::bad_request(
                    "The container is not yet eligible for extension",
                ));
            }
            let stop_at = shared.1
                + chrono::Duration::minutes(i64::from(container_policy.extension_duration));
            let stop_at: DateTime<Utc> = sqlx::query_scalar(
                r#"UPDATE "Containers" SET expect_stop_at = $2
                    WHERE id = $1 RETURNING expect_stop_at"#,
            )
            .bind(sid)
            .bind(stop_at)
            .fetch_one(&mut **distributed.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            let status = match shared.2 {
                value if value == ContainerStatus::Pending as i16 => ContainerStatus::Pending,
                value if value == ContainerStatus::Running as i16 => ContainerStatus::Running,
                value if value == ContainerStatus::Destroyed as i16 => ContainerStatus::Destroyed,
                value => {
                    return Err(AppError::internal(format!(
                        "invalid container status {value}"
                    )))
                }
            };
            let entry = if shared.3 {
                sid.to_string()
            } else {
                format!(
                    "{}:{}",
                    shared.6.as_deref().unwrap_or(&shared.4),
                    shared.7.unwrap_or(shared.5)
                )
            };
            Ok(ContainerInfoModel {
                id: sid.to_string(),
                status,
                started_at: shared.0,
                expect_stop_at: stop_at,
                entry,
            })
        }
        .await;
        return match result {
            Ok(response) => {
                operations::complete_locked(distributed.transaction_mut(), &operation, &response)
                    .await?;
                distributed.release().await?;
                Ok(response)
            }
            Err(error) => {
                distributed.rollback().await?;
                Err(error)
            }
        };
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
            distributed.transaction_mut(),
            ctx.participation.id,
            cid,
            expected_container_id,
            &container_policy,
        )
        .await?;
        Ok(response)
    }
    .await;
    match result {
        Ok(response) => {
            operations::complete_locked(distributed.transaction_mut(), &operation, &response)
                .await?;
            distributed.release().await?;
            Ok(response)
        }
        Err(error) => {
            distributed.rollback().await?;
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "containers/operation_header_tests.rs"]
mod operation_header_tests;

#[cfg(test)]
#[path = "containers/delete_tests.rs"]
mod delete_tests;
#[cfg(test)]
#[path = "containers/reaping_tests.rs"]
mod reaping_tests;
