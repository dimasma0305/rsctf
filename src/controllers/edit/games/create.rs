use super::*;

/// `POST /api/edit/games` — create with a fresh key pair + defaults.
pub async fn add_game(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Json(model): Json<GameInfoModel>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    let discord_webhook = model.validate()?;
    model.validate_event_security(&st)?;
    let koth_epoch_ticks = model.koth_epoch_ticks.unwrap_or(12);
    let koth_cycle_ticks = model.koth_cycle_ticks.unwrap_or(3);
    let koth_champion_cooldown_ticks = model.koth_champion_cooldown_ticks.unwrap_or(1);
    let koth_claim_confirmation_ticks = model.koth_claim_confirmation_ticks.unwrap_or(2);

    let mut digest_model = model.clone();
    digest_model.operation_id = None;
    let request_digest = crate::utils::codec::sha256_str(
        &serde_json::to_string(&digest_model)
            .map_err(|error| AppError::internal(error.to_string()))?,
    );
    let mut transaction = st
        .pg()
        .begin()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(operation_id) = model.operation_id {
        if let Some(result_id) = crate::services::create_operations::claim(
            &mut transaction,
            user.id,
            "game",
            0,
            operation_id,
            &request_digest,
        )
        .await?
        {
            let game_id = result_id
                .parse::<i32>()
                .map_err(|_| AppError::internal("invalid retained game create result"))?;
            transaction
                .commit()
                .await
                .map_err(|error| AppError::internal(error.to_string()))?;
            let created = load_game(&st, game_id).await?;
            return Ok(RequestResponse::ok(GameInfoModel::from_game(&created)));
        }
    }

    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let game_id = sqlx::query_scalar::<_, i32>(
        r#"INSERT INTO "Games"
                  (title, public_key, private_key, hidden, practice_mode, summary,
                   content, accept_without_review, allow_user_submissions,
                   writeup_required, invite_code, team_member_count_limit,
                   discord_webhook, container_count_limit, start_time_utc,
                   end_time_utc, writeup_deadline, freeze_time_utc, writeup_note,
                   blood_bonus_value, koth_epoch_ticks, koth_cycle_ticks,
                   koth_champion_cooldown_ticks, koth_claim_confirmation_ticks,
                   ad_scoring_paused, vpn_access_required,
                   vpn_behavior_telemetry_enabled, vpn_flag_scan_enabled,
                   vpn_provider_dns_telemetry_enabled,
                   vpn_source_asn_telemetry_enabled,
                   vpn_device_sharing_telemetry_enabled, ad_warmup_seconds,
                   ad_snapshot_retention_days, ad_tick_seconds,
                   ad_flag_lifetime_ticks, ad_reset_cooldown_minutes,
                   ad_allow_snapshot_download, ad_getflag_window_fraction,
                   ad_min_grace_period_seconds, ad_epoch_ticks)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                   $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23,
                   $24, FALSE, $25, $26, $27, $28, $29, $30, $31, $32, $33,
                   $34, $35, $36, $37, $38, $39, $40)
           RETURNING id"#,
    )
    .bind(&model.title)
    .bind(public_key)
    .bind(private_key)
    .bind(model.hidden)
    .bind(model.practice_mode)
    .bind(&model.summary)
    .bind(&model.content)
    .bind(model.accept_without_review)
    .bind(model.allow_user_submissions)
    .bind(model.writeup_required)
    .bind(&model.invite_code)
    .bind(model.team_member_count_limit)
    .bind(discord_webhook)
    .bind(model.container_count_limit)
    .bind(model.start_time_utc)
    .bind(model.end_time_utc)
    .bind(model.writeup_deadline)
    .bind(model.freeze_time_utc)
    .bind(&model.writeup_note)
    .bind(super::super::blood_bonus_from_value(
        model.blood_bonus_value,
    ))
    .bind(koth_epoch_ticks)
    .bind(koth_cycle_ticks)
    .bind(koth_champion_cooldown_ticks)
    .bind(koth_claim_confirmation_ticks)
    .bind(model.vpn_access_required)
    .bind(model.vpn_behavior_telemetry_enabled)
    .bind(model.vpn_flag_scan_enabled)
    .bind(model.vpn_provider_dns_telemetry_enabled)
    .bind(model.vpn_source_asn_telemetry_enabled)
    .bind(model.vpn_device_sharing_telemetry_enabled)
    .bind(model.ad_warmup_seconds)
    .bind(model.ad_snapshot_retention_days)
    .bind(model.ad_tick_seconds)
    .bind(model.ad_flag_lifetime_ticks)
    .bind(model.ad_reset_cooldown_minutes)
    .bind(model.ad_allow_snapshot_download.unwrap_or(true))
    .bind(model.ad_getflag_window_fraction)
    .bind(model.ad_min_grace_period_seconds)
    .bind(model.ad_epoch_ticks.unwrap_or(8))
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(operation_id) = model.operation_id {
        crate::services::create_operations::complete(
            &mut transaction,
            user.id,
            "game",
            0,
            operation_id,
            &game_id.to_string(),
        )
        .await?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let created = load_game(&st, game_id).await?;
    Ok(RequestResponse::ok(GameInfoModel::from_game(&created)))
}
