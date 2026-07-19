//! Edit-facing A&D operator console.
use axum::extract::Query;
use axum::response::IntoResponse;

use super::*;

mod provision;
pub use provision::*;

/// A&D admin — force round-advance result (`Api.ts` `AdAdvanceRoundResult`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdAdvanceRoundResult {
    pub round_number: i32,
    pub flags_planted: i32,
    #[serde(with = "crate::utils::datetime::millis")]
    pub started_at: DateTime<Utc>,
    #[serde(with = "crate::utils::datetime::millis")]
    pub ends_at: DateTime<Utc>,
}

/// A&D admin — per-challenge state (`Api.ts` `AdChallengeStateModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdChallengeStateModel {
    pub challenge_id: i32,
    pub title: String,
    pub is_enabled: bool,
    pub tick_seconds: i32,
    pub flag_lifetime_ticks: i32,
    pub teams_with_live_container: Option<i32>,
}

/// A&D admin — per-cell (team × challenge) state (`Api.ts` `AdTeamCellModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTeamCellModel {
    pub ad_team_service_id: i32,
    pub challenge_id: i32,
    pub container_ip: Option<String>,
    pub container_port: Option<i32>,
    pub container_guid: Option<String>,
    pub last_check_status: Option<String>,
    pub last_check_id: Option<i32>,
    pub current_flag: Option<String>,
    pub snapshot_available: bool,
    pub changed_file_count: Option<i32>,
    pub self_hosted: bool,
}

/// A&D admin — one team row in the grid (`Api.ts` `AdTeamRowModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdTeamRowModel {
    pub participation_id: i32,
    pub team_name: String,
    pub services: Vec<AdTeamCellModel>,
}

/// A&D admin — the operator console state (`Api.ts` `AdGameStateModel`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdGameStateModel {
    pub current_round: Option<i32>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_started_at: Option<DateTime<Utc>>,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub round_ends_at: Option<DateTime<Utc>>,
    pub scoring_paused: bool,
    #[serde(with = "crate::utils::datetime::millis_opt")]
    pub scoring_paused_at: Option<DateTime<Utc>>,
    pub challenges: Vec<AdChallengeStateModel>,
    pub teams: Vec<AdTeamRowModel>,
}

/// Human label for a stored `AdCheckStatus` numeric (matches the `AdCheckStatus`
/// string enum the React console keys its status colours off of).
fn ad_check_status_label(status: i16) -> &'static str {
    match status {
        0 => "Ok",
        1 => "Mumble",
        2 => "Offline",
        _ => "InternalError",
    }
}

