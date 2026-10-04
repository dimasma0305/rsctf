//! Edit-facing A&D operator console.
use crate::services::ad::koth_capability_cache::finish_game_epoch_mutation_if_any;
use axum::extract::Query;
use axum::response::IntoResponse;
use base64::Engine as _;

use super::*;

mod inspector;
mod provision;
mod provision_recovery;
mod state;
pub use inspector::*;
pub use provision::*;
pub(crate) use provision_recovery::*;
pub use state::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DesiredStateDecision {
    AlreadyCurrent,
    Transition { next_revision: i64 },
}

fn decide_desired_state(
    current: bool,
    current_revision: i64,
    desired: bool,
    expected_revision: i64,
    resource: &str,
) -> AppResult<DesiredStateDecision> {
    // A command observed at the current revision is an ordinary no-op when its
    // desired value is already authoritative. A command observed at exactly
    // the preceding revision is the one safe lost-response replay: one real
    // boolean transition necessarily produced the current value and revision.
    //
    // Do not accept older matching values. After two or more transitions the
    // same boolean can recur, but that does not make the old command a replay
    // of the latest transition.
    let exact_replay =
        desired == current && expected_revision.checked_add(1) == Some(current_revision);
    if desired == current && (expected_revision == current_revision || exact_replay) {
        return Ok(DesiredStateDecision::AlreadyCurrent);
    }
    if expected_revision != current_revision {
        return Err(AppError::conflict(format!(
            "{resource} state changed; current revision is {current_revision}"
        )));
    }
    let next_revision = current_revision
        .checked_add(1)
        .filter(|revision| *revision <= 9_007_199_254_740_991)
        .ok_or_else(|| AppError::conflict(format!("{resource} control revision is exhausted")))?;
    Ok(DesiredStateDecision::Transition { next_revision })
}

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

