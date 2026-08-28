use super::*;

pub(super) fn request_digest(model: &GameInfoModel) -> AppResult<String> {
    let payload = serde_json::json!({
        "configuration": model,
        "vpnPolicyChangeReason": model.vpn_policy_change_reason,
    });
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| AppError::internal(format!("could not encode game update: {error}")))?;
    Ok(crate::utils::codec::sha256_hex(&bytes))
}

pub(super) async fn replay_operation(
    connection: &mut sqlx::PgConnection,
    operation_id: Uuid,
    game_id: i32,
    actor_user_id: Uuid,
    digest: &str,
) -> AppResult<Option<GameInfoModel>> {
    let row = sqlx::query_as::<_, (i32, Uuid, String, sqlx::types::Json<GameInfoModel>)>(
        r#"SELECT game_id, actor_user_id, request_digest, result
             FROM "GameConfigurationOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let Some((stored_game, stored_actor, stored_digest, result)) = row else {
        return Ok(None);
    };
    if stored_game != game_id || stored_actor != actor_user_id || stored_digest != digest {
        return Err(AppError::conflict(
            "The settings operation ID was already used for a different request",
        ));
    }
    Ok(Some(result.0))
}

pub(super) async fn load_game_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    lock: bool,
) -> AppResult<game::Model> {
    let sql = if lock {
        r#"SELECT to_jsonb(game) FROM "Games" game WHERE game.id = $1 FOR UPDATE"#
    } else {
        r#"SELECT to_jsonb(game) FROM "Games" game WHERE game.id = $1"#
    };
    let value = sqlx::query_scalar::<_, serde_json::Value>(sql)
        .bind(game_id)
        .fetch_optional(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    serde_json::from_value(value)
        .map_err(|error| AppError::internal(format!("could not decode game row: {error}")))
}

pub(super) fn requested_game(
    current: &game::Model,
    model: &GameInfoModel,
    discord_webhook: Option<String>,
) -> game::Model {
    let mut requested = current.clone();
    requested.title.clone_from(&model.title);
    requested.content.clone_from(&model.content);
    requested.summary.clone_from(&model.summary);
    requested.hidden = model.hidden;
    requested.practice_mode = model.practice_mode;
    requested.accept_without_review = model.accept_without_review;
    requested.allow_user_submissions = model.allow_user_submissions;
    requested.invite_code.clone_from(&model.invite_code);
    requested.start_time_utc = model.start_time_utc;
    requested.end_time_utc = model.end_time_utc;
    requested.team_member_count_limit = model.team_member_count_limit;
    requested.container_count_limit = model.container_count_limit;
    requested.writeup_note.clone_from(&model.writeup_note);
    requested.writeup_required = model.writeup_required;
    requested.writeup_deadline = model.writeup_deadline;
    requested.freeze_time_utc = model.freeze_time_utc;
    requested.blood_bonus_value = super::blood_bonus_from_value(model.blood_bonus_value);
    requested.discord_webhook = discord_webhook;
    requested.ad_warmup_seconds = model.ad_warmup_seconds;
    requested.ad_snapshot_retention_days = model.ad_snapshot_retention_days;
    requested.ad_tick_seconds = model.ad_tick_seconds;
    requested.ad_flag_lifetime_ticks = model.ad_flag_lifetime_ticks;
    requested.ad_reset_cooldown_minutes = model.ad_reset_cooldown_minutes;
    if let Some(value) = model.ad_allow_snapshot_download {
        requested.ad_allow_snapshot_download = value;
    }
    requested.ad_getflag_window_fraction = model.ad_getflag_window_fraction;
    requested.ad_min_grace_period_seconds = model.ad_min_grace_period_seconds;
    requested.ad_epoch_ticks = model.ad_epoch_ticks.unwrap_or(current.ad_epoch_ticks);
    requested.koth_epoch_ticks = model.koth_epoch_ticks.unwrap_or(current.koth_epoch_ticks);
    requested.koth_cycle_ticks = model.koth_cycle_ticks.unwrap_or(current.koth_cycle_ticks);
    requested.koth_champion_cooldown_ticks = model
        .koth_champion_cooldown_ticks
        .unwrap_or(current.koth_champion_cooldown_ticks);
    requested.koth_claim_confirmation_ticks = model
        .koth_claim_confirmation_ticks
        .unwrap_or(current.koth_claim_confirmation_ticks);
    requested.vpn_access_required = model.vpn_access_required;
    requested.vpn_behavior_telemetry_enabled = model.vpn_behavior_telemetry_enabled;
    requested.vpn_flag_scan_enabled = model.vpn_flag_scan_enabled;
    requested.vpn_provider_dns_telemetry_enabled = model.vpn_provider_dns_telemetry_enabled;
    requested.vpn_source_asn_telemetry_enabled = model.vpn_source_asn_telemetry_enabled;
    requested.vpn_device_sharing_telemetry_enabled = model.vpn_device_sharing_telemetry_enabled;
    requested
}

fn editable_projection(game: &game::Model) -> serde_json::Value {
    serde_json::json!({
        "title": game.title,
        "content": game.content,
        "summary": game.summary,
        "hidden": game.hidden,
        "practiceMode": game.practice_mode,
        "acceptWithoutReview": game.accept_without_review,
        "allowUserSubmissions": game.allow_user_submissions,
        "inviteCode": game.invite_code,
        "start": game.start_time_utc,
        "end": game.end_time_utc,
        "teamMemberCountLimit": game.team_member_count_limit,
        "containerCountLimit": game.container_count_limit,
        "writeupNote": game.writeup_note,
        "writeupRequired": game.writeup_required,
        "writeupDeadline": game.writeup_deadline,
        "freeze": game.freeze_time_utc,
        "bloodBonus": game.blood_bonus_value,
        "discordWebhook": game.discord_webhook,
        "adWarmupSeconds": game.ad_warmup_seconds,
        "adSnapshotRetentionDays": game.ad_snapshot_retention_days,
        "adTickSeconds": game.ad_tick_seconds,
        "adFlagLifetimeTicks": game.ad_flag_lifetime_ticks,
        "adResetCooldownMinutes": game.ad_reset_cooldown_minutes,
        "adAllowSnapshotDownload": game.ad_allow_snapshot_download,
        "adGetflagWindowFraction": game.ad_getflag_window_fraction,
        "adMinGracePeriodSeconds": game.ad_min_grace_period_seconds,
        "adEpochTicks": game.ad_epoch_ticks,
        "kothEpochTicks": game.koth_epoch_ticks,
        "kothCycleTicks": game.koth_cycle_ticks,
        "kothChampionCooldownTicks": game.koth_champion_cooldown_ticks,
        "kothClaimConfirmationTicks": game.koth_claim_confirmation_ticks,
        "vpnAccessRequired": game.vpn_access_required,
        "vpnBehaviorTelemetryEnabled": game.vpn_behavior_telemetry_enabled,
        "vpnFlagScanEnabled": game.vpn_flag_scan_enabled,
        "vpnProviderDnsTelemetryEnabled": game.vpn_provider_dns_telemetry_enabled,
        "vpnSourceAsnTelemetryEnabled": game.vpn_source_asn_telemetry_enabled,
        "vpnDeviceSharingTelemetryEnabled": game.vpn_device_sharing_telemetry_enabled,
    })
}

pub(super) fn configuration_changed(current: &game::Model, requested: &game::Model) -> bool {
    editable_projection(current) != editable_projection(requested)
}

pub(super) fn scoreboard_changed(current: &game::Model, requested: &game::Model) -> bool {
    serde_json::json!({
        "title": current.title,
        "hidden": current.hidden,
        "practice": current.practice_mode,
        "start": current.start_time_utc,
        "end": current.end_time_utc,
        "freeze": current.freeze_time_utc,
        "blood": current.blood_bonus_value,
        "adEpoch": current.ad_epoch_ticks,
        "kothEpoch": current.koth_epoch_ticks,
    }) != serde_json::json!({
        "title": requested.title,
        "hidden": requested.hidden,
        "practice": requested.practice_mode,
        "start": requested.start_time_utc,
        "end": requested.end_time_utc,
        "freeze": requested.freeze_time_utc,
        "blood": requested.blood_bonus_value,
        "adEpoch": requested.ad_epoch_ticks,
        "kothEpoch": requested.koth_epoch_ticks,
    })
}

pub(super) async fn store_operation(
    connection: &mut sqlx::PgConnection,
    operation_id: Uuid,
    game_id: i32,
    actor_user_id: Uuid,
    digest: &str,
    expected_revision: i64,
    result: &GameInfoModel,
) -> AppResult<()> {
    let inserted = sqlx::query(
        r#"INSERT INTO "GameConfigurationOperations"
               (operation_id, game_id, actor_user_id, request_digest,
                expected_revision, result_revision, result)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(operation_id)
    .bind(game_id)
    .bind(actor_user_id)
    .bind(digest)
    .bind(expected_revision)
    .bind(result.configuration_revision)
    .bind(sqlx::types::Json(result))
    .execute(connection)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(error) if crate::utils::error::is_unique_violation(&error) => Err(AppError::conflict(
            "The settings operation ID was already used for a different request",
        )),
        Err(error) => Err(AppError::internal(error.to_string())),
    }
}

pub(super) async fn enqueue_effects(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    revision: i64,
    invalidate_scoreboards: bool,
    invalidate_policy: bool,
) -> AppResult<()> {
    sqlx::query(
        r#"INSERT INTO "GameConfigurationEffects"
               (game_id, configuration_revision, invalidate_game,
                invalidate_scoreboards, invalidate_policy)
           VALUES ($1, $2, TRUE, $3, $4)
           ON CONFLICT (game_id) DO UPDATE
             SET configuration_revision = GREATEST(
                     "GameConfigurationEffects".configuration_revision,
                     EXCLUDED.configuration_revision),
                 invalidate_game = "GameConfigurationEffects".invalidate_game
                                   OR EXCLUDED.invalidate_game,
                 invalidate_scoreboards = "GameConfigurationEffects".invalidate_scoreboards
                                         OR EXCLUDED.invalidate_scoreboards,
                 invalidate_policy = "GameConfigurationEffects".invalidate_policy
                                     OR EXCLUDED.invalidate_policy,
                 claim_id = NULL,
                 claim_expires_at_utc = NULL,
                 updated_at_utc = CURRENT_TIMESTAMP"#,
    )
    .bind(game_id)
    .bind(revision)
    .bind(invalidate_scoreboards)
    .bind(invalidate_policy)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if invalidate_policy {
        sqlx::query(
            r#"INSERT INTO "AdNetworkReconcileState"
                   (id, requested_generation, applied_generation, requested_at, applied_at)
               VALUES (1, 1, 0, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
               ON CONFLICT (id) DO UPDATE
                 SET requested_generation =
                         "AdNetworkReconcileState".requested_generation + 1,
                     requested_at = CURRENT_TIMESTAMP"#,
        )
        .execute(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct PendingEffect {
    game_id: i32,
    configuration_revision: i64,
    invalidate_game: bool,
    invalidate_scoreboards: bool,
    invalidate_policy: bool,
}

pub(crate) async fn process_configuration_effects(state: &SharedState) -> AppResult<u64> {
    let claim_id = Uuid::new_v4();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let effects = sqlx::query_as::<_, PendingEffect>(
        r#"WITH candidates AS (
               SELECT game_id FROM "GameConfigurationEffects"
                WHERE claim_expires_at_utc IS NULL
                   OR claim_expires_at_utc <= CURRENT_TIMESTAMP
                ORDER BY updated_at_utc, game_id
                FOR UPDATE SKIP LOCKED LIMIT 32
           )
           UPDATE "GameConfigurationEffects" effect
              SET claim_id = $1,
                  claim_expires_at_utc = CURRENT_TIMESTAMP + INTERVAL '2 minutes'
             FROM candidates
            WHERE effect.game_id = candidates.game_id
        RETURNING effect.game_id, effect.configuration_revision,
                  effect.invalidate_game, effect.invalidate_scoreboards,
                  effect.invalidate_policy"#,
    )
    .bind(claim_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut processed = 0;
    for effect in effects {
        if effect.invalidate_game {
            crate::controllers::game::invalidate_game_row_cache(effect.game_id);
        }
        if effect.invalidate_policy {
            crate::services::event_security::invalidate_policy(state, effect.game_id).await;
        }
        if effect.invalidate_scoreboards {
            flush_game_scoreboards(state, effect.game_id).await;
        }
        let deleted = sqlx::query(
            r#"DELETE FROM "GameConfigurationEffects"
                WHERE game_id = $1 AND configuration_revision = $2 AND claim_id = $3"#,
        )
        .bind(effect.game_id)
        .bind(effect.configuration_revision)
        .bind(claim_id)
        .execute(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        processed += deleted.rows_affected();
    }
    sqlx::query(
        r#"DELETE FROM "GameConfigurationOperations"
            WHERE operation_id IN (
                SELECT operation_id FROM "GameConfigurationOperations"
                 WHERE created_at_utc < CURRENT_TIMESTAMP - INTERVAL '7 days'
                 ORDER BY created_at_utc LIMIT 256
            )"#,
    )
    .execute(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(processed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_only_edits_do_not_flush_scoreboards() {
        let mut current: game::Model = serde_json::from_value(serde_json::json!({
            "id": 1, "title": "Event", "public_key": "public", "private_key": "private",
            "hidden": false, "practice_mode": true, "poster_hash": null,
            "summary": "old", "content": "old", "accept_without_review": false,
            "allow_user_submissions": false, "writeup_required": false, "invite_code": null,
            "team_member_count_limit": 0, "discord_webhook": null, "container_count_limit": 0,
            "start_time_utc": "2026-01-01T00:00:00Z", "end_time_utc": "2026-01-02T00:00:00Z",
            "writeup_deadline": "2026-01-03T00:00:00Z", "freeze_time_utc": null,
            "writeup_note": "", "blood_bonus_value": 0, "repo_binding_id": null,
            "event_manifest_path": null, "vpn_access_required": false,
            "vpn_behavior_telemetry_enabled": false, "vpn_flag_scan_enabled": false,
            "vpn_provider_dns_telemetry_enabled": false, "vpn_source_asn_telemetry_enabled": false,
            "vpn_device_sharing_telemetry_enabled": false, "configuration_revision": 0,
            "ad_warmup_seconds": null, "ad_tick_seconds": null, "ad_flag_lifetime_ticks": null,
            "ad_reset_cooldown_minutes": null, "ad_getflag_window_fraction": null,
            "ad_min_grace_period_seconds": null, "koth_refresh_ticks": null,
            "koth_hold_points_per_tick": null, "ad_allow_snapshot_download": true,
            "ad_snapshot_retention_days": null, "ad_scoring_paused": false,
            "ad_scoring_paused_at": null, "ad_epoch_ticks": 8,
            "ad_scoring_start_round": null, "koth_scoring_start_round": null,
            "koth_epoch_ticks": 12, "koth_cycle_ticks": 3,
            "koth_champion_cooldown_ticks": 1, "koth_claim_confirmation_ticks": 2
        }))
        .unwrap();
        let original = current.clone();
        current.summary = "new".into();
        current.content = "new".into();
        assert!(configuration_changed(&original, &current));
        assert!(!scoreboard_changed(&original, &current));
    }
}
