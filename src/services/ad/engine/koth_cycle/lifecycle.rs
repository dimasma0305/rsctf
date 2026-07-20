use chrono::Utc;
use serde_json::json;

use crate::app_state::SharedState;
use crate::services::container::ContainerSpec;
use crate::utils::enums::ChallengeType;
use crate::utils::error::{AppError, AppResult};

use super::{cycle_position, select_cycle_champions, CrownPhase};
mod audit;
mod capability;
mod data;
mod deadline;
mod readiness;

use capability::mint_capabilities;
#[cfg(test)]
use capability::{rotate_capability_window, CapabilityWindow};
pub(super) use data::OfficialConfig;
use data::{load_config, load_cycle, load_hill_spec, CycleRow};

async fn set_phase(
    connection: &mut sqlx::PgConnection,
    cycle_id: i64,
    expected: CrownPhase,
    next: CrownPhase,
) -> AppResult<bool> {
    let affected = sqlx::query(
        r#"UPDATE "KothCrownCycles"
              SET phase = $3, updated_at = clock_timestamp(), last_error = NULL
            WHERE id = $1 AND phase = $2"#,
    )
    .bind(cycle_id)
    .bind(expected.as_str())
    .bind(next.as_str())
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    Ok(affected == 1)
}

async fn record_receipt(
    connection: &mut sqlx::PgConnection,
    cycle: &CycleRow,
    phase: CrownPhase,
    receipt: serde_json::Value,
    filesystem_diff: Option<serde_json::Value>,
) -> AppResult<()> {
    // The caller commits this with the phase mutation. Selecting the parent in
    // the same statement also turns a concurrent game/challenge deletion into
    // a harmless no-op instead of an orphaned-receipt foreign-key failure.
    sqlx::query(
        r#"INSERT INTO "KothCycleAuditReceipts"
             (cycle_id, phase, attempt, receipt, filesystem_diff)
           SELECT parent.id, $2, $3, $4, $5
             FROM "KothCrownCycles" parent
            WHERE parent.id = $1
           ON CONFLICT (cycle_id, phase, attempt) DO NOTHING"#,
    )
    .bind(cycle.id)
    .bind(phase.as_str())
    .bind(cycle.reset_attempt.max(0))
    .bind(receipt)
    .bind(filesystem_diff)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

async fn create_or_load_cycle(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
    cycle_number: i32,
    epoch: i32,
    planned_start_round: i32,
    planned_end_round: i32,
) -> AppResult<i64> {
    let id = sqlx::query_scalar::<_, i64>(
        r#"INSERT INTO "KothCrownCycles"
             (game_id, challenge_id, cycle_number, epoch,
              planned_start_round, planned_end_round, phase,
              old_container_id, expected_image, reset_started_at, reset_attempt)
           SELECT $1, challenge.id, $3, $4, $5, $6, 'FinalizePending',
                  target.container_id, frozen.item->>'image',
                  clock_timestamp(), 1
             FROM "GameChallenges" challenge
             JOIN "KothTargets" target
               ON target.game_id = challenge.game_id
              AND target.challenge_id = challenge.id
             JOIN "KothOfficialConfigs" config
               ON config.game_id = challenge.game_id
             JOIN LATERAL jsonb_array_elements(config.hills_snapshot) frozen(item)
               ON (frozen.item->>'challengeId')::integer = challenge.id
            WHERE challenge.game_id = $1 AND challenge.id = $2
              AND challenge."Type" = $7
              AND NULLIF(BTRIM(frozen.item->>'image'), '') IS NOT NULL
           ON CONFLICT (game_id, challenge_id, cycle_number)
           DO UPDATE SET
             old_container_id = CASE
               WHEN "KothCrownCycles".phase = 'FinalizePending'
                 THEN EXCLUDED.old_container_id
               ELSE "KothCrownCycles".old_container_id END,
             updated_at = "KothCrownCycles".updated_at
           RETURNING id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(cycle_number)
    .bind(epoch)
    .bind(planned_start_round)
    .bind(planned_end_round)
    .bind(ChallengeType::KingOfTheHill as i16)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| {
        AppError::bad_request(
            "Crown-cycle KotH requires exactly one platform-hosted target per hill",
        )
    })?;
    Ok(id)
}

async fn finalize_previous_cycle(
    st: &SharedState,
    config: &OfficialConfig,
    cycle: &CycleRow,
    round_number: i32,
) -> AppResult<()> {
    let previous_id: Option<i64> = sqlx::query_scalar(
        r#"SELECT id FROM "KothCrownCycles"
            WHERE game_id = $1 AND challenge_id = $2
              AND cycle_number = $3"#,
    )
    .bind(cycle.game_id)
    .bind(cycle.challenge_id)
    .bind(cycle.cycle_number - 1)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut evidence = Vec::<(i32, i64)>::new();
    if let Some(previous_id) = previous_id {
        evidence = sqlx::query_as(
            r#"SELECT confirmed_participation_id, COUNT(*)::bigint
                 FROM "KothControlResults"
                WHERE cycle_id = $1 AND is_scorable = TRUE AND status = 0
                  AND confirmed_participation_id IS NOT NULL
                  AND controlling_participation_id = confirmed_participation_id
                GROUP BY confirmed_participation_id
                ORDER BY confirmed_participation_id"#,
        )
        .bind(previous_id)
        .fetch_all(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let champions = select_cycle_champions(&config.roster, &evidence);
    let lead = champions
        .first()
        .and_then(|champion| {
            evidence
                .iter()
                .find_map(|(id, ticks)| (id == champion).then_some(*ticks))
        })
        .unwrap_or(0);

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(previous_id) = previous_id {
        sqlx::query(
            r#"UPDATE "KothCrownCycles"
                  SET phase = 'Completed',
                      actual_end_round = CASE
                        WHEN actual_start_round IS NULL THEN NULL
                        ELSE GREATEST($2, actual_start_round) END,
                      finalized_at = COALESCE(finalized_at, clock_timestamp()),
                      completed_at = COALESCE(completed_at, clock_timestamp()),
                      updated_at = clock_timestamp()
                WHERE id = $1 AND phase IN ('Active','CooldownReleasePending')"#,
        )
        .bind(previous_id)
        .bind(round_number.saturating_sub(1))
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    if config.champion_cooldown_ticks > 0 && !champions.is_empty() {
        sqlx::query(
            r#"INSERT INTO "KothCycleCooldowns"
                 (cycle_id, participation_id, lead_healthy_controlled_ticks,
                  starts_round, expires_after_round)
               SELECT $1, selected.participation_id, $3, $4, $5
                 FROM UNNEST($2::integer[]) AS selected(participation_id)
               ON CONFLICT (cycle_id, participation_id) DO NOTHING"#,
        )
        .bind(cycle.id)
        .bind(&champions)
        .bind(lead)
        .bind(round_number)
        .bind(round_number + config.champion_cooldown_ticks - 1)
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    sqlx::query(
        r#"UPDATE "KothCrownCycles"
              SET champion_participation_id = $2, phase = 'SnapshotPending',
                  finalized_at = clock_timestamp(), updated_at = clock_timestamp()
            WHERE id = $1 AND phase = 'FinalizePending'"#,
    )
    .bind(cycle.id)
    .bind(champions.first().copied())
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    record_receipt(
        &mut transaction,
        cycle,
        CrownPhase::FinalizePending,
        json!({
            "round": round_number,
            "previousCycleId": previous_id,
            "championParticipationIds": champions,
            "leadHealthyControlledTicks": lead,
        }),
        None,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn snapshot_cycle(st: &SharedState, cycle: &CycleRow) -> AppResult<()> {
    let changes = if let Some(container_id) = cycle.old_container_id.as_deref() {
        st.containers.snapshot_changes(container_id).await?
    } else {
        Vec::new()
    };
    let audit::BoundedFilesystemDiff {
        value: diff,
        summary,
    } = audit::bounded_filesystem_diff(changes)?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "KothCrownCycles"
              SET filesystem_diff = $2, phase = 'DestroyPending',
                  updated_at = clock_timestamp()
            WHERE id = $1 AND phase = 'SnapshotPending'"#,
    )
    .bind(cycle.id)
    .bind(&diff)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    record_receipt(
        &mut transaction,
        cycle,
        CrownPhase::SnapshotPending,
        json!({
            "containerId": cycle.old_container_id,
            "filesystemDiffSummary": summary,
        }),
        Some(diff),
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn destroy_old(st: &SharedState, cycle: &CycleRow) -> AppResult<()> {
    sqlx::query(
        r#"WITH cleared AS (
             UPDATE "KothTargets"
                SET host = '', port = 0, holder_participation_id = NULL,
                    held_since = NULL
              WHERE game_id = $1 AND challenge_id = $2
                AND container_id IS NOT DISTINCT FROM $3
              RETURNING id
           )
           DELETE FROM "KothClaimStates" claim
            USING cleared WHERE claim.target_id = cleared.id"#,
    )
    .bind(cycle.game_id)
    .bind(cycle.challenge_id)
    .bind(&cycle.old_container_id)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let mut invalidated_games = vec![cycle.game_id];
    if let Some(container_id) = cycle.old_container_id.as_deref() {
        invalidated_games.extend(
            crate::services::ad_vpn::stage_backend_endpoint_deactivation_retaining_identity(
                &st.db,
                container_id,
            )
            .await?,
        );
    }
    invalidated_games.sort_unstable();
    invalidated_games.dedup();
    for game_id in invalidated_games {
        crate::controllers::game::ad::invalidate_live_hill_snapshot(st, game_id).await;
    }
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    if let Some(container_id) = cycle.old_container_id.as_deref() {
        crate::services::traffic::destroy_container_after_capture_fence(st, container_id).await?;
        sqlx::query(
            r#"WITH removed AS (
                 DELETE FROM "Containers" WHERE container_id = $1 RETURNING id
               )
               UPDATE "GameChallenges" challenge
                  SET shared_container_id = NULL
                WHERE challenge.id = $2
                  AND challenge.shared_container_id IN (SELECT id FROM removed)"#,
        )
        .bind(container_id)
        .bind(cycle.challenge_id)
        .execute(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let destroyed_container_ids = cycle.old_container_id.iter().cloned().collect::<Vec<_>>();
    deadline::clear_destroyed_deadline_target(
        &mut transaction,
        cycle.game_id,
        cycle.challenge_id,
        &destroyed_container_ids,
    )
    .await?;
    set_phase(
        &mut transaction,
        cycle.id,
        CrownPhase::DestroyPending,
        CrownPhase::CreatePending,
    )
    .await?;
    record_receipt(
        &mut transaction,
        cycle,
        CrownPhase::DestroyPending,
        json!({"destroyedContainerId": cycle.old_container_id}),
        None,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn create_replacement(st: &SharedState, cycle: &CycleRow) -> AppResult<()> {
    let spec = load_hill_spec(st, cycle).await?;
    if spec.image != cycle.expected_image {
        return Err(AppError::conflict(
            "KotH challenge image changed after the cycle reset was declared",
        ));
    }
    let image = crate::services::challenge_images::validate_runtime_reference(
        &cycle.expected_image,
        st.containers.backend_kind(),
        st.config.runtime_role,
        crate::services::challenge_images::shared_docker_daemon_acknowledged(),
    )?;
    let info = st
        .containers
        .create(ContainerSpec {
            game_kind: rsctf_worker_protocol::GameKind::KingOfTheHill,
            image,
            memory_limit: spec.memory_limit,
            cpu_count: spec.cpu_count,
            expose_port: spec.expose_port,
            env: Vec::new(),
            flag: None,
            ad_network: Some(crate::services::ad_vpn::services_network()),
            allow_egress: spec.allow_egress,
            operation_id: Some(format!(
                "koth-cycle:{}:attempt:{}",
                cycle.id, cycle.reset_attempt
            )),
        })
        .await?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "KothCrownCycles"
              SET replacement_container_id = $2, replacement_host = $3,
                  replacement_port = $4, phase = 'PublishPending',
                  updated_at = clock_timestamp()
            WHERE id = $1 AND phase = 'CreatePending'
              AND replacement_container_id IS NULL"#,
    )
    .bind(cycle.id)
    .bind(&info.id)
    .bind(&info.ip)
    .bind(info.port)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    record_receipt(
        &mut transaction,
        cycle,
        CrownPhase::CreatePending,
        json!({"replacementContainerId": info.id, "image": cycle.expected_image}),
        None,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))
}

async fn publish_replacement(st: &SharedState, cycle: &CycleRow) -> AppResult<()> {
    let container_id = cycle
        .replacement_container_id
        .as_deref()
        .ok_or_else(|| AppError::internal("replacement container identity is missing"))?;
    let host = cycle
        .replacement_host
        .as_deref()
        .ok_or_else(|| AppError::internal("replacement host is missing"))?;
    let port = cycle
        .replacement_port
        .ok_or_else(|| AppError::internal("replacement port is missing"))?;
    let mut control = super::super::koth_auth::acquire_game_lock(&st.db, cycle.game_id).await?;
    let published = sqlx::query(
        r#"UPDATE "KothTargets" target
              SET host = $3, port = $4, container_id = $5,
                  holder_participation_id = NULL, held_since = NULL
             FROM "KothCrownCycles" cycle
            WHERE target.game_id = $1 AND target.challenge_id = $2
              AND target.container_id IS NULL
              AND cycle.id = $6 AND cycle.phase = 'PublishPending'
              AND cycle.replacement_container_id = $5"#,
    )
    .bind(cycle.game_id)
    .bind(cycle.challenge_id)
    .bind(host)
    .bind(port)
    .bind(container_id)
    .bind(cycle.id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if published != 1 {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Err(AppError::conflict(
            "KotH target ownership changed during replacement publication",
        ));
    }
    sqlx::query(
        r#"UPDATE "KothCrownCycles" SET phase = 'CapabilityPending',
                  updated_at = clock_timestamp()
            WHERE id = $1 AND phase = 'PublishPending'"#,
    )
    .bind(cycle.id)
    .execute(&mut **control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    record_receipt(
        control.transaction_mut(),
        cycle,
        CrownPhase::PublishPending,
        json!({"containerId": container_id, "host": host, "port": port}),
        None,
    )
    .await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::controllers::game::ad::invalidate_live_hill_snapshot(st, cycle.game_id).await;
    Ok(())
}

async fn activate_cycle(st: &SharedState, cycle: &CycleRow, round_number: i32) -> AppResult<()> {
    sqlx::query(
        r#"UPDATE "KothCycleCooldowns" cooldown
              SET starts_round = $2,
                  expires_after_round = $2 + config.champion_cooldown_ticks - 1
             FROM "KothCrownCycles" cycle
             JOIN "KothOfficialConfigs" config
               ON config.game_id = cycle.game_id
            WHERE cooldown.cycle_id = cycle.id AND cycle.id = $1
              AND cooldown.network_enforced = FALSE
              AND cooldown.network_released_at IS NULL"#,
    )
    .bind(cycle.id)
    .bind(round_number)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let enforced_cooldowns =
        crate::services::ad_vpn::enforce_cycle_cooldown(&st.db, cycle.id).await?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query(
        r#"UPDATE "KothCycleCooldowns"
              SET network_enforced = TRUE,
                  network_enforced_at = COALESCE(network_enforced_at, clock_timestamp())
            WHERE cycle_id = $1 AND network_released_at IS NULL"#,
    )
    .bind(cycle.id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let durable_enforced: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM "KothCycleCooldowns"
            WHERE cycle_id = $1 AND network_enforced = TRUE
              AND network_released_at IS NULL"#,
    )
    .bind(cycle.id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if durable_enforced != i64::try_from(enforced_cooldowns).unwrap_or(i64::MAX) {
        return Err(AppError::conflict(
            "KotH cooldown enforcement receipt does not cover every selected champion",
        ));
    }
    sqlx::query(
        r#"UPDATE "KothCrownCycles"
              SET phase = 'Active', actual_start_round = $2,
                  activated_at = clock_timestamp(), updated_at = clock_timestamp(),
                  readiness_error = NULL, last_error = NULL
            WHERE id = $1 AND phase = 'FirewallPending'"#,
    )
    .bind(cycle.id)
    .bind(round_number)
    .execute(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    record_receipt(
        &mut transaction,
        cycle,
        CrownPhase::FirewallPending,
        json!({
            "activatedRound": round_number,
            "cooldownNetworkEnforced": enforced_cooldowns > 0,
            "enforcedCooldownParticipants": enforced_cooldowns,
        }),
        None,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // Active is the first phase allowed to publish the replacement endpoint.
    crate::controllers::game::ad::invalidate_live_hill_snapshot(st, cycle.game_id).await;
    Ok(())
}

pub(super) async fn drive_one_cycle(
    st: &SharedState,
    config: &OfficialConfig,
    cycle_id: i64,
    ad_round_id: i32,
    round_number: i32,
) -> AppResult<()> {
    for transition in 0..12 {
        let cycle = load_cycle(st, cycle_id).await?;
        let phase = CrownPhase::parse(&cycle.phase)
            .ok_or_else(|| AppError::internal("unknown KotH crown-cycle phase"))?;
        if transition == 0 {
            super::rollover::backfill_missing_receipts(st, cycle.id, phase.as_str()).await?;
        }
        if Utc::now() >= config.end_time_utc {
            match deadline::action(phase, cycle.replacement_container_id.is_some()) {
                deadline::Action::Complete => {
                    deadline::complete_active_cycle(st, &cycle, round_number).await?;
                    continue;
                }
                deadline::Action::Cleanup => {
                    deadline::cleanup_completed_cycle(st, config, &cycle, round_number).await?;
                    return Ok(());
                }
                deadline::Action::Done => return Ok(()),
                // A crash can leave the deterministic operation-id container
                // created but not yet persisted. Re-enter create to adopt that
                // exact runtime, persist its id, then reclaim it below.
                deadline::Action::AdoptReplacement => {
                    create_replacement(st, &cycle).await?;
                    continue;
                }
                deadline::Action::Reclaim => {
                    deadline::terminate_interrupted_cycle(st, config, &cycle, round_number).await?;
                    return Ok(());
                }
            }
        }
        match phase {
            CrownPhase::FinalizePending => {
                finalize_previous_cycle(st, config, &cycle, round_number).await?
            }
            CrownPhase::SnapshotPending => snapshot_cycle(st, &cycle).await?,
            CrownPhase::DestroyPending => destroy_old(st, &cycle).await?,
            CrownPhase::CreatePending => create_replacement(st, &cycle).await?,
            CrownPhase::PublishPending => publish_replacement(st, &cycle).await?,
            CrownPhase::CapabilityPending => {
                mint_capabilities(st, config, &cycle, ad_round_id, round_number).await?
            }
            CrownPhase::ReadinessPending => readiness::validate(st, &cycle).await?,
            CrownPhase::FirewallPending => activate_cycle(st, &cycle, round_number).await?,
            CrownPhase::Active
            | CrownPhase::CooldownReleasePending
            | CrownPhase::Completed
            | CrownPhase::Ended => return Ok(()),
            CrownPhase::Failed => {
                return Err(AppError::conflict(
                    "KotH crown-cycle recovery requires an administrator retry",
                ))
            }
        }
    }
    Err(AppError::internal(
        "KotH crown-cycle exceeded its transition bound",
    ))
}

pub(crate) async fn drive_cycle_transitions(
    st: &SharedState,
    game_id: i32,
    ad_round_id: i32,
    round_number: i32,
) -> AppResult<()> {
    let Some(config) = load_config(st, game_id).await? else {
        return Ok(());
    };
    if round_number < config.scoring_start_round {
        return Ok(());
    }
    super::cooldown::release_expired(st, game_id, round_number).await?;
    let Some(position) = cycle_position(
        round_number,
        config.scoring_start_round,
        config.epoch_ticks,
        config.cycle_ticks,
    ) else {
        return Err(AppError::internal("invalid snapshotted KotH cycle shape"));
    };
    for &challenge_id in &config.challenge_ids {
        let key = format!("shared-container:{challenge_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let lock = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &key)
            .await?;
        let planned_start =
            config.scoring_start_round + (position.cycle_number - 1) * config.cycle_ticks;
        let cycle_id = create_or_load_cycle(
            st,
            game_id,
            challenge_id,
            position.cycle_number,
            position.epoch,
            planned_start,
            planned_start + config.cycle_ticks - 1,
        )
        .await?;
        let recovery = super::rollover::resume_previous_cycle(
            st,
            &config,
            game_id,
            challenge_id,
            position.cycle_number,
            ad_round_id,
            round_number,
        )
        .await;
        if let Err(error) = recovery {
            sqlx::query(
                r#"UPDATE "KothCrownCycles"
                      SET last_error = $2, updated_at = clock_timestamp()
                    WHERE id = $1 AND phase = 'FinalizePending'"#,
            )
            .bind(cycle_id)
            .bind(error.to_string())
            .execute(st.pg())
            .await
            .map_err(|db_error| AppError::internal(db_error.to_string()))?;
            lock.release().await?;
            return Err(error);
        }
        super::rollover::refresh_old_container(st, cycle_id).await?;
        let result = drive_one_cycle(st, &config, cycle_id, ad_round_id, round_number).await;
        if let Err(error) = &result {
            sqlx::query(
                r#"UPDATE "KothCrownCycles"
                      SET last_error = $2, updated_at = clock_timestamp()
                    WHERE id = $1 AND phase NOT IN ('Active','Completed','Ended')"#,
            )
            .bind(cycle_id)
            .bind(error.to_string())
            .execute(st.pg())
            .await
            .map_err(|db_error| AppError::internal(db_error.to_string()))?;
        }
        lock.release().await?;
        result?;
    }
    Ok(())
}

/// Resume crown-cycle teardown after an event deadline, independently of the
/// round pipeline. The final round may already be sealed when a replica dies in
/// destroy/create/readiness, so cron must keep driving those durable states
/// until every affected hill is terminal.
pub(crate) async fn recover_ended_cycle_transitions(st: &SharedState) -> AppResult<u64> {
    let pending = sqlx::query_as::<_, (i64, i32, i32, i32, i32)>(
        r#"SELECT cycle.id, cycle.game_id, cycle.challenge_id,
                  latest_round.id, latest_round.number
             FROM "KothCrownCycles" cycle
             JOIN "Games" game ON game.id = cycle.game_id
             JOIN "KothOfficialConfigs" config
               ON config.game_id = cycle.game_id
             JOIN LATERAL (
                  SELECT round.id, round.number
                    FROM "AdRounds" round
                   WHERE round.game_id = cycle.game_id
                   ORDER BY round.number DESC
                   LIMIT 1
             ) latest_round ON TRUE
            WHERE game.end_time_utc <= now()
              AND (
                   cycle.phase NOT IN ('Completed','Ended')
                   OR (
                     cycle.phase = 'Completed'
                     AND NOT EXISTS (
                       SELECT 1 FROM "KothCrownCycles" newer
                        WHERE newer.game_id = cycle.game_id
                          AND newer.challenge_id = cycle.challenge_id
                          AND newer.cycle_number > cycle.cycle_number
                     )
                     AND (
                       NOT EXISTS (
                         SELECT 1 FROM "KothCycleAuditReceipts" receipt
                          WHERE receipt.cycle_id = cycle.id
                            AND receipt.phase = 'DeadlineCleanup'
                            AND receipt.attempt = cycle.reset_attempt
                       )
                       OR EXISTS (
                         SELECT 1 FROM "KothTargets" target
                          WHERE target.game_id = cycle.game_id
                            AND target.challenge_id = cycle.challenge_id
                            AND (target.container_id IS NOT NULL OR target.host <> ''
                                 OR target.holder_participation_id IS NOT NULL)
                       )
                       OR EXISTS (
                         SELECT 1 FROM "KothClaimStates" claim
                          JOIN "KothTargets" target ON target.id = claim.target_id
                         WHERE target.game_id = cycle.game_id
                           AND target.challenge_id = cycle.challenge_id
                       )
                       OR EXISTS (
                         SELECT 1 FROM "KothTokens" token
                          JOIN "KothCrownCycles" owner ON owner.id = token.cycle_id
                         WHERE owner.game_id = cycle.game_id
                           AND owner.challenge_id = cycle.challenge_id
                           AND token.revoked_at IS NULL
                       )
                     )
                   )
              )
            ORDER BY cycle.game_id, cycle.challenge_id, cycle.cycle_number"#,
    )
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    let mut recovered = 0u64;
    for (cycle_id, game_id, challenge_id, ad_round_id, round_number) in pending {
        let key = format!("shared-container:{challenge_id}");
        let _local = crate::utils::single_flight::coalesce(&key).await;
        let lock = crate::utils::single_flight::PgAdvisoryLock::acquire_provisioning(st.pg(), &key)
            .await?;

        let result = match load_config(st, game_id).await? {
            Some(config) if Utc::now() >= config.end_time_utc => {
                drive_one_cycle(st, &config, cycle_id, ad_round_id, round_number).await
            }
            _ => Ok(()),
        };
        if let Err(error) = &result {
            deadline::record_recovery_error(st, cycle_id, &error.to_string()).await?;
            tracing::warn!(
                game = game_id,
                challenge = challenge_id,
                cycle = cycle_id,
                %error,
                "cron: ended KotH crown-cycle recovery remains pending"
            );
        } else {
            let terminal = sqlx::query_scalar::<_, bool>(
                r#"SELECT phase IN ('Completed','Ended')
                     FROM "KothCrownCycles" WHERE id = $1"#,
            )
            .bind(cycle_id)
            .fetch_optional(st.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?
            .unwrap_or(false);
            recovered += u64::from(terminal);
        }
        lock.release().await?;
    }
    Ok(recovered)
}

pub(crate) async fn recover_cycle(
    st: &SharedState,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<(i32, String)> {
    let (round_id, round_number): (i32, i32) = sqlx::query_as(
        r#"SELECT id, number FROM "AdRounds"
            WHERE game_id = $1 ORDER BY number DESC LIMIT 1"#,
    )
    .bind(game_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::conflict("No authoritative round exists yet"))?;
    drive_cycle_transitions(st, game_id, round_id, round_number).await?;
    sqlx::query_as::<_, (i32, String)>(
        r#"SELECT cycle_number, phase FROM "KothCrownCycles"
            WHERE game_id = $1 AND challenge_id = $2
            ORDER BY cycle_number DESC LIMIT 1"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("KotH crown cycle not found"))
}

#[cfg(test)]
#[path = "lifecycle/tests.rs"]
mod tests;