/// Human label for a stored `AdCheckStatus` numeric (matches the `AdCheckStatus`
/// string enum the React console keys its status colours off of).
pub(super) fn ad_check_status_label(status: i16) -> &'static str {
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdScoringDesiredState {
    pub paused: bool,
    pub revision: i64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdScoringCommandResult {
    pub scoring_paused: bool,
    pub revision: i64,
}

/// Set the explicit scoring state under an optimistic revision fence. Replays of
/// an already-applied intent are side-effect-free, so a lost response can never
/// turn a retry into the opposite transition.
pub async fn ad_scoring_pause(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
    Json(command): Json<AdScoringDesiredState>,
) -> AppResult<RequestResponse<AdScoringCommandResult>> {
    manager_or_admin(&st, &user, game_id).await?;
    if command.revision < 1 {
        return Err(AppError::bad_request("revision must be positive"));
    }
    // Checker result persistence takes the same lock. A pass that committed first
    // stays committed; a pass already running when pause wins may still land in
    // the unchanged current round, and no new pass starts while paused.
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?;
    let tx = control.transaction_mut();
    let (was_paused, paused_at, revision): (bool, Option<DateTime<Utc>>, i64) = sqlx::query_as(
        r#"SELECT ad_scoring_paused, ad_scoring_paused_at, ad_control_revision
             FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let decision = decide_desired_state(
        was_paused,
        revision,
        command.paused,
        command.revision,
        "Scoring",
    )?;
    if decision == DesiredStateDecision::AlreadyCurrent {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(AdScoringCommandResult {
            scoring_paused: was_paused,
            revision,
        }));
    }
    let DesiredStateDecision::Transition { next_revision } = decision else {
        unreachable!("already-current scoring command returned above")
    };

    // Resuming: give the live round back the time it was frozen for.
    if !command.paused {
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
                    THEN clock_timestamp() ELSE NULL END,
                  ad_control_revision = $3
            WHERE id = $1 AND ad_control_revision = $4"#,
    )
    .bind(game_id)
    .bind(command.paused)
    .bind(next_revision)
    .bind(revision)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    flush_ad_scoreboard(&st, game_id).await;

    Ok(RequestResponse::ok(AdScoringCommandResult {
        scoring_paused: command.paused,
        revision: next_revision,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdChallengeDesiredState {
    pub enabled: bool,
    pub revision: i64,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdChallengeCommandResult {
    pub is_enabled: bool,
    pub revision: i64,
}

/// Set one A&D/KotH challenge's explicit enabled state. Only the winning
/// revision performs teardown; exact replays return the authoritative result.
pub async fn ad_toggle_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
    Json(command): Json<AdChallengeDesiredState>,
) -> AppResult<RequestResponse<AdChallengeCommandResult>> {
    manager_or_admin(&st, &user, game_id).await?;
    if command.revision < 1 {
        return Err(AppError::bad_request("revision must be positive"));
    }
    // Enabled-state transitions and their slow runtime cleanup are one ordered
    // operation across replicas. The outer transition must precede the game
    // control lock, matching the general challenge update/delete path.
    let runtime_transition = crate::services::challenge_workloads::acquire_runtime_transition_lock(
        st.pg(),
        challenge_id,
    )
    .await?;
    let mut engine_control =
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, game_id).await?);
    let tx = engine_control
        .as_mut()
        .expect("engine challenge holds the game control lock")
        .transaction_mut();
    let (challenge_type, is_enabled, revision, deletion_pending): (i16, bool, i64, bool) =
        sqlx::query_as(
            r#"SELECT "Type", is_enabled, ad_control_revision, deletion_pending
                 FROM "GameChallenges"
                WHERE id = $1 AND game_id = $2
                FOR UPDATE"#,
        )
        .bind(challenge_id)
        .bind(game_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if challenge_type != ChallengeType::AttackDefense as i16
        && challenge_type != ChallengeType::KingOfTheHill as i16
    {
        return Err(AppError::bad_request("Not an A&D / KotH challenge"));
    }
    if deletion_pending {
        return Err(AppError::conflict("Challenge is being deleted"));
    }
    let decision = decide_desired_state(
        is_enabled,
        revision,
        command.enabled,
        command.revision,
        "Challenge",
    )?;
    if decision == DesiredStateDecision::AlreadyCurrent {
        engine_control
            .take()
            .expect("engine control lock exists")
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        runtime_transition
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(AdChallengeCommandResult {
            is_enabled,
            revision,
        }));
    }
    let DesiredStateDecision::Transition { next_revision } = decision else {
        unreachable!("already-current challenge command returned above")
    };
    let cache_mutation = if challenge_type == ChallengeType::KingOfTheHill as i16 {
        Some(
            crate::services::ad::koth_capability_cache::begin_game_epoch_mutation(
                st.cache.as_ref(),
                game_id,
            )
            .await?,
        )
    } else {
        None
    };
    let toggled = match sqlx::query(
        r#"UPDATE "GameChallenges"
              SET is_enabled = $3, ad_control_revision = $4
            WHERE id = $1 AND game_id = $2
              AND deletion_pending = FALSE AND ad_control_revision = $5"#,
    )
    .bind(challenge_id)
    .bind(game_id)
    .bind(command.enabled)
    .bind(next_revision)
    .bind(revision)
    .execute(&mut **tx)
    .await
    {
        Ok(result) => result.rows_affected(),
        Err(error) => {
            finish_game_epoch_mutation_if_any(st.cache.as_ref(), game_id, cache_mutation).await;
            return Err(AppError::internal(error.to_string()));
        }
    };
    if toggled != 1 {
        finish_game_epoch_mutation_if_any(st.cache.as_ref(), game_id, cache_mutation).await;
        return Err(AppError::conflict("Challenge is being deleted"));
    }
    if !command.enabled && challenge_type == ChallengeType::KingOfTheHill as i16 {
        if let Err(error) = sqlx::query(
            r#"UPDATE "KothTargets"
                  SET holder_participation_id = NULL, held_since = NULL
                WHERE game_id = $1 AND challenge_id = $2"#,
        )
        .bind(game_id)
        .bind(challenge_id)
        .execute(&mut **tx)
        .await
        {
            finish_game_epoch_mutation_if_any(st.cache.as_ref(), game_id, cache_mutation).await;
            return Err(AppError::internal(error.to_string()));
        }
    }
    if let Some(lock) = engine_control {
        lock.release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    // Commit acknowledgement is the publication point. An uncertain commit
    // deliberately leaves the marker fail-closed; known rollback paths above
    // restore the epoch immediately.
    finish_game_epoch_mutation_if_any(st.cache.as_ref(), game_id, cache_mutation).await;
    // Both A&D and KotH challenge membership feeds the shared epoch surfaces.
    // Flush after either engine-backed toggle so KotH eligibility and board
    // caches do not wait for their TTL on the writer replica.
    flush_ad_scoreboard(&st, game_id).await;
    let challenge = game_challenge::Entity::find_by_id(challenge_id)
        .one(&st.db)
        .await?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    if !command.enabled {
        st.byoc.disconnect_challenge(&st.db, challenge_id).await?;
        let _ =
            crate::controllers::edit::destroy_challenge_containers(&st, &challenge, true, false)
                .await;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    runtime_transition
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if command.enabled {
        // Enabling changes the desired topology. Admission is event-scoped and
        // coalesces with the scheduler/manual ensure owner before any grid scan.
        request_ad_reconcile_job(&st, game_id, true, true).await?;
    }

    Ok(RequestResponse::ok(AdChallengeCommandResult {
        is_enabled: command.enabled,
        revision: next_revision,
    }))
}

#[cfg(test)]
mod desired_state_tests {
    use super::{decide_desired_state, DesiredStateDecision};

    #[test]
    fn exact_replay_is_a_noop_even_after_revision_advanced() {
        assert_eq!(
            decide_desired_state(true, 8, true, 7, "resource").unwrap(),
            DesiredStateDecision::AlreadyCurrent
        );
    }

    #[test]
    fn current_state_noop_requires_the_current_revision() {
        assert_eq!(
            decide_desired_state(true, 8, true, 8, "resource").unwrap(),
            DesiredStateDecision::AlreadyCurrent
        );
    }

    #[test]
    fn matching_value_from_an_older_generation_is_not_a_replay() {
        let error = decide_desired_state(true, 10, true, 7, "resource").unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn future_revision_is_rejected_even_when_the_value_matches() {
        let error = decide_desired_state(true, 8, true, 9, "resource").unwrap_err();
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
    }

    #[test]
    fn stale_opposite_intent_is_rejected() {
        assert!(decide_desired_state(true, 8, false, 7, "resource").is_err());
    }

    #[test]
    fn matching_revision_advances_once() {
        assert_eq!(
            decide_desired_state(false, 8, true, 8, "resource").unwrap(),
            DesiredStateDecision::Transition { next_revision: 9 }
        );
    }

    #[test]
    fn javascript_safe_revision_limit_is_enforced() {
        assert!(decide_desired_state(
            false,
            9_007_199_254_740_991,
            true,
            9_007_199_254_740_991,
            "resource"
        )
        .is_err());
    }
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
/// Inspect one file inside a team's service container through the runtime's
/// bounded archive API. This deliberately never launches a participant-owned
/// process, so FIFOs and devices cannot strand an exec after cancellation. The
/// image `baseline` + `unifiedDiff` need an offline image read
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
    crate::services::ad::forensics::validate_path(&q.path)?;
    let svc = live_forensics_service(&st, game_id, ats_id).await?;

    let container_running = svc.container_id.is_some();
    let current = match svc.container_id.as_deref() {
        Some(cid) if !svc.self_hosted => {
            let _permit = crate::services::ad::forensics::acquire(
                st.pg(),
                cid,
                crate::services::ad::forensics::ForensicsWork::File,
            )
            .await?;
            let file = tokio::time::timeout(
                crate::services::ad::forensics::FILE_DEADLINE,
                st.containers.read_file(
                    cid,
                    &q.path,
                    crate::services::ad::forensics::MAX_FILE_PREVIEW_BYTES,
                ),
            )
            .await
            .map_err(|_| crate::services::ad::forensics::timeout_error("file read"))??;
            file_preview_json(file)
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

#[derive(sqlx::FromRow)]
struct LiveForensicsService {
    container_id: Option<String>,
    self_hosted: bool,
}

async fn live_forensics_service(
    st: &SharedState,
    game_id: i32,
    ats_id: i32,
) -> AppResult<LiveForensicsService> {
    sqlx::query_as(
        r#"SELECT NULLIF(service.container_id, '') AS container_id,
                  challenge.ad_self_hosted AS self_hosted
             FROM "AdTeamServices" service
             JOIN "GameChallenges" challenge
               ON challenge.id = service.challenge_id
              AND challenge.game_id = service.game_id
            WHERE service.id = $1 AND service.game_id = $2"#,
    )
    .bind(ats_id)
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Service not found"))
}

fn file_preview_json(file: crate::services::container::ContainerFile) -> JsonValue {
    let (text, binary_bytes) = match String::from_utf8(file.bytes) {
        Ok(text)
            if text.chars().all(|character| {
                !character.is_control() || matches!(character, '\n' | '\r' | '\t')
            }) =>
        {
            (Some(text), None)
        }
        Ok(text) => (None, Some(text.into_bytes())),
        Err(error) => (None, Some(error.into_bytes())),
    };
    let binary = binary_bytes.is_some();
    json!({
        "size": file.size,
        "truncated": file.truncated,
        "binary": binary,
        "text": text,
        "base64": binary_bytes.map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
    })
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
    headers: axum::http::HeaderMap,
) -> AppResult<(
    axum::http::StatusCode,
    RequestResponse<crate::services::control_jobs::ControlJobModel>,
)> {
    manager_or_admin(&st, &user, game_id).await?;
    let service = ad_team_service::Entity::find_by_id(ats_id)
        .one(&st.db)
        .await?
        .filter(|service| service.game_id == game_id)
        .ok_or_else(|| AppError::not_found("Service not found"))?;
    let operation_id = super::control_jobs::operation_id(&headers)?;
    let input = serde_json::json!({
        "serviceId": service.id,
        "participationId": service.participation_id,
        "expectedBackendId": service.container_id,
        "playerPolicy": false,
    });
    let fingerprint = super::control_jobs::fingerprint(&input)?;
    let job = crate::services::control_jobs::enqueue(
        st.pg(),
        crate::services::control_jobs::ControlJobKind::AdReset,
        &format!("ad-service:{}", service.id),
        game_id,
        Some(service.challenge_id),
        operation_id,
        &fingerprint,
        input,
    )
    .await?;
    crate::services::control_jobs::kick(st);
    Ok((axum::http::StatusCode::ACCEPTED, RequestResponse::ok(job)))
}

/// `GET /api/edit/games/{id}/ad/Services/{adTeamServiceId}/Snapshot` — admin
/// forensics download of ANY team's compressed service filesystem snapshot
/// (`Api.ts` `editAdSnapshotUrl`).
///
/// Port of `AdAdminController.DownloadSnapshot`: unlike the player endpoint
/// (`game::ad::download_snapshot`) this is NOT team-scoped. A game admin may
/// download the retained final blob, or export a still-running hosted service
/// for live forensics. BYOC self-hosted services expose only a tunnel relay, not
/// the team's box — refuse rather than leak relay internals.
pub async fn ad_download_snapshot(
    State(st): State<SharedState>,
    user: CurrentUser,
    headers: axum::http::HeaderMap,
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

    if let Some(snapshot) =
        crate::services::blob_refs::load_service_snapshot(st.pg(), svc.id).await?
    {
        let grant = crate::controllers::game::ad::snapshot_download::SnapshotResponseGrant {
            team_service_id: svc.id,
            snapshot_id: snapshot.id,
            hash: snapshot.hash,
            filename: snapshot.name,
            file_size: snapshot.file_size,
        };
        let prepared =
            match crate::controllers::game::ad::snapshot_download::prepare_snapshot_stream(
                &st, &headers, &grant,
            )
            .await?
            {
                crate::controllers::game::ad::snapshot_download::SnapshotPreparation::Ready(
                    prepared,
                ) => prepared,
                crate::controllers::game::ad::snapshot_download::SnapshotPreparation::Response(
                    response,
                ) => return Ok(response),
            };
        // Storage may be slow. Revalidate both operator authority and the exact
        // retained relation after opening the immutable stream.
        manager_or_admin(&st, &user, game_id).await?;
        let current = crate::services::blob_refs::load_service_snapshot(st.pg(), svc.id).await?;
        let service_available: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(
                   SELECT 1
                     FROM "AdTeamServices" service
                     JOIN "GameChallenges" challenge
                       ON challenge.id = service.challenge_id
                      AND challenge.game_id = service.game_id
                    WHERE service.id = $1
                      AND service.game_id = $2
                      AND challenge.ad_self_hosted = FALSE
                      AND challenge.deletion_pending = FALSE
               )"#,
        )
        .bind(svc.id)
        .bind(game_id)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if !service_available
            || current.as_ref().is_none_or(|current| {
                current.id != grant.snapshot_id
                    || current.hash != grant.hash
                    || current.name != grant.filename
                    || current.file_size != grant.file_size
            })
        {
            return Err(AppError::not_found("Snapshot is no longer available"));
        }
        return prepared.into_response(&grant.filename);
    }

    let Some(cid) = svc.container_id.as_deref().filter(|c| !c.is_empty()) else {
        return Err(AppError::not_found(
            "Snapshot not available for this service",
        ));
    };
    let permit = match st
        .bulk_export_admission
        .try_acquire(
            std::sync::Arc::clone(&st.cache),
            crate::services::ad::snapshots::MAX_STORED_SNAPSHOT_BYTES,
        )
        .await
    {
        Ok(permit) => std::sync::Arc::new(permit),
        Err(_) => return Ok(crate::services::bulk_export::overload_response()),
    };
    let archive =
        crate::services::ad::snapshots::export_archive(st.containers.as_ref(), cid).await?;
    let filename =
        crate::services::ad::snapshots::archive_name(svc.participation_id, svc.challenge_id);
    let archive_len = archive.len();
    Ok((
        [
            (
                header::CONTENT_TYPE,
                crate::services::ad::snapshots::SNAPSHOT_CONTENT_TYPE.to_string(),
            ),
            (header::CONTENT_LENGTH, archive_len.to_string()),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
            (header::PRAGMA, "no-cache".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        crate::services::bulk_export::permitted_bytes_body(archive, permit),
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
    let persisted = crate::services::blob_refs::load_service_snapshot(st.pg(), ats_id).await?;
    let live_service = live_forensics_service(&st, game_id, ats_id).await?;
    let live = live_service.container_id.is_some() && !live_service.self_hosted;
    Ok(RequestResponse::ok(json!({
        "snapshotAvailable": persisted.is_some() || changes.observed > 0,
        "live": live,
        "changes": changes.changes.iter().map(|c| json!({
            "path": c.path,
            "kind": change_kind_number(&c.kind),
        })).collect::<Vec<_>>(),
        "observedChanges": changes.observed,
        "truncated": changes.truncated,
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
    let added: Vec<JsonValue> = changes
        .changes
        .iter()
        .filter(|c| c.kind == "Added")
        .map(|c| json!({ "path": c.path, "kind": 1 }))
        .collect();
    let removed: Vec<JsonValue> = changes
        .changes
        .iter()
        .filter(|c| c.kind == "Deleted")
        .map(|c| json!({ "path": c.path, "kind": 2 }))
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
    if changes.changes.is_empty() {
        return Ok(RequestResponse::ok(Vec::new()));
    }
    Ok(RequestResponse::ok(vec![json!({
        "id": ats_id,
        "changeCount": changes.changes.len(),
        "observedChangeCount": changes.observed,
        "truncated": changes.truncated,
        "kind": "live",
    })]))
}

/// Filesystem changes of the service's live container (empty when it has no
/// platform-launched container or the runtime is unavailable).
async fn snapshot_changes_for(
    st: &SharedState,
    game_id: i32,
    ats_id: i32,
) -> AppResult<std::sync::Arc<crate::services::ad::forensics::BoundedChanges>> {
    let svc = live_forensics_service(st, game_id, ats_id).await?;
    if svc.self_hosted {
        return Ok(std::sync::Arc::new(
            crate::services::ad::forensics::bound_changes(Vec::new()),
        ));
    }
    let Some(cid) = svc.container_id else {
        return Ok(std::sync::Arc::new(
            crate::services::ad::forensics::bound_changes(Vec::new()),
        ));
    };
    if let Some(cached) = crate::services::ad::forensics::cached_changes(&cid) {
        return Ok(cached);
    }
    let _permit = crate::services::ad::forensics::acquire(
        st.pg(),
        &cid,
        crate::services::ad::forensics::ForensicsWork::Changes,
    )
    .await?;
    let changes = tokio::time::timeout(
        crate::services::ad::forensics::CHANGE_DEADLINE,
        st.containers.snapshot_changes(&cid),
    )
    .await
    .map_err(|_| crate::services::ad::forensics::timeout_error("change scan"))??;
    let bounded = std::sync::Arc::new(crate::services::ad::forensics::bound_changes(changes));
    crate::services::ad::forensics::cache_changes(&cid, std::sync::Arc::clone(&bounded));
    Ok(bounded)
}

fn change_kind_number(kind: &str) -> i32 {
    match kind {
        "Added" => 1,
        "Deleted" => 2,
        _ => 0,
    }
}

// ============================================================================
//  Helpers
// ============================================================================
