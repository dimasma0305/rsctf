use super::*;

/// Port of RSCTF `GameInstanceRepository.GetOrCreateSharedContainer`. The caller must
/// hold `shared-container:{challenge_id}` until the returned endpoint is published or
/// handed to the player. Unlike RSCTF (`Flag = null`, static flag baked into the image),
/// rsctf injects the challenge's static flag as env.
pub(crate) async fn get_or_create_shared_container_locked(
    st: &SharedState,
    challenge: &game_challenge::Model,
    vpn_access_required: bool,
) -> AppResult<container::Model> {
    let container_policy =
        crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;
    let game_id = challenge.game_id;
    let (challenge, workload, identity, publication_fence, legacy_image) =
        load_shared_definition_snapshot(st, game_id, challenge.id).await?;

    if let Some(sid) = challenge.shared_container_id {
        if let Some(existing) = container::Entity::find_by_id(sid).one(&st.db).await? {
            if crate::services::challenge_workloads::existing_runtime_is_reusable(
                st.containers.as_ref(),
                &existing.container_id,
                &existing.image,
                &identity,
                legacy_image.is_some(),
            )
            .await?
            {
                let current = load_eligible_shared_challenge(st, challenge.id).await?;
                if current.shared_container_id != Some(sid) {
                    return Err(AppError::bad_request(
                        "Shared container ownership changed during provisioning",
                    ));
                }
                let stop_at = Utc::now()
                    + chrono::Duration::minutes(i64::from(container_policy.default_lifetime));
                let mut am: container::ActiveModel = existing.into();
                am.expect_stop_at = Set(stop_at);
                return Ok(am.update(&st.db).await?);
            }
            revoke_published_shared_container(
                st,
                challenge.id,
                existing.id,
                &existing.container_id,
            )
            .await?;
        }
    }

    let selected_static_flag = crate::services::challenge_workloads::load_selected_static_flag(
        st.pg(),
        challenge.id,
        challenge.challenge_type,
    )
    .await?;
    let flag = selected_static_flag.clone().unwrap_or_default();
    let ad_network = matches!(challenge.challenge_type, ChallengeType::KingOfTheHill)
        .then(crate::services::ad_vpn::services_network);
    let game_kind = crate::services::container::game_kind_for_challenge(challenge.challenge_type);
    let platform_proxy =
        crate::controllers::admin::container_port_mapping(st).await == "PlatformProxy";
    let is_proxy = crate::services::container::should_use_platform_proxy(
        game_kind,
        st.containers.requires_proxy(),
        platform_proxy,
        vpn_access_required,
    );
    let container_uuid = uuid::Uuid::new_v4();
    let operation_id = Some(format!("container:{container_uuid}"));
    let info = match workload {
        Some(spec) => {
            st.containers
                .create_workload(spec, operation_id, Some(flag), is_proxy)
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
                    env: Vec::new(),
                    flag: Some(flag),
                    ad_network,
                    allow_egress: challenge.ad_allow_egress,
                    network_mode: challenge.network_mode.unwrap_or(NetworkMode::Open),
                    operation_id,
                })
                .await?
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
    let stop_at = now + chrono::Duration::minutes(i64::from(container_policy.default_lifetime));
    let persisted: AppResult<container::Model> = async {
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
            game_instance_id: Set(None),
            exercise_instance_id: Set(None),
            ad_team_service_id: Set(None),
        }
        .insert(&txn)
        .await?;
        game_challenge::ActiveModel {
            id: Set(challenge.id),
            shared_container_id: Set(Some(container_uuid)),
            ..Default::default()
        }
        .update(&txn)
        .await?;
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
                tracing::error!(%backend_id, %cleanup_error, "shared container publication rollback failed; retaining durable owner for retry");
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
