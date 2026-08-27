//! KotH operator-state endpoint.

use super::*;

pub(super) async fn require_game_admin(
    st: &SharedState,
    user: &CurrentUser,
    game_id: i32,
) -> AppResult<()> {
    if user.is_admin() {
        return Ok(());
    }
    let is_manager = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
             SELECT 1 FROM "GameManagers"
              WHERE game_id = $1 AND user_id = $2
           )"#,
    )
    .bind(game_id)
    .bind(user.id)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    is_manager.then_some(()).ok_or(AppError::Forbidden)
}

async fn load_cycle_champions(
    st: &SharedState,
    game_id: i32,
) -> AppResult<std::collections::HashMap<i32, Vec<KothCycleChampion>>> {
    let rows = sqlx::query_as::<_, (i32, i32, i32, String, i64)>(
        r#"WITH latest_cycle AS (
             SELECT DISTINCT ON (challenge_id)
                    id, challenge_id, cycle_number, champion_participation_id
               FROM "KothCrownCycles"
              WHERE game_id = $1
              ORDER BY challenge_id, cycle_number DESC
           ), candidate AS (
             SELECT cycle.challenge_id,
                    cycle.cycle_number - 1 AS source_cycle_number,
                    champion.value::integer AS participation_id,
                    GREATEST(COALESCE(NULLIF(
                      audit.receipt->>'leadHealthyControlledTicks', ''
                    ), '0')::bigint, 0) AS healthy_ticks
               FROM latest_cycle cycle
               JOIN "KothCycleAuditReceipts" audit
                 ON audit.cycle_id = cycle.id
                AND audit.phase = 'FinalizePending'
              CROSS JOIN LATERAL jsonb_array_elements_text(
                COALESCE(audit.receipt->'championParticipationIds', '[]'::jsonb)
              ) champion(value)
             UNION ALL
             SELECT cycle.challenge_id, cycle.cycle_number - 1,
                    cooldown.participation_id,
                    cooldown.lead_healthy_controlled_ticks::bigint
               FROM latest_cycle cycle
               JOIN "KothCycleCooldowns" cooldown ON cooldown.cycle_id = cycle.id
             UNION ALL
             SELECT cycle.challenge_id, cycle.cycle_number - 1,
                    cycle.champion_participation_id, 0::bigint
               FROM latest_cycle cycle
              WHERE cycle.cycle_number > 1
                AND cycle.champion_participation_id IS NOT NULL
           )
           SELECT candidate.challenge_id, candidate.source_cycle_number,
                  participation.id,
                  team.name, MAX(candidate.healthy_ticks)::bigint
             FROM candidate
             JOIN "Participations" participation
               ON participation.id = candidate.participation_id
              AND participation.game_id = $1
             JOIN "Teams" team ON team.id = participation.team_id
            WHERE candidate.source_cycle_number >= 1
            GROUP BY candidate.challenge_id, candidate.source_cycle_number,
                     participation.id, team.name
            ORDER BY candidate.challenge_id, participation.id"#,
    )
    .bind(game_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut champions = std::collections::HashMap::<i32, Vec<KothCycleChampion>>::new();
    for (
        challenge_id,
        source_cycle_number,
        participation_id,
        team_name,
        healthy_controlled_ticks,
    ) in rows
    {
        champions
            .entry(challenge_id)
            .or_default()
            .push(KothCycleChampion {
                source_cycle_number,
                participation_id,
                team_name,
                healthy_controlled_ticks,
            });
    }
    Ok(champions)
}