/// `POST /api/edit/games/{id}/ad/AdvanceRound` -> `AdAdvanceRoundResult`.
///
/// Retained as a typed compatibility route. The delegated handler rejects the
/// request because only the automatic checker pipeline may create scored rounds.
pub async fn ad_advance_round(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<AdAdvanceRoundResult>> {
    manager_or_admin(&st, &user, game_id).await?;
    let result =
        crate::controllers::admin::ad::advance_round(State(st), AdminUser(user), Path(game_id))
            .await?
            .data;
    Ok(RequestResponse::ok(AdAdvanceRoundResult {
        round_number: result.round,
        flags_planted: result.flags_planted,
        started_at: result.started_at,
        ends_at: result.ends_at,
    }))
}

/// `GET /api/edit/games/{id}/ad/State` -> `AdGameStateModel`.
///
/// Port of `AdAdminController.State`: the live round window + scoring-pause state
/// + the per-(team × challenge) A&D grid. Challenges are `AttackDefense` only —
///
/// KotH has its own `Koth/State` console (the React `AdOps` toggles between them
/// and gates KotH-only detection on `challenges.length == 0`). Per cell we
/// surface the registered service endpoint, the current round's planted flag,
/// and the latest checker verdict; self-hosted (BYOC) cells hide the endpoint
/// (it's the tunnel relay, not the team's service) and only expose SLA status.
pub async fn ad_state(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<AdGameStateModel>> {
    manager_or_admin(&st, &user, game_id).await?;
    let game = load_game(&st, game_id).await?;

    // A&D challenges only (KotH is served by the separate Koth/State endpoint).
    let ad_challenges = game_challenge::Entity::find()
        .filter(game_challenge::Column::GameId.eq(game_id))
        .filter(game_challenge::Column::ChallengeType.eq(ChallengeType::AttackDefense))
        .order_by_asc(game_challenge::Column::Id)
        .all(&st.db)
        .await?;

    // Latest round = the live tick (round timing + which flags are current).
    let current_round = ad_round::Entity::find()
        .filter(ad_round::Column::GameId.eq(game_id))
        .order_by_desc(ad_round::Column::Number)
        .one(&st.db)
        .await?;

    // Accepted teams only (mirrors RSCTF's grid roster).
    let participations = participation::Entity::find()
        .filter(participation::Column::GameId.eq(game_id))
        .filter(participation::Column::Status.eq(ParticipationStatus::Accepted))
        .all(&st.db)
        .await?;
    let part_ids: Vec<i32> = participations.iter().map(|p| p.id).collect();

    // participation id -> team display name (guard empty IN()).
    let team_ids: Vec<i32> = {
        let mut seen = std::collections::HashSet::new();
        participations
            .iter()
            .map(|p| p.team_id)
            .filter(|id| seen.insert(*id))
            .collect()
    };
    let team_names: std::collections::HashMap<i32, String> = if team_ids.is_empty() {
        std::collections::HashMap::new()
    } else {
        team::Entity::find()
            .filter(team::Column::Id.is_in(team_ids))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|t| (t.id, t.name))
            .collect()
    };

    // Every registered A&D/KotH service for the accepted teams. KotH service rows
    // may be included; they simply never match an AttackDefense column (fidelity
    // with RSCTF, which also doesn't type-filter here).
    let services = if part_ids.is_empty() {
        Vec::new()
    } else {
        ad_team_service::Entity::find()
            .filter(ad_team_service::Column::GameId.eq(game_id))
            .filter(ad_team_service::Column::ParticipationId.is_in(part_ids.clone()))
            .all(&st.db)
            .await?
    };
    let service_ids: Vec<i32> = services.iter().map(|s| s.id).collect();

    // Latest checker verdict per service (max checked_at) → status label + id.
    let last_check_by_service: std::collections::HashMap<i32, ad_check_result::Model> =
        if service_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            let checks = ad_check_result::Entity::find()
                .filter(ad_check_result::Column::TeamServiceId.is_in(service_ids.clone()))
                .all(&st.db)
                .await?;
            let mut latest: std::collections::HashMap<i32, ad_check_result::Model> =
                std::collections::HashMap::new();
            for c in checks {
                match latest.get(&c.team_service_id) {
                    Some(prev) if prev.checked_at >= c.checked_at => {}
                    _ => {
                        latest.insert(c.team_service_id, c);
                    }
                }
            }
            latest
        };

    // The current round's planted flag per service (the operator "copy flag").
    let current_flags: std::collections::HashMap<i32, String> = match &current_round {
        Some(r) if !service_ids.is_empty() => ad_flag::Entity::find()
            .filter(ad_flag::Column::RoundId.eq(r.id))
            .filter(ad_flag::Column::TeamServiceId.is_in(service_ids.clone()))
            .all(&st.db)
            .await?
            .into_iter()
            .map(|f| (f.team_service_id, f.flag))
            .collect(),
        _ => std::collections::HashMap::new(),
    };

    // Self-hosted (BYOC) challenges: the service address is the tunnel relay, not
    // the team's box — expose only the SLA status, never the endpoint.
    let byoc_challenge_ids: std::collections::HashSet<i32> = ad_challenges
        .iter()
        .filter(|c| c.ad_self_hosted)
        .map(|c| c.id)
        .collect();

    let teams: Vec<AdTeamRowModel> = participations
        .iter()
        .map(|p| {
            let cells = services
                .iter()
                .filter(|s| s.participation_id == p.id)
                .map(|s| {
                    let is_byoc = byoc_challenge_ids.contains(&s.challenge_id);
                    let last = last_check_by_service.get(&s.id);
                    AdTeamCellModel {
                        ad_team_service_id: s.id,
                        challenge_id: s.challenge_id,
                        // host:port is the registered probe endpoint (the team's
                        // container address); hidden for BYOC relays.
                        container_ip: if is_byoc { None } else { Some(s.host.clone()) },
                        container_port: if is_byoc { None } else { Some(s.port) },
                        // Raw docker id of the team's service container — the admin
                        // exec hub accepts it directly (A&D containers aren't in the
                        // `container` table). A self-hosted (BYOC) service has no local
                        // container, but the hub CAN shell into it over the team's
                        // agent tunnel — encode `byoc:<pid>:<cid>` so the hub routes to
                        // the 'E' stream (resolves to "Open shell" in the UI).
                        container_guid: if is_byoc {
                            Some(format!("byoc:{}:{}", s.participation_id, s.challenge_id))
                        } else {
                            s.container_id.clone().filter(|c| !c.is_empty())
                        },
                        last_check_status: last
                            .map(|c| ad_check_status_label(c.status).to_string()),
                        last_check_id: last.map(|c| c.id),
                        current_flag: current_flags.get(&s.id).cloned(),
                        // Snapshot tarballs are blob/k8s-gated — none stored.
                        snapshot_available: false,
                        changed_file_count: None,
                        self_hosted: is_byoc,
                    }
                })
                .collect();
            AdTeamRowModel {
                participation_id: p.id,
                team_name: team_names.get(&p.team_id).cloned().unwrap_or_default(),
                services: cells,
            }
        })
        .collect();

    // Tick + flag lifetime are game-wide (RSCTF `Game.AdTickSeconds` /
    // `AdFlagLifetimeTicks`) — same value on every challenge row.
    let tick_seconds = game.ad_tick_seconds.unwrap_or(60);
    let flag_lifetime_ticks = game.ad_flag_lifetime_ticks.unwrap_or(5);
    let challenges: Vec<AdChallengeStateModel> = ad_challenges
        .iter()
        .map(|c| AdChallengeStateModel {
            challenge_id: c.id,
            title: c.title.clone(),
            is_enabled: c.is_enabled,
            tick_seconds,
            flag_lifetime_ticks,
            // Count of this challenge's registered team-services that currently
            // hold a live platform container (non-null `container_id`) — computed
            // over the accepted-team service rows already loaded above.
            teams_with_live_container: Some(
                services
                    .iter()
                    .filter(|s| s.challenge_id == c.id && s.container_id.is_some())
                    .count() as i32,
            ),
        })
        .collect();

    Ok(RequestResponse::ok(AdGameStateModel {
        current_round: current_round.as_ref().map(|r| r.number),
        round_started_at: current_round.as_ref().map(|r| r.start_time_utc),
        round_ends_at: current_round.as_ref().map(|r| r.end_time_utc),
        scoring_paused: game.ad_scoring_paused,
        scoring_paused_at: game.ad_scoring_paused_at,
        challenges,
        teams,
    }))
}

