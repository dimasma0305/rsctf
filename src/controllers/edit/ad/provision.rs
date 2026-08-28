//! A&D container ensure/provision (EnsureContainers, EnsureInstances, on-accept
//! provisioning) — split from edit/ad/mod.rs to stay under the 1000-line rule.
use super::super::*;
use axum::http::HeaderMap;
use uuid::Uuid;

const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";

fn reconcile_operation_id(headers: &HeaderMap) -> AppResult<Uuid> {
    let Some(raw) = headers.get(IDEMPOTENCY_KEY_HEADER) else {
        // Keep cached pre-idempotency clients and existing operator automation
        // working during a rolling deployment. Such a request gets protection
        // within this attempt, but only a client-supplied key can span retries.
        return Ok(Uuid::new_v4());
    };
    let raw = raw
        .to_str()
        .map_err(|_| AppError::bad_request("Idempotency-Key must be an ASCII UUID"))?;
    Uuid::parse_str(raw).map_err(|_| AppError::bad_request("Idempotency-Key must be a UUID"))
}

fn ad_service_operation_id(
    reconcile_operation_id: Option<Uuid>,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
) -> Option<String> {
    reconcile_operation_id.map(|operation_id| {
        format!("ad-ensure:{operation_id}:{game_id}:{participation_id}:{challenge_id}")
    })
}

fn is_manual_operation_conflict(error: &AppError, operation_id: Option<Uuid>) -> bool {
    operation_id.is_some() && matches!(error, AppError::Conflict(_))
}

fn should_provision_vpn(
    vpn_enabled: bool,
    game_active: bool,
    has_engine_challenge: bool,
    ensure_vpn: bool,
) -> bool {
    vpn_enabled && game_active && has_engine_challenge && ensure_vpn
}

fn should_reconcile_vpn(need_vpn: bool, has_managed_challenges: bool) -> bool {
    need_vpn || has_managed_challenges
}

async fn current_ad_pair(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
    challenge_id: i32,
    self_hosted: bool,
) -> AppResult<Option<(participation::Model, game_challenge::Model)>> {
    let game_exists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
             SELECT 1 FROM "Games"
              WHERE id = $1
                AND deletion_pending = FALSE
                AND end_time_utc >= clock_timestamp()
           )"#,
    )
    .bind(game_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !game_exists {
        return Ok(None);
    }
    let participation = participation::Entity::find()
        .filter(participation::Column::Id.eq(participation_id))
        .filter(participation::Column::GameId.eq(game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .one(&st.db)
        .await?;
    let Some(participation) = participation else {
        return Ok(None);
    };
    let team_is_live = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
             SELECT 1 FROM "Teams"
              WHERE id = $1 AND deletion_pending = FALSE
           )"#,
    )
    .bind(participation.team_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !team_is_live {
        return Ok(None);
    }
    let challenge = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .one(&st.db)
        .await?;
    let Some(challenge) = challenge.filter(|challenge| challenge.ad_self_hosted == self_hosted)
    else {
        return Ok(None);
    };
    let challenge_is_live = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
             SELECT 1 FROM "GameChallenges"
              WHERE id = $1 AND game_id = $2 AND deletion_pending = FALSE
           )"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !challenge_is_live {
        return Ok(None);
    }
    if !self_hosted && crate::services::challenge_images::runtime_image(st, &challenge).is_err() {
        return Ok(None);
    }
    Ok(Some((participation, challenge)))
}

async fn deactivate_stale_pair(
    st: &SharedState,
    participation_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let service = ad_team_service::Entity::find()
        .filter(ad_team_service::Column::ParticipationId.eq(participation_id))
        .filter(ad_team_service::Column::ChallengeId.eq(challenge_id))
        .one(&st.db)
        .await?;
    let Some(service) = service else {
        return Ok(());
    };
    let backend_id = service.container_id.clone();
    crate::services::ad_vpn::deactivate_team_service(&st.db, service.id).await?;
    if let Some(backend_id) = backend_id {
        crate::services::traffic::destroy_container_after_capture_fence(st, &backend_id).await?;
    }
    Ok(())
}

