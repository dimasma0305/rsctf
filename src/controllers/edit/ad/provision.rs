//! A&D container ensure/provision (EnsureContainers, EnsureInstances, on-accept
//! provisioning) — split from edit/ad/mod.rs to stay under the 1000-line rule.
use super::super::*;

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
    let game_exists = game::Entity::find()
        .filter(game::Column::Id.eq(game_id))
        .filter(game::Column::EndTimeUtc.gte(Utc::now()))
        .one(&st.db)
        .await?
        .is_some();
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
        crate::services::traffic::stop_container_capture(st, &backend_id).await?;
        let _ = st.containers.destroy(&backend_id).await;
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
    _admin: AdminUser,
    Path(game_id): Path<i32>,
) -> AppResult<MessageResponse> {
    let game = load_game(&st, game_id).await?;
    let (launched, failures) = ensure_ad_containers(&st, &game, None, true, true).await?;
    Ok(MessageResponse::ok(format!(
        "Launched {launched} service container(s){}",
        if failures > 0 {
            format!(", {failures} failed (runtime unavailable?)")
        } else {
            String::new()
        }
    )))
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
                let lock_key = format!("ad-service:{}:{}", p.id, c.id);
                let _local = crate::utils::single_flight::coalesce(&lock_key).await;
                let distributed =
                    crate::utils::single_flight::PgAdvisoryLock::acquire(st.pg(), &lock_key)
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
            let lock_key = format!("ad-service:{}:{}", p.id, c.id);
            let _local = crate::utils::single_flight::coalesce(&lock_key).await;
            let distributed = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(
                st.pg(),
                &lock_key,
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
                if let Some(row) = existing.as_mut() {
                    row.host.clear();
                    row.port = 0;
                    row.container_id = None;
                    row.status = crate::utils::enums::AdCheckStatus::Offline as i16;
                }
                crate::services::traffic::stop_container_capture(st, &cid).await?;
                let _ = st.containers.destroy(&cid).await;
            }
            let team_hash =
                crate::utils::flag_generator::team_challenge_hash(&salt, c.id, &p.token);
            let flag =
                crate::utils::flag_generator::generate_flag(c.flag_template.as_deref(), &team_hash);
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
            let info = match st
                .containers
                .create(ContainerSpec::ad_service(
                    image,
                    c.memory_limit.unwrap_or(256),
                    c.cpu_count.unwrap_or(1),
                    c.expose_port.unwrap_or(80),
                    p.team_id,
                    c.ad_allow_egress,
                    flag,
                ))
                .await
            {
                Ok(i) => i,
                Err(_) => {
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
                    continue;
                }
            };
            let backend_id = info.id.clone();
            if current_ad_pair(st, game.id, p.id, c.id, false)
                .await?
                .is_none()
            {
                let _ = st.containers.destroy(&backend_id).await;
                distributed.release().await?;
                continue;
            }
            let persisted = match existing {
                Some(s) => {
                    let mut am: ad_team_service::ActiveModel = s.into();
                    am.host = Set(info.ip);
                    am.port = Set(info.port);
                    am.container_id = Set(Some(info.id));
                    am.status = Set(crate::utils::enums::AdCheckStatus::Ok as i16);
                    am.update(&st.db).await.map(|_| ())
                }
                None => ad_team_service::ActiveModel {
                    game_id: Set(game.id),
                    participation_id: Set(p.id),
                    challenge_id: Set(c.id),
                    host: Set(info.ip),
                    port: Set(info.port),
                    status: Set(crate::utils::enums::AdCheckStatus::Ok as i16),
                    container_id: Set(Some(info.id)),
                    last_reset_at: Set(None),
                    ..Default::default()
                }
                .insert(&st.db)
                .await
                .map(|_| ()),
            };
            if let Err(err) = persisted {
                crate::services::traffic::stop_container_capture(st, &backend_id).await?;
                let _ = st.containers.destroy(&backend_id).await;
                return Err(err.into());
            }
            if current_ad_pair(st, game.id, p.id, c.id, false)
                .await?
                .is_none()
            {
                deactivate_stale_pair(st, p.id, c.id).await?;
                distributed.release().await?;
                continue;
            }
            launched += 1;
            distributed.release().await?;
            if c.enable_traffic_capture {
                crate::services::traffic::start_container_capture(st, &backend_id).await?;
            }
        }
    }

    // Reconcile the wg0 hub with the (possibly newly-created) peer set.
    if reconcile_vpn {
        crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
    }

    if ensure_koth {
        crate::controllers::game::koth::ensure_koth_hills(st, game.id).await?;
    }

    Ok((launched, failures))
}

#[cfg(test)]
mod tests {
    use super::{should_provision_vpn, should_reconcile_vpn};

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
    let accepted = participation::Entity::find()
        .filter(participation::Column::Id.eq(participation_id))
        .filter(participation::Column::GameId.eq(game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .one(&st.db)
        .await?
        .is_some();
    if !accepted {
        distributed.release().await?;
        return Ok(0);
    }
    let existing: std::collections::HashSet<i32> = game_instance::Entity::find()
        .filter(game_instance::Column::ParticipationId.eq(participation_id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|gi| gi.challenge_id)
        .collect();

    let challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::IsEnabled.eq(true))
        .filter(game_challenge::Column::ReviewStatus.eq(ChallengeReviewStatus::Active))
        .all(&st.db)
        .await?;

    let mut inserted = 0;
    for c in challenges {
        if existing.contains(&c.id) {
            continue;
        }
        let created: Option<i32> = sqlx::query_scalar(
            r#"INSERT INTO "GameInstances"
                 (challenge_id, participation_id, is_loaded, last_container_operation,
                  flag_id, container_id)
               SELECT challenge.id, participation.id, FALSE, now(), NULL, NULL
                 FROM "Participations" participation
                 JOIN "GameChallenges" challenge
                   ON challenge.id = $3 AND challenge.game_id = participation.game_id
                WHERE participation.id = $1
                  AND participation.game_id = $2
                  AND participation.status = $4
                  AND challenge.is_enabled = TRUE
                  AND challenge.review_status = $5
                  AND NOT EXISTS (
                      SELECT 1 FROM "GameInstances" existing
                       WHERE existing.participation_id = participation.id
                         AND existing.challenge_id = challenge.id
                  )
               RETURNING id"#,
        )
        .bind(participation_id)
        .bind(game_id)
        .bind(c.id)
        .bind(ParticipationStatus::Accepted as i16)
        .bind(ChallengeReviewStatus::Active as i16)
        .fetch_optional(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        inserted += usize::from(created.is_some());
    }
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
        ensure_ad_containers(st, &game, Some(participation_id), true, true).await?;
    }
    Ok(())
}