/// `POST /api/edit/games/{id}/ad/ScoringPause` -> `{ scoringPaused: boolean }`.
///
/// Port of `AdAdminController.ToggleScoringPause`: flip `Game.AdScoringPaused`,
/// stamping `AdScoringPausedAt` on pause. On resume, extend the current round by
/// the paused duration so it doesn't instantly expire. Its start timestamp is
/// immutable because official freeze/cutoff views use it as evidence identity.
pub async fn ad_scoring_pause(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, game_id).await?;
    // Checker result persistence takes the same lock. A pass that committed first
    // stays committed; a pass already running when pause wins may still land in
    // the unchanged current round, and no new pass starts while paused.
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let tx = control.transaction_mut();
    let (was_paused, paused_at): (bool, Option<DateTime<Utc>>) = sqlx::query_as(
        r#"SELECT ad_scoring_paused, ad_scoring_paused_at
             FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let paused = !was_paused;

    // Resuming: give the live round back the time it was frozen for.
    if !paused {
        sqlx::query(
            r#"UPDATE "AdRounds" round
                  SET end_time_utc = round.end_time_utc
                    + GREATEST(clock_timestamp() - $2, interval '0 seconds')
                WHERE round.id = (
                  SELECT id FROM "AdRounds"
                   WHERE game_id = $1
                   ORDER BY number DESC, id DESC LIMIT 1
                )"#,
        )
        .bind(game_id)
        .bind(paused_at.unwrap_or_else(Utc::now))
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }

    sqlx::query(
        r#"UPDATE "Games"
              SET ad_scoring_paused = $2,
                  ad_scoring_paused_at = CASE WHEN $2
                    THEN clock_timestamp() ELSE NULL END
            WHERE id = $1"#,
    )
    .bind(game_id)
    .bind(paused)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    flush_ad_scoreboard(&st, game_id).await;

    Ok(RequestResponse::ok(json!({ "scoringPaused": paused })))
}