/// `POST /api/edit/games/{id}/ad/EnsureContainers` -> void.
///
/// Launch the platform-hosted A&D service container for every (accepted team,
/// platform-hosted A&D challenge) that doesn't already have one, register its
/// host:port, and plant the team's flag. Idempotent: services that already have
/// a `container_id` are skipped. Thin wrapper over [`ensure_ad_containers`]
/// (whole game, every accepted team).
pub async fn ad_ensure_containers(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    headers: HeaderMap,
) -> AppResult<(
    axum::http::StatusCode,
    RequestResponse<crate::services::control_jobs::ControlJobModel>,
)> {
    manager_or_admin(&st, &user, game_id).await?;
    let operation = reconcile_operation_id(&headers)?;
    let input = serde_json::json!({ "ensureVpn": true, "ensureKoth": true });
    let fingerprint = super::super::control_jobs::fingerprint(&input)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::AdReconcile,
        &format!("game:{game_id}"),
        game_id,
        None,
        operation,
        &fingerprint,
        input,
    )
    .await?;
    crate::services::control_jobs::merge_reconcile_input(
        st.pg(),
        job.id,
        serde_json::json!({ "ensureVpn": true, "ensureKoth": true }),
    )
    .await?;
    let job = crate::services::control_jobs::get(st.pg(), job.id)
        .await?
        .ok_or_else(|| AppError::internal("queued reconcile job disappeared"))?;
    crate::services::control_jobs::kick(st);
    Ok((axum::http::StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

pub(crate) async fn run_ad_reconcile_job(
    st: &SharedState,
    game_id: i32,
    ensure_vpn: bool,
    ensure_koth: bool,
    operation_id: Uuid,
) -> AppResult<(i32, i32)> {
    let game = load_game(st, game_id).await?;
    const PARTICIPATION_PAGE_SIZE: i64 = 64;
    let mut cursor = 0;
    let mut launched = 0i32;
    let mut failures = 0i32;
    loop {
        let participation_ids = sqlx::query_scalar::<_, i32>(
            r#"SELECT id FROM "Participations"
                WHERE game_id = $1 AND status = $2 AND id > $3
                ORDER BY id LIMIT $4"#,
        )
        .bind(game_id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(cursor)
        .bind(PARTICIPATION_PAGE_SIZE)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if participation_ids.is_empty() {
            break;
        }
        for participation_id in participation_ids {
            cursor = participation_id;
            let (page_launched, page_failures) = ensure_ad_containers(
                st,
                &game,
                Some(participation_id),
                ensure_vpn,
                false,
                false,
                Some(operation_id),
            )
            .await?;
            launched = launched.saturating_add(page_launched);
            failures = failures.saturating_add(page_failures);
        }
    }
    let has_managed_ad = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS (
             SELECT 1 FROM "GameChallenges"
              WHERE game_id = $1 AND is_enabled = TRUE
                AND review_status = $2 AND "Type" = $3
                AND ad_self_hosted = FALSE
           )"#,
    )
    .bind(game_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ChallengeType::AttackDefense as i16)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if ensure_vpn || has_managed_ad {
        crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
    }
    if ensure_koth {
        crate::controllers::game::koth::ensure_koth_hills_with_operation(
            st,
            game.id,
            Some(operation_id),
        )
        .await?;
    }
    Ok((launched, failures))
}

pub(crate) async fn request_ad_reconcile_job(
    st: &SharedState,
    game_id: i32,
    ensure_vpn: bool,
    ensure_koth: bool,
) -> AppResult<crate::services::control_jobs::ControlJobModel> {
    let input = serde_json::json!({ "ensureVpn": ensure_vpn, "ensureKoth": ensure_koth });
    let fingerprint = super::super::control_jobs::fingerprint(&input)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::AdReconcile,
        &format!("game:{game_id}"),
        game_id,
        None,
        uuid::Uuid::new_v4(),
        &fingerprint,
        input,
    )
    .await?;
    crate::services::control_jobs::merge_reconcile_input(
        st.pg(),
        job.id,
        serde_json::json!({ "ensureVpn": ensure_vpn, "ensureKoth": ensure_koth }),
    )
    .await?;
    let job = crate::services::control_jobs::get(st.pg(), job.id)
        .await?
        .ok_or_else(|| AppError::internal("queued reconcile job disappeared"))?;
    crate::services::control_jobs::kick(st.clone());
    Ok(job)
}

/// Reusable core of [`ad_ensure_containers`]: launch the platform-hosted A&D
/// service container for every (accepted team, self-hosted A&D challenge) that
/// doesn't already have a live one. Mirrors `ad_ensure_containers`'s original
/// selection exactly — `AttackDefense` challenges that are not `ad_self_hosted`
/// and have a successfully pinned runtime image. Returns `(launched, failures)`.
///
/// `only_participation` narrows the accepted-team set to a single participation
/// (used by the participation-accept path to bring up just the newly-accepted
/// team's boxes); `None` provisions every accepted team (the manual endpoint).
///
/// Best-effort (accept path only): when the container runtime is unreachable and
/// we're scoped to one participation, a placeholder `ad_team_service` row is
/// registered (no `container_id`, `Offline` status) so the team still appears in
/// the A&D grid — a later `EnsureContainers` pass fills in the live host:port.
/// The manual endpoint (`only_participation == None`) keeps its historical
/// skip-on-failure behavior and registers no row.
pub(crate) async fn ensure_ad_containers(
    st: &SharedState,
    game: &game::Model,
    only_participation: Option<i32>,
    // When false, skip the WireGuard hub setup/sync (network + per-team peers +
    // `configure_interface`, which FLUSHES wg0's addresses/peers and briefly
    // disrupts live tunnels). The per-tick container reconcile passes false — a
    // recreated service container never changes the team peer set, so touching wg0
    // every tick is pure churn. Team accept / manual ensure pass true.
    ensure_vpn: bool,
    // The round pipeline repairs A&D before checking, but KotH only after the
    // checker has persisted a dead-backend receipt for the published holder.
    ensure_koth: bool,
    // Durable event jobs page participants and finalize topology exactly once
    // after the final page. Direct one-team acceptance keeps the old immediate
    // finalization behavior.
    finalize_topology: bool,
    // A manual reconcile keeps one request identity across retries. Binding it
    // to each service lets Docker/Kubernetes adopt a container created before
    // a lost response or process crash instead of launching a duplicate.
    reconcile_operation_id: Option<Uuid>,
) -> AppResult<(i32, i32)> {
    let all_ad: Vec<game_challenge::Model> = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game.id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .all(&st.db)
        .await?;
    // RSCTF `AdSelfHosted` = BYOC: the TEAM hosts the service container and the
    // platform only relays. So the platform launches per-team containers ONLY for
    // platform-hosted (`!ad_self_hosted`) challenges that ship an image; BYOC
    // challenges get a container-less relay row the team fills via `Byoc/Setup`.
    let challenges: Vec<&game_challenge::Model> = all_ad
        .iter()
        .filter(|c| {
            !c.ad_self_hosted
                && c.container_image
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
        })
        .collect();
    let byoc: Vec<&game_challenge::Model> = all_ad.iter().filter(|c| c.ad_self_hosted).collect();

    let mut parts_query = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game.id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted));
    if let Some(pid) = only_participation {
        parts_query = parts_query.filter(participation::Column::Id.eq(pid));
    }
    let parts: Vec<participation::Model> = parts_query.all(&st.db).await?;

    let salt = crate::utils::flag_generator::team_hash_salt(&game.private_key);
    let mut launched = 0;
    let mut failures = 0;
    let has_koth = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game.id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::KingOfTheHill))
        .one(&st.db)
        .await?
        .is_some();
    let has_engine_challenge = !challenges.is_empty() || !byoc.is_empty() || has_koth;
    let need_vpn = should_provision_vpn(
        crate::services::ad_vpn::enabled(),
        game.is_active(Utc::now()),
        has_engine_challenge,
        ensure_vpn,
    );
    let reconcile_vpn = should_reconcile_vpn(need_vpn, !challenges.is_empty());

    // Create the isolated service network before peer/firewall reconciliation,
    // then retain the allocator-selected address so BYOC rows can never drift
    // from WireGuard cryptokey routing after a collision probe.
    if reconcile_vpn
        && st.containers.backend_kind() == crate::services::container::ContainerBackendKind::Docker
    {
        st.containers
            .ensure_network(
                &crate::services::ad_vpn::services_network(),
                &crate::services::ad_vpn::services_cidr(),
            )
            .await?;
    }
    let mut vpn_addresses = std::collections::HashMap::new();
    if need_vpn {
        for p in &parts {
            let peer = crate::services::ad_vpn::ensure_peer_deferred(&st.db, game.id, p.id).await?;
            vpn_addresses.insert(p.id, peer.address);
        }
    }

    // BYOC (self-hosted) challenges: NO per-team relay container (that would spawn
    // O(teams × challenges) proxy containers on the host). Instead the platform
    // runs a SINGLE WireGuard hub and hands each team a stable /32 (the same one
    // `Ad/Vpn/Config` issues); the team runs its own container behind that /32 and
    // the checker/attack engine reaches it directly at `{team_/32}:{expose_port}`
    // over the one tunnel — O(1) containers regardless of team/challenge count.
    // We register the service pointed at that routable address (no container_id),
    // Offline until the team's checker first answers.
    if need_vpn {
        for c in &byoc {
            for p in &parts {
                let distributed = crate::services::ad::service_lifecycle::acquire_publication_lock(
                    st.pg(),
                    game.id,
                    p.id,
                    c.id,
                )
                .await?;
                let Some((p, c)) = current_ad_pair(st, game.id, p.id, c.id, true).await? else {
                    distributed.release().await?;
                    continue;
                };
                let existing = ad_team_service::Entity::find()
                    .filter(ad_team_service::Column::ParticipationId.eq(p.id))
                    .filter(ad_team_service::Column::ChallengeId.eq(c.id))
                    .one(&st.db)
                    .await?;
                let host = vpn_addresses.get(&p.id).cloned().ok_or_else(|| {
                    AppError::internal("Could not allocate the team's VPN address")
                })?;
                let port = c.expose_port.unwrap_or(80);
                match existing {
                    Some(row) => {
                        if (row.host.is_empty() || row.host == host)
                            && (row.host != host || row.port != port)
                        {
                            let mut active: ad_team_service::ActiveModel = row.into();
                            active.host = Set(host);
                            active.port = Set(port);
                            active.update(&st.db).await?;
                        }
                    }
                    None => {
                        ad_team_service::ActiveModel {
                            game_id: Set(game.id),
                            participation_id: Set(p.id),
                            challenge_id: Set(c.id),
                            host: Set(host),
                            port: Set(port),
                            status: Set(crate::utils::enums::AdCheckStatus::Offline as i16),
                            container_id: Set(None),
                            last_reset_at: Set(None),
                            ..Default::default()
                        }
                        .insert(&st.db)
                        .await?;
                    }
                }
                if current_ad_pair(st, game.id, p.id, c.id, true)
                    .await?
                    .is_none()
                {
                    deactivate_stale_pair(st, p.id, c.id).await?;
                }
                distributed.release().await?;
            }
        }
    }

    for c in &challenges {
        for p in &parts {
            let distributed = crate::services::ad::service_lifecycle::acquire_publication_lock(
                st.pg(),
                game.id,
                p.id,
                c.id,
            )
            .await?;
            let Some((p, c)) = current_ad_pair(st, game.id, p.id, c.id, false).await? else {
                distributed.release().await?;
                continue;
            };
            let mut existing = ad_team_service::Entity::find()
                .filter(ad_team_service::Column::ParticipationId.eq(p.id))
                .filter(ad_team_service::Column::ChallengeId.eq(c.id))
                .one(&st.db)
                .await?;
            // Skip only if the service's container is actually ALIVE. A dead one
            // (crashed / reaped) must be recreated — otherwise it stays Offline, or,
            // since A&D services and KotH hills share the rsctf-ad subnet, its freed
            // IP gets reused by another container and the checker silently hits the
            // wrong service (an unexplained Mumble). Tear down the stale container
            // first so it can't linger.
            if let Some(cid) = existing.as_ref().and_then(|s| s.container_id.clone()) {
                let endpoint_is_published = existing
                    .as_ref()
                    .is_some_and(|service| !service.host.trim().is_empty() && service.port > 0);
                if endpoint_is_published && st.containers.is_running(&cid).await {
                    distributed.release().await?;
                    continue; // already running
                }
                crate::services::ad_vpn::deactivate_team_service(
                    &st.db,
                    existing.as_ref().unwrap().id,
                )
                .await?;
                crate::services::traffic::destroy_container_after_capture_fence(st, &cid).await?;
                if let Some(row) = existing.as_mut() {
                    row.host.clear();
                    row.port = 0;
                    row.container_id = None;
                    row.status = crate::utils::enums::AdCheckStatus::Offline as i16;
                }
            }
            let team_hash =
                crate::utils::flag_generator::team_challenge_hash(&salt, c.id, &p.token);
            let operation_id = ad_service_operation_id(reconcile_operation_id, game.id, p.id, c.id);
            let flag = operation_id.as_deref().map_or_else(
                || {
                    crate::utils::flag_generator::generate_flag(
                        c.flag_template.as_deref(),
                        &team_hash,
                    )
                },
                |operation_id| {
                    crate::utils::flag_generator::generate_retryable_flag(
                        c.flag_template.as_deref(),
                        &team_hash,
                        operation_id,
                    )
                },
            );
            let image = match crate::services::challenge_images::runtime_image(st, &c) {
                Ok(image) => image,
                Err(error) => {
                    tracing::warn!(
                        challenge = c.id,
                        %error,
                        "A&D service image is not immutably published"
                    );
                    failures += 1;
                    distributed.release().await?;
                    continue;
                }
            };
            let mut spec = ContainerSpec::ad_service(
                image,
                ContainerResourceLimits {
                    memory_limit: c.memory_limit.unwrap_or(256),
                    cpu_count: c.cpu_count.unwrap_or(1),
                    storage_limit: crate::services::container::storage_limit_or_default(
                        c.storage_limit,
                    ),
                },
                c.expose_port.unwrap_or(80),
                p.team_id,
                c.ad_allow_egress,
                flag,
            );
            spec.operation_id = operation_id;
            let info = match st.containers.create(spec).await {
                Ok(i) => i,
                Err(error) => {
                    let operation_conflict =
                        is_manual_operation_conflict(&error, reconcile_operation_id);
                    // Best-effort (accept path): register a container-less service
                    // row so the team shows in the grid without failing the accept.
                    // Gated on `only_participation` so the manual endpoint keeps its
                    // exact skip-on-failure behavior. Guarded on `existing.is_none()`
                    // so a re-accept never inserts a duplicate row.
                    if only_participation.is_some() && existing.is_none() {
                        ad_team_service::ActiveModel {
                            game_id: Set(game.id),
                            participation_id: Set(p.id),
                            challenge_id: Set(c.id),
                            host: Set(String::new()),
                            port: Set(0),
                            status: Set(crate::utils::enums::AdCheckStatus::Offline as i16),
                            container_id: Set(None),
                            last_reset_at: Set(None),
                            ..Default::default()
                        }
                        .insert(&st.db)
                        .await?;
                    }
                    if current_ad_pair(st, game.id, p.id, c.id, false)
                        .await?
                        .is_none()
                    {
                        deactivate_stale_pair(st, p.id, c.id).await?;
                    }
                    failures += 1;
                    distributed.release().await?;
                    if operation_conflict {
                        return Err(error);
                    }
                    continue;
                }
            };
            let backend_id = info.id.clone();
            let retained = crate::services::ad::service_lifecycle::retain_created_backend_identity(
                st.pg(),
                game.id,
                p.id,
                c.id,
                &backend_id,
            )
            .await;
            match retained {
                Ok(true) => {}
                Ok(false) => {
                    st.containers.destroy(&backend_id).await?;
                    distributed.release().await?;
                    continue;
                }
                Err(error) => {
                    if let Err(destroy_error) = st.containers.destroy(&backend_id).await {
                        tracing::error!(
                            backend_id = %backend_id,
                            %destroy_error,
                            "failed to destroy A&D backend whose retry identity could not be retained"
                        );
                    }
                    return Err(error);
                }
            }
            match current_ad_pair(st, game.id, p.id, c.id, false).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    crate::services::ad::service_lifecycle::rollback_created_backend(
                        st,
                        p.id,
                        c.id,
                        &backend_id,
                    )
                    .await?;
                    distributed.release().await?;
                    continue;
                }
                Err(error) => {
                    crate::services::ad::service_lifecycle::rollback_created_backend(
                        st,
                        p.id,
                        c.id,
                        &backend_id,
                    )
                    .await?;
                    return Err(error);
                }
            }
            let publication = crate::services::ad::service_lifecycle::ManagedBackendPublication {
                game_id: game.id,
                participation_id: p.id,
                challenge_id: c.id,
                host: &info.ip,
                port: info.port,
                backend_id: &backend_id,
            };
            let persisted =
                crate::services::ad::service_lifecycle::publish_managed_backend_if_eligible(
                    st.pg(),
                    publication,
                )
                .await;
            match persisted {
                Ok(true) => {}
                Ok(false) => {
                    crate::services::ad::service_lifecycle::rollback_created_backend(
                        st,
                        p.id,
                        c.id,
                        &backend_id,
                    )
                    .await?;
                    distributed.release().await?;
                    continue;
                }
                Err(error) => {
                    let rollback =
                        crate::services::ad::service_lifecycle::rollback_created_backend(
                            st,
                            p.id,
                            c.id,
                            &backend_id,
                        )
                        .await;
                    if let Err(rollback_error) = rollback {
                        tracing::error!(
                            backend_id = %backend_id,
                            %rollback_error,
                            "failed to destroy unpublished A&D container after persistence failure"
                        );
                    }
                    return Err(error);
                }
            }
            match current_ad_pair(st, game.id, p.id, c.id, false).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    crate::services::ad::service_lifecycle::rollback_created_backend(
                        st,
                        p.id,
                        c.id,
                        &backend_id,
                    )
                    .await?;
                    distributed.release().await?;
                    continue;
                }
                Err(error) => {
                    crate::services::ad::service_lifecycle::rollback_created_backend(
                        st,
                        p.id,
                        c.id,
                        &backend_id,
                    )
                    .await?;
                    return Err(error);
                }
            }
            launched += 1;
            distributed.release().await?;
            if c.enable_traffic_capture {
                crate::services::traffic::start_container_capture(st, &backend_id).await?;
            }
        }
    }

    // Reconcile the wg0 hub with the (possibly newly-created) peer set.
    if reconcile_vpn && finalize_topology {
        crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
    }

    if ensure_koth && finalize_topology {
        crate::controllers::game::koth::ensure_koth_hills_with_operation(
            st,
            game.id,
            reconcile_operation_id,
        )
        .await?;
    }

    Ok((launched, failures))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use uuid::Uuid;

    use super::{
        ad_service_operation_id, is_manual_operation_conflict, reconcile_operation_id,
        should_provision_vpn, should_reconcile_vpn, IDEMPOTENCY_KEY_HEADER,
    };

    #[test]
    fn reconcile_identity_is_backward_compatible_and_binds_each_service_retry() {
        let mut headers = HeaderMap::new();
        assert!(!reconcile_operation_id(&headers).unwrap().is_nil());
        headers.insert(
            IDEMPOTENCY_KEY_HEADER,
            HeaderValue::from_static("not-a-uuid"),
        );
        assert!(reconcile_operation_id(&headers).is_err());

        let first = Uuid::new_v4();
        headers.insert(
            IDEMPOTENCY_KEY_HEADER,
            HeaderValue::from_str(&first.to_string()).unwrap(),
        );
        assert_eq!(reconcile_operation_id(&headers).unwrap(), first);
        let service = ad_service_operation_id(Some(first), 7, 11, 13).unwrap();
        assert_eq!(
            ad_service_operation_id(Some(first), 7, 11, 13).as_deref(),
            Some(service.as_str())
        );
        assert_ne!(
            ad_service_operation_id(Some(Uuid::new_v4()), 7, 11, 13).as_deref(),
            Some(service.as_str())
        );
        assert_ne!(
            ad_service_operation_id(Some(first), 7, 11, 14).as_deref(),
            Some(service.as_str())
        );
        assert_eq!(ad_service_operation_id(None, 7, 11, 13), None);
    }

    #[test]
    fn only_a_definitive_manual_workload_conflict_rotates_the_operation() {
        let operation_id = Some(Uuid::new_v4());
        assert!(is_manual_operation_conflict(
            &crate::utils::error::AppError::conflict("changed launch specification"),
            operation_id
        ));
        assert!(!is_manual_operation_conflict(
            &crate::utils::error::AppError::unavailable("runtime unavailable"),
            operation_id
        ));
        assert!(!is_manual_operation_conflict(
            &crate::utils::error::AppError::conflict("automatic repair"),
            None
        ));
    }

    #[test]
    fn round_repair_skips_byoc_vpn_reprovisioning() {
        let need_vpn = should_provision_vpn(true, true, true, false);
        assert!(!need_vpn);
        assert!(!should_reconcile_vpn(need_vpn, false));
    }

    #[test]
    fn explicit_vpn_provisioning_still_configures_byoc() {
        let need_vpn = should_provision_vpn(true, true, true, true);
        assert!(need_vpn);
        assert!(should_reconcile_vpn(need_vpn, false));
    }

    #[test]
    fn managed_service_repairs_still_reconcile_the_network() {
        let need_vpn = should_provision_vpn(true, true, true, false);
        assert!(!need_vpn);
        assert!(should_reconcile_vpn(need_vpn, true));
    }
}