#[derive(Debug, sqlx::FromRow)]
struct ObserverAdminRow {
    challenge_id: i32,
    claim_source: String,
    configured: bool,
    secret_hint: Option<String>,
    last_observation_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn load_observer_admin_state(
    st: &SharedState,
    game_id: i32,
) -> AppResult<std::collections::HashMap<i32, ObserverAdminRow>> {
    let rows = sqlx::query_as::<_, ObserverAdminRow>(
        r#"SELECT challenge.id AS challenge_id,
                  CASE
                    WHEN config.game_id IS NOT NULL
                      THEN COALESCE(NULLIF(frozen.item->>'claimSource', ''), 'Marker')
                    WHEN observer.challenge_id IS NOT NULL THEN 'Api'
                    ELSE 'Marker'
                  END AS claim_source,
                  observer.challenge_id IS NOT NULL AS configured,
                  observer.secret_hint,
                  snapshot.accepted_at AS last_observation_at
             FROM "GameChallenges" challenge
             LEFT JOIN "KothOfficialConfigs" config
               ON config.game_id = challenge.game_id
             LEFT JOIN LATERAL (
               SELECT item
                 FROM jsonb_array_elements(config.hills_snapshot) item
                WHERE (item->>'challengeId')::integer = challenge.id
                LIMIT 1
             ) frozen ON TRUE
             LEFT JOIN "KothApiObservers" observer
               ON observer.game_id = challenge.game_id
              AND observer.challenge_id = challenge.id
             LEFT JOIN LATERAL (
               SELECT snapshot.accepted_at
                 FROM "KothTargets" target
                 JOIN "KothApiSnapshots" snapshot ON snapshot.target_id = target.id
                WHERE target.game_id = challenge.game_id
                  AND target.challenge_id = challenge.id
                ORDER BY snapshot.accepted_at DESC, snapshot.target_id DESC
                LIMIT 1
             ) snapshot ON TRUE
            WHERE challenge.game_id = $1 AND challenge."Type" = $2"#,
    )
    .bind(game_id)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|row| (row.challenge_id, row))
        .collect())
}