/// `POST /api/edit/games/{id}/ad/Challenges/{challengeId}/Toggle` ->
/// `{ isEnabled: boolean }`.
///
/// Port of `AdAdminController.ToggleChallenge`: flip the target challenge's
/// `IsEnabled`. Gated on `UsesAdEngine()` so BOTH A&D and KotH challenges toggle
/// (the KotH console's per-hill switch hits this same route); non-A&D/KotH
/// challenges are rejected.
pub async fn ad_toggle_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, game_id).await?;
    let challenge = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if !challenge.challenge_type.uses_ad_engine() {
        return Err(AppError::bad_request("Not an A&D / KotH challenge"));
    }

    let mut engine_control =
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?);
    if ad_epoch_scoring_started_locked(
        engine_control
            .as_mut()
            .expect("engine challenge holds the game control lock")
            .transaction_mut(),
        game_id,
    )
    .await?
    {
        return Err(AppError::bad_request(
            "A&D/KotH challenge enabled state is locked after epoch scoring has started.",
        ));
    }
    let challenge = game_challenge::Entity::find()
        .filter(game_challenge::Column::Id.eq(challenge_id))
        .filter(game_challenge::Column::GameId.eq(game_id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;

    let is_enabled = !challenge.is_enabled;
    let mut am: game_challenge::ActiveModel = challenge.clone().into();
    am.is_enabled = Set(is_enabled);
    am.update(&st.db).await?;
    if !is_enabled && challenge.challenge_type == ChallengeType::KingOfTheHill {
        crate::services::ad_engine::clear_challenge_control(&st.db, game_id, challenge_id).await?;
    }
    if let Some(lock) = engine_control {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    // Both A&D and KotH challenge membership feeds the shared epoch surfaces.
    // Flush after either engine-backed toggle so KotH eligibility and board
    // caches do not wait for their TTL on the writer replica.
    flush_ad_scoreboard(&st, game_id).await;
    if !is_enabled {
        st.byoc.disconnect_challenge(&st.db, challenge_id).await?;
        crate::controllers::edit::destroy_challenge_containers(&st, &challenge).await;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;

    Ok(RequestResponse::ok(json!({ "isEnabled": is_enabled })))
}

/// Body of `POST .../Checks/{checkId}/Override` (`Api.ts` `AdOverrideCheckModel`).
/// `newStatus` is the `AdCheckStatus` STRING enum on the wire ("Ok" / "Mumble" /
/// "Offline" / "InternalError"), not a numeric.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdOverrideCheckModel {
    pub new_status: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// Map an `AdCheckStatus` wire string to its stored numeric, or `None` if it
/// isn't one of the four known verdicts (rejected as a 400 rather than silently
/// stored as `InternalError`).
fn ad_check_status_from_label(s: &str) -> Option<i16> {
    match s {
        "Ok" => Some(0),
        "Mumble" => Some(1),
        "Offline" => Some(2),
        "InternalError" => Some(3),
        _ => None,
    }
}

/// `POST /api/edit/games/{id}/ad/Checks/{checkId}/Override` -> void.
///
/// Port of `AdAdminController.OverrideCheck`: a judge corrects a recorded SLA
/// verdict (e.g. a transient glitch made a healthy service read Offline). Load
/// the check, scope it to this game via its round (`check -> ad_round ->
/// game_id`), overwrite the verdict, and stamp an override note.
///
/// Official epoch scoring derives SLA evidence from the ordered status history,
/// so correcting this status automatically ripples through later recovery
/// credit. An override does not claim the checker verified a flag.
pub async fn ad_override_check(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path((game_id, check_id)): Path<(i32, i32)>,
    Json(model): Json<AdOverrideCheckModel>,
) -> AppResult<MessageResponse> {
    let new_status = ad_check_status_from_label(&model.new_status)
        .ok_or_else(|| AppError::bad_request("Unknown check status"))?;

    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    crate::services::ad::scoring::lock_epoch_rollups(&mut *control.transaction_mut(), game_id)
        .await?;
    let (previous_status, round_number, completion): (i16, i32, Option<f64>) = sqlx::query_as(
        r#"SELECT result.status, round.number, result.sla_credit
             FROM "AdCheckResults" result
             JOIN "AdRounds" round ON round.id = result.round_id
            WHERE result.id = $1 AND round.game_id = $2
            FOR UPDATE OF result"#,
    )
    .bind(check_id)
    .bind(game_id)
    .fetch_optional(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Check result not found"))?;
    if completion.is_none() {
        return Err(AppError::conflict(
            "The checker result is still pending. Retry after the pass completes.",
        ));
    }

    let previous = ad_check_status_label(previous_status);
    let message = model
        .note
        .filter(|note| !note.trim().is_empty())
        .map(|note| {
            format!(
                "[admin override: {previous} -> {}] {note}",
                model.new_status
            )
        });
    // NULL identifies an unresolved round-preparation placeholder. The epoch
    // scorer recomputes credit from ordered statuses, so zero is only the
    // explicit completion marker and never the final score for this override.
    sqlx::query(
        r#"UPDATE "AdCheckResults"
              SET status = $2, sla_credit = 0.0,
                  message = COALESCE($3, message)
            WHERE id = $1"#,
    )
    .bind(check_id)
    .bind(new_status)
    .bind(message)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    crate::services::ad::scoring::invalidate_rollups_from_round(
        &mut *control.transaction_mut(),
        game_id,
        round_number,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    flush_ad_scoreboard(&st, game_id).await;

    Ok(MessageResponse::ok(""))
}

/// `GET /api/edit/games/{id}/ad/Services/{adTeamServiceId}/File` ->
/// `AdFileViewModel`.
///
/// Port of `AdAdminController.File`: inspect one file inside a team's service
/// container. rsctf reads the CURRENT content by exec-ing `cat <path>` in the
/// live container (best-effort: empty when the service has no platform
/// container). The image `baseline` + `unifiedDiff` need an offline image read
/// rsctf doesn't have, so they stay null (the UI then shows current only). BYOC
/// self-hosted services expose only a relay, not the team's box — return empty
/// rather than leak relay internals (RSCTF refuses outright).
pub async fn ad_service_file(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, ats_id)): Path<(i32, i32)>,
    Query(q): Query<AdFileQuery>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, game_id).await?;
    if q.path.trim().is_empty() {
        return Err(AppError::bad_request("A file path is required"));
    }
    let svc = ad_team_service::Entity::find_by_id(ats_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == game_id)
        .ok_or_else(|| AppError::not_found("Service not found"))?;

    let self_hosted = game_challenge::Entity::find_by_id(svc.challenge_id)
        .one(&st.db)
        .await?
        .map(|c| c.ad_self_hosted)
        .unwrap_or(false);

    let container_running = svc.container_id.is_some();
    let current: JsonValue = match svc.container_id.as_deref().filter(|c| !c.is_empty()) {
        Some(cid) if !self_hosted => {
            match st
                .containers
                .exec(cid, vec!["cat".into(), q.path.clone()])
                .await
            {
                Ok(text) if !text.is_empty() => json!({
                    "size": text.len(),
                    "truncated": false,
                    // exec surfaces stdout+stderr as a lossy String, so we always
                    // present the current side as text (the base64 path is only
                    // reachable with raw bytes, which this backend can't yield).
                    "binary": false,
                    "text": text,
                    "base64": null,
                }),
                _ => JsonValue::Null,
            }
        }
        _ => JsonValue::Null,
    };

    Ok(RequestResponse::ok(json!({
        "path": q.path,
        "containerRunning": container_running,
        "current": current,
        "baseline": null,
        "unifiedDiff": null,
    })))
}

/// Query for `ad_service_file` — the file path to inspect.
#[derive(Debug, Deserialize)]
pub struct AdFileQuery {
    pub path: String,
}

/// `POST /api/edit/games/{id}/ad/Services/{adTeamServiceId}/Inspector` ->
/// `AdInspectorModel`.
pub async fn ad_spawn_inspector(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, _ats_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, game_id).await?;
    Ok(RequestResponse::ok(json!({ "containerGuid": "" })))
}

/// `DELETE /api/edit/games/{id}/ad/Services/{adTeamServiceId}/Inspector/{containerGuid}`
/// -> void.
pub async fn ad_destroy_inspector(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, _ats_id, _container_guid)): Path<(i32, i32, String)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, game_id).await?;
    Ok(MessageResponse::ok(""))
}