/// Port of RSCTF `ParticipationRepository.EnsureInstances`
/// (`ParticipationRepository.cs:14`): insert a `GameInstance` row for every
/// enabled + `Active`-review challenge in the game that this participation does
/// not already have one for. Run when a participation is Accepted so the team's
/// jeopardy play surface exists immediately (each row is what a player later
/// loads a container for). Idempotent — existing `(participation, challenge)`
/// rows are skipped, so a re-accept is a no-op. Returns the number inserted.
///
/// NOTE: this is generic participation-repository logic; the source mirror would
/// place it under `repositories/participation`. It lives here only because this
/// change is scoped to `edit/ad/` + `admin/mod.rs` and `admin/mod.rs` is at the
/// enforced ~1000-line ceiling.
pub(crate) async fn ensure_instances(
    st: &SharedState,
    participation_id: i32,
    game_id: i32,
) -> AppResult<usize> {
    let flight_key = format!("game-container:{participation_id}");
    let _flight = crate::utils::single_flight::coalesce(&flight_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &flight_key).await?;
    let inserted = sqlx::query(
        r#"INSERT INTO "GameInstances"
              (challenge_id, participation_id, is_loaded, last_container_operation,
               flag_id, container_id)
           SELECT challenge.id, participation.id, FALSE, clock_timestamp(), NULL, NULL
             FROM "Participations" participation
             JOIN "GameChallenges" challenge
               ON challenge.game_id = participation.game_id
            WHERE participation.id = $1
              AND participation.game_id = $2
              AND participation.status = $3
              AND challenge.is_enabled = TRUE
              AND challenge.review_status = $4
           ON CONFLICT (participation_id, challenge_id) DO NOTHING"#,
    )
    .bind(participation_id)
    .bind(game_id)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(ChallengeReviewStatus::Active as i16)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected() as usize;
    distributed.release().await?;
    Ok(inserted)
}

/// Provision a freshly-accepted participation's play resources — the rsctf
/// analogue of what RSCTF's `UpdateParticipationStatus` triggers on `Accepted`.
/// (1) [`ensure_instances`] — a `GameInstance` per enabled+Active challenge the
/// team lacks; (2) [`ensure_ad_containers`] scoped to this participation — the
/// team's self-hosted A&D service containers (best-effort on a Docker outage).
/// Called from `admin::update_participation`.
pub(crate) async fn provision_accepted_participation(
    st: &SharedState,
    game_id: i32,
    participation_id: i32,
) -> AppResult<()> {
    ensure_instances(st, participation_id, game_id).await?;
    if let Some(game) = game::Entity::find_by_id(game_id).one(&st.db).await? {
        let (_, failures) =
            ensure_ad_containers(st, &game, Some(participation_id), true, true, true, None).await?;
        if failures > 0 {
            return Err(AppError::unavailable(format!(
                "{failures} accepted-participation service workload(s) remain unavailable"
            )));
        }
    }
    Ok(())
}