/// `GET /api/edit/games/{id}/ad/koth/state` — the admin KotH operator console:
/// every hill (enabled and disabled) with its container address + current king,
/// plus the game's KotH tunables and the same ranked team rows as the board.
pub async fn admin_state(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(game_id): Path<i32>,
) -> AppResult<RequestResponse<AdminKothStateModel>> {
    // Game-admin gate (mirrors `edit::manager_or_admin`): platform admin, or a
    // co-manager of this game.
    require_game_admin(&st, &user, game_id).await?;

    let game = super::super::load_game_cached(&st, game_id).await?;
    let (scoring, board) = tokio::try_join!(
        build_koth_scoreboard_cached(&st, &game, true),
        compute_koth_admin_hill_state(&st, game_id)
    )?;
    let mut lifecycle = load_lifecycle_map(&st, game_id, board.latest_round, None).await?;
    let mut cycle_champions = load_cycle_champions(&st, game_id).await?;
    let mut observer_state = load_observer_admin_state(&st, game_id).await?;

    // The admin console shows every hill, including disabled ones.
    let all: Vec<&KothHillInfo> = board.hills.iter().collect();

    let hills: Vec<AdminKothHill> = all
        .iter()
        .map(|h| {
            let view = lifecycle.remove(&h.challenge_id).unwrap_or_default();
            let observer = observer_state.remove(&h.challenge_id);
            AdminKothHill {
                challenge_id: h.challenge_id,
                title: h.title.clone(),
                is_enabled: h.is_enabled,
                // Raw docker id of the shared hill container — the admin exec hub
                // accepts it directly (KotH containers aren't in the `container` table).
                container_guid: h.container_id.clone().filter(|c| !c.is_empty()),
                container_ip: h.container_ip.clone(),
                container_port: h.container_port,
                last_check_status: board
                    .latest_control_by_challenge
                    .get(&h.challenge_id)
                    .map(|(s, _)| s.clone()),
                current_holder_team_name: board
                    .holder_team_name_by_challenge
                    .get(&h.challenge_id)
                    .cloned(),
                current_holder_participation_id: board
                    .holder_by_challenge
                    .get(&h.challenge_id)
                    .copied(),
                provisional_claimant_team_name: view.provisional_team_name,
                provisional_claimant_participation_id: view.provisional_participation_id,
                provisional_confirmation_ticks: view.confirmation_progress,
                cycle_number: view.cycle_number,
                cycle_tick: view.cycle_tick,
                durable_phase: view.durable_phase,
                reset_phase: view.reset_phase,
                is_scorable: view.is_scorable,
                next_reset_ticks: view.next_reset_ticks,
                cooldown_participants: view.cooldown_participants,
                cycle_champions: cycle_champions.remove(&h.challenge_id).unwrap_or_default(),
                old_container_id: view.old_container_id,
                replacement_container_id: view.replacement_container_id,
                reset_attempt: view.reset_attempt,
                readiness_failure_count: view.readiness_failures,
                last_readiness_error: view.readiness_error,
                can_retry: view.can_retry,
                reset_receipt_id: view.reset_receipt_id,
                scoring_receipt_id: view.scoring_receipt_id,
                claim_source: observer
                    .as_ref()
                    .map(|state| state.claim_source.clone())
                    .unwrap_or_else(|| "Marker".to_string()),
                api_observer_configured: observer.as_ref().is_some_and(|state| state.configured),
                api_observer_secret_hint: observer
                    .as_ref()
                    .and_then(|state| state.secret_hint.clone()),
                api_last_observation_at: observer.and_then(|state| state.last_observation_at),
            }
        })
        .collect();

    Ok(RequestResponse::ok(AdminKothStateModel {
        epoch_ticks: game.koth_epoch_ticks,
        cycle_ticks: game.koth_cycle_ticks,
        champion_cooldown_ticks: game.koth_champion_cooldown_ticks,
        claim_confirmation_ticks: game.koth_claim_confirmation_ticks,
        tick_seconds: board.tick_seconds,
        scoring_generated_at: scoring.generated_at,
        latest_round: scoring.latest_round,
        current_round_ends_at: scoring.current_round_ends_at,
        scoring_paused: game.ad_scoring_paused,
        scoring_paused_at: game.ad_scoring_paused_at,
        hills,
        teams: scoring.teams,
    }))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverKothHillModel {
    challenge_id: i32,
    cycle_number: i32,
    reset_phase: String,
}

/// Re-enter the latest durable phase for one hill. The lifecycle advisory lock,
/// phase CAS, and deterministic container operation identity make repeated
/// clicks and concurrent replicas safe.
pub async fn recover_hill(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<RecoverKothHillModel>> {
    require_game_admin(&st, &user, game_id).await?;
    crate::services::ad_engine::koth_cycle::require_recovery_owner(st.config.runtime_role)?;
    require_live_hill(&st, game_id, challenge_id).await?;
    let (cycle_number, phase) =
        crate::services::ad_engine::koth_cycle::recover_cycle(&st, game_id, challenge_id).await?;
    st.cache.remove(&format!("_KothScoreBoard_{game_id}")).await;
    st.cache
        .remove(&format!("_KothHillState_{game_id}_{challenge_id}"))
        .await;
    crate::controllers::game::invalidate_combined_scoreboard(&st, game_id).await;
    super::invalidate_live_lifecycle_cache(st.cache.as_ref(), game_id).await;
    Ok(RequestResponse::ok(RecoverKothHillModel {
        challenge_id,
        cycle_number,
        reset_phase: super::lifecycle::wire_phase(Some(&phase)),
    }))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AdminKothAuditReceipt {
    id: i64,
    phase: String,
    attempt: i32,
    receipt: serde_json::Value,
    filesystem_diff: Option<serde_json::Value>,
    #[serde(with = "crate::utils::datetime::millis")]
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminKothAuditReceiptsModel {
    challenge_id: i32,
    cycle_number: i32,
    receipts: Vec<AdminKothAuditReceipt>,
}

/// Return the bounded audit trail for the latest durable cycle. Filesystem
/// diffs are intentionally loaded on demand instead of bloating the admin
/// state poll that runs every five seconds.
pub async fn audit_receipts(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((game_id, challenge_id)): Path<(i32, i32)>,
) -> AppResult<RequestResponse<AdminKothAuditReceiptsModel>> {
    require_game_admin(&st, &user, game_id).await?;
    require_live_hill(&st, game_id, challenge_id).await?;
    let (cycle_id, cycle_number): (i64, i32) = sqlx::query_as(
        r#"SELECT id, cycle_number FROM "KothCrownCycles"
            WHERE game_id = $1 AND challenge_id = $2
            ORDER BY cycle_number DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH lifecycle audit trail not found"))?;
    let receipts = sqlx::query_as::<_, AdminKothAuditReceipt>(
        r#"SELECT id, phase, attempt, receipt, filesystem_diff, created_at
             FROM "KothCycleAuditReceipts"
            WHERE cycle_id = $1
            ORDER BY id DESC LIMIT 24"#,
    )
    .bind(cycle_id)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(AdminKothAuditReceiptsModel {
        challenge_id,
        cycle_number,
        receipts,
    }))
}