/// `POST /api/edit/games/{id}/ad/Services/{adTeamServiceId}/Restart` -> void.
///
/// Port of `AdAdminController.ForceRestart` -> `AdContainerManager.RestartContainerAsync`
/// (AdContainerManager.cs:2423): an operator force-restart of one team's A&D
/// service container — for when a box is wedged and the team can't recover it.
/// Destroys the current container (if any), relaunches a fresh one with the team's
/// rotating flag, re-registers its host:port, and stamps `last_reset_at`. Bypasses
/// the player-facing cooldown + `ad_allow_self_reset` gates (admin override); the
/// single-service destroy+relaunch mirrors the player path `game::ad::reset_service`.
///
/// Self-hosted (BYOC) services run in the team's own container behind a tunnel
/// relay — the platform can't relaunch what it doesn't own, so refuse (matching the
/// File / Snapshot / player-reset endpoints). On a failed relaunch we return 400
/// (and null the stale container link) rather than report a phantom success that
/// leaves the box down, mirroring RSCTF's `RestartContainerAsync` `return false`.
pub async fn ad_restart_service(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, ats_id)): Path<(i32, i32)>,
) -> AppResult<MessageResponse> {
    manager_or_admin(&st, &user, game_id).await?;
    let initial = ad_team_service::Entity::find_by_id(ats_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == game_id)
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    let lock_key = format!(
        "ad-service:{}:{}",
        initial.participation_id, initial.challenge_id
    );
    let _local = crate::utils::single_flight::coalesce(&lock_key).await;
    let distributed =
        crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &lock_key)
            .await?;
    let svc = ad_team_service::Entity::find_by_id(ats_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == game_id)
        .ok_or_else(|| AppError::not_found("Service not found"))?;

    let challenge = game_challenge::Entity::find_by_id(svc.challenge_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if challenge.ad_self_hosted {
        return Err(AppError::bad_request(
            "Self-hosted (BYOC) services cannot be restarted from the platform",
        ));
    }

    let game = game::Entity::find_by_id(game_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    if !game.is_active(Utc::now()) {
        return Err(AppError::bad_request(
            "Service restart is only available while the game is running",
        ));
    }
    let image = crate::services::challenge_images::runtime_image(&st, &challenge)?;
    let part = participation::Entity::find_by_id(svc.participation_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Participation not found"))?;

    // Fence checker persistence before changing endpoint identity. A pending
    // current sample becomes explicit zero-credit reset downtime, while a verdict
    // that already committed remains untouched.
    let replacement = crate::services::ad_engine::prepare_service_reset(
        &st.db,
        game_id,
        svc.id,
        "administrator restart before checker completion",
    )
    .await?;
    // Revoke the endpoint before teardown so Docker cannot reuse its address while
    // the old game policy still authorizes it.
    crate::services::ad_vpn::deactivate_team_service(&st.db, svc.id).await?;
    // Tear the wedged container down (best-effort) + stop its capture, then relaunch.
    if let Some(cid) = &replacement.retired_container_id {
        crate::services::traffic::stop_container_capture(&st, cid).await?;
        let _ = st.containers.destroy(cid).await;
    }

    let prepared_round_id = replacement.prepared_round_id;
    let flag = replacement.current_flag.unwrap_or_else(|| {
        let salt = crate::utils::flag_generator::team_hash_salt(&game.private_key);
        let team_hash =
            crate::utils::flag_generator::team_challenge_hash(&salt, challenge.id, &part.token);
        crate::utils::flag_generator::generate_flag(challenge.flag_template.as_deref(), &team_hash)
    });

    let info = match st
        .containers
        .create(ContainerSpec::ad_service(
            image,
            challenge.memory_limit.unwrap_or(256),
            challenge.cpu_count.unwrap_or(1),
            challenge.expose_port.unwrap_or(80),
            part.team_id,
            challenge.ad_allow_egress,
            flag,
        ))
        .await
    {
        Ok(i) => i,
        Err(_) => {
            return Err(AppError::bad_request("Restart failed; check logs"));
        }
    };

    let backend_id = info.id.clone();
    let published = match crate::services::ad_engine::publish_service_reset(
        &st.db,
        game_id,
        svc.id,
        &info.ip,
        info.port,
        &info.id,
        prepared_round_id,
        true,
    )
    .await
    {
        Ok(published) => published,
        Err(error) => {
            crate::services::traffic::stop_container_capture(&st, &backend_id).await?;
            let _ = st.containers.destroy(&backend_id).await;
            return Err(error);
        }
    };
    if !published {
        crate::services::traffic::stop_container_capture(&st, &backend_id).await?;
        let _ = st.containers.destroy(&backend_id).await;
        return Err(AppError::conflict(
            "Service eligibility changed while the replacement was launching",
        ));
    }

    distributed.release().await?;
    if challenge.enable_traffic_capture {
        crate::services::traffic::start_container_capture(&st, &backend_id).await?;
    }
    crate::services::ad_vpn::reconcile_for_deployment(&st.db).await?;
    Ok(MessageResponse::ok(""))
}

/// `GET /api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshot` — admin
/// forensics download of ANY team's service container snapshot tarball
/// (`Api.ts` `editAdSnapshotUrl`).
///
/// Port of `AdAdminController.DownloadSnapshot`: unlike the player endpoint
/// (`game::ad::download_snapshot`) this is NOT team-scoped — a game admin pulls
/// any team's snapshot — and it drops the post-game gate and the
/// `AdAllowSnapshotDownload` policy check (forensics may run mid-game). Same
/// production model as the player side: rsctf keeps no snapshot-blob column, so
/// the tarball is produced on demand by `docker export` of the live service
/// container (an uncompressed TAR of its current filesystem). BYOC self-hosted
/// services expose only a tunnel relay, not the team's box — refuse rather than
/// leak relay internals (RSCTF has no snapshot blob for them either).
pub async fn ad_download_snapshot(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, ats_id)): Path<(i32, i32)>,
) -> AppResult<Response> {
    manager_or_admin(&st, &user, game_id).await?;
    let svc = ad_team_service::Entity::find_by_id(ats_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == game_id)
        .ok_or_else(|| AppError::not_found("Service not found"))?;

    let challenge = game_challenge::Entity::find_by_id(svc.challenge_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if challenge.ad_self_hosted {
        return Err(AppError::not_found(
            "Self-hosted (BYOC) service has no platform snapshot",
        ));
    }

    let Some(cid) = svc.container_id.as_deref().filter(|c| !c.is_empty()) else {
        return Err(AppError::not_found(
            "Snapshot not available (no platform container for this service)",
        ));
    };
    let tar = st.containers.export(cid).await?;
    let filename = format!(
        "ad-snapshot-team{}-challenge{}.tar",
        svc.participation_id, svc.challenge_id
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-tar".to_string()),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (header::PRAGMA, "no-cache".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        tar,
    )
        .into_response())
}

