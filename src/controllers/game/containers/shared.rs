use super::*;

/// Port of RSCTF `GameInstanceRepository.GetOrCreateSharedContainer`. The caller must
/// hold `shared-container:{challenge_id}` until the returned endpoint is published or
/// handed to the player. Unlike RSCTF (`Flag = null`, static flag baked into the image),
/// rsctf injects the challenge's static flag as env.
pub(crate) async fn get_or_create_shared_container_locked(
    st: &SharedState,
    challenge: &game_challenge::Model,
    vpn_access_required: bool,
    caller: Option<LiveParticipationIdentity<'_>>,
    lifecycle_operation_id: Uuid,
    publication_id: Uuid,
    runtime_operation_id: Option<String>,
) -> AppResult<container::Model> {
    let container_policy =
        crate::services::container_policy::ContainerPolicy::load(st.pg()).await?;
    let game_id = challenge.game_id;
    let (challenge, workload, identity, _publication_fence, legacy_image) =
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
    let container_uuid = publication_id;
    let operation_id =
        runtime_operation_id.or_else(|| Some(format!("player-container:{lifecycle_operation_id}")));
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
                    control_plane_callback_ports: Vec::new(),
                    network_mode: challenge.network_mode.unwrap_or(NetworkMode::Open),
                    operation_id,
                })
                .await?
        }
    };

    let backend_id = info.id.clone();
    let now = Utc::now();
    let stop_at = now + chrono::Duration::minutes(i64::from(container_policy.default_lifetime));
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let publication: AppResult<container::Model> = async {
        if let Some(caller) = caller {
            crate::utils::single_flight::acquire_transaction_advisory_lock_shared(
                &mut transaction,
                &crate::services::live_roster::lock_key(caller.team_id),
            )
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
        crate::utils::single_flight::acquire_transaction_advisory_lock(
            &mut transaction,
            &crate::services::challenge_workloads::definition_lock_key(game_id, challenge.id),
        )
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        ensure_publication_definition_current(
            &mut transaction,
            game_id,
            challenge.id,
            &challenge,
            selected_static_flag.as_deref(),
        )
        .await?;
        if let Some(caller) = caller {
            if !player_container_request_is_eligible(
                &mut transaction,
                caller,
                challenge.id,
                ContainerRequestMode::Shared,
            )
            .await?
            {
                return Err(AppError::Forbidden);
            }
        }
        sqlx::query(
            r#"INSERT INTO "Containers"
                   (id, image, container_id, status, started_at, expect_stop_at,
                    is_proxy, ip, port, public_ip, public_port,
                    game_instance_id, exercise_instance_id, ad_team_service_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                       NULL, NULL, NULL, NULL, NULL)"#,
        )
        .bind(container_uuid)
        .bind(&identity)
        .bind(&backend_id)
        .bind(ContainerStatus::Running as i16)
        .bind(now)
        .bind(stop_at)
        .bind(is_proxy)
        .bind(&info.ip)
        .bind(info.port)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let linked = sqlx::query(
            r#"UPDATE "GameChallenges"
                  SET shared_container_id = $2
                WHERE id = $1 AND shared_container_id IS NULL"#,
        )
        .bind(challenge.id)
        .bind(container_uuid)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if linked.rows_affected() != 1 {
            return Err(AppError::conflict(
                "Shared container ownership changed during publication",
            ));
        }
        Ok(container::Model {
            id: container_uuid,
            image: identity.clone(),
            container_id: backend_id.clone(),
            status: ContainerStatus::Running,
            started_at: now,
            expect_stop_at: stop_at,
            is_proxy,
            ip: info.ip.clone(),
            port: info.port,
            public_ip: None,
            public_port: None,
            game_instance_id: None,
            exercise_instance_id: None,
            ad_team_service_id: None,
        })
    }
    .await;
    let c = match publication {
        Ok(c) => c,
        Err(error) => {
            transaction.rollback().await.map_err(|rollback_error| {
                AppError::internal(format!("{error}; rollback failed: {rollback_error}"))
            })?;
            if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                tracing::warn!(%backend_id, error = %destroy_error, "unpublished shared container destroy failed");
            }
            return Err(error);
        }
    };
    if let Err(commit_error) = transaction.commit().await {
        let recovered = container::Entity::find_by_id(container_uuid)
            .one(&st.db)
            .await?
            .filter(|published| published.container_id == backend_id);
        if let Some(recovered) = recovered {
            return Ok(recovered);
        }
        if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
            tracing::warn!(%backend_id, error = %destroy_error, "ambiguous shared container destroy failed");
        }
        return Err(AppError::internal(commit_error.to_string()));
    }
    Ok(c)
}