/// `GET /api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshot/Changes` ->
/// `AdSnapshotChangesModel`.
pub async fn ad_snapshot_changes(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, ats_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, game_id).await?;
    let changes = snapshot_changes_for(&st, game_id, ats_id).await?;
    Ok(RequestResponse::ok(json!({
        "snapshotAvailable": !changes.is_empty(),
        "changes": changes.iter().map(|c| json!({"path": c.path, "kind": c.kind})).collect::<Vec<_>>(),
    })))
}

/// `GET /api/edit/games/{id}/ad/Services/{adTeamServiceId}/SnapshotDiff` ->
/// `AdSnapshotTimeDiffModel`. Added/Deleted paths of the live container.
pub async fn ad_snapshot_diff(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, ats_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<JsonValue>> {
    manager_or_admin(&st, &user, game_id).await?;
    let changes = snapshot_changes_for(&st, game_id, ats_id).await?;
    let added: Vec<String> = changes
        .iter()
        .filter(|c| c.kind == "Added")
        .map(|c| c.path.clone())
        .collect();
    let removed: Vec<String> = changes
        .iter()
        .filter(|c| c.kind == "Deleted")
        .map(|c| c.path.clone())
        .collect();
    Ok(RequestResponse::ok(
        json!({ "added": added, "removed": removed }),
    ))
}

/// `GET /api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshots` ->
/// `AdSnapshotPointModel[]`. The current live snapshot (filesystem drift count).
pub async fn ad_service_snapshots(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, ats_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<Vec<JsonValue>>> {
    manager_or_admin(&st, &user, game_id).await?;
    let changes = snapshot_changes_for(&st, game_id, ats_id).await?;
    if changes.is_empty() {
        return Ok(RequestResponse::ok(Vec::new()));
    }
    Ok(RequestResponse::ok(vec![json!({
        "id": ats_id,
        "changeCount": changes.len(),
        "kind": "live",
    })]))
}

/// Filesystem changes of the service's live container (empty when it has no
/// platform-launched container or the runtime is unavailable).
async fn snapshot_changes_for(
    st: &SharedState,
    game_id: i32,
    ats_id: i32,
) -> AppResult<Vec<crate::services::container::FileChange>> {
    let svc = crate::models::data::ad_team_service::Entity::find()
        .filter(crate::models::data::ad_team_service::Column::Id.eq(ats_id))
        .filter(crate::models::data::ad_team_service::Column::GameId.eq(game_id))
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    match svc.container_id {
        Some(cid) => st.containers.snapshot_changes(&cid).await,
        None => Ok(Vec::new()),
    }
}

// ============================================================================
//  Helpers
// ============================================================================
