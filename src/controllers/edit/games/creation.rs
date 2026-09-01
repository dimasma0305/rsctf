use super::*;

#[cfg(test)]
pub(crate) fn apply_ad_creation_settings(model: &GameInfoModel, active: &mut game::ActiveModel) {
    active.ad_warmup_seconds = Set(model.ad_warmup_seconds);
    active.ad_snapshot_retention_days = Set(model.ad_snapshot_retention_days);
    active.ad_tick_seconds = Set(model.ad_tick_seconds);
    active.ad_flag_lifetime_ticks = Set(model.ad_flag_lifetime_ticks);
    active.ad_reset_cooldown_minutes = Set(model.ad_reset_cooldown_minutes);
    active.ad_allow_snapshot_download = Set(model.ad_allow_snapshot_download.unwrap_or(true));
    active.ad_getflag_window_fraction = Set(model.ad_getflag_window_fraction);
    active.ad_min_grace_period_seconds = Set(model.ad_min_grace_period_seconds);
    active.ad_epoch_ticks = Set(model.ad_epoch_ticks.unwrap_or(8));
}

/// `POST /api/edit/games` — atomically create a template and retain its result
/// identity under the caller's stable idempotency key.
pub async fn add_game(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
    Json(model): Json<GameInfoModel>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    let discord_webhook = model.validate()?;
    model.validate_event_security(&st)?;
    let operation_id = crate::controllers::edit::control_jobs::operation_id(&headers)?;
    let fingerprint = crate::services::mutation_operations::fingerprint("game-create", &model)?;
    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let replay = crate::services::mutation_operations::claim(
        &mut transaction,
        admin.id,
        "game-create",
        "global",
        operation_id,
        fingerprint,
    )
    .await?;

    let game_id = if let Some(replay) = replay {
        replay
            .result_id
            .parse::<i32>()
            .map_err(|_| AppError::internal("invalid retained game result identity"))?
    } else {
        let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
        let game_id: i32 = sqlx::query_scalar(
            r#"INSERT INTO "Games"
                 (title, public_key, private_key, hidden, practice_mode, summary, content,
                  accept_without_review, allow_user_submissions, writeup_required, invite_code,
                  team_member_count_limit, discord_webhook, container_count_limit,
                  start_time_utc, end_time_utc, writeup_deadline, freeze_time_utc, writeup_note,
                  blood_bonus_value, ad_warmup_seconds, ad_snapshot_retention_days,
                  ad_tick_seconds, ad_flag_lifetime_ticks, ad_reset_cooldown_minutes,
                  ad_allow_snapshot_download, ad_getflag_window_fraction,
                  ad_min_grace_period_seconds, ad_epoch_ticks, koth_epoch_ticks,
                  koth_cycle_ticks, koth_champion_cooldown_ticks,
                  koth_claim_confirmation_ticks, ad_scoring_start_round, ad_scoring_paused,
                  vpn_access_required, vpn_behavior_telemetry_enabled, vpn_flag_scan_enabled,
                  vpn_provider_dns_telemetry_enabled, vpn_source_asn_telemetry_enabled,
                  vpn_device_sharing_telemetry_enabled)
               VALUES
                 ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
                  $21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,NULL,FALSE,$34,$35,$36,
                  $37,$38,$39)
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
        .bind(model.ad_warmup_seconds)
        .bind(model.ad_snapshot_retention_days)
        .bind(model.ad_tick_seconds)
        .bind(model.ad_flag_lifetime_ticks)
        .bind(model.ad_reset_cooldown_minutes)
        .bind(model.ad_allow_snapshot_download.unwrap_or(true))
        .bind(model.ad_getflag_window_fraction)
        .bind(model.ad_min_grace_period_seconds)
        .bind(model.ad_epoch_ticks.unwrap_or(8))
        .bind(model.koth_epoch_ticks.unwrap_or(12))
        .bind(model.koth_cycle_ticks.unwrap_or(3))
        .bind(model.koth_champion_cooldown_ticks.unwrap_or(1))
        .bind(model.koth_claim_confirmation_ticks.unwrap_or(2))
        .bind(model.vpn_access_required)
        .bind(model.vpn_behavior_telemetry_enabled)
        .bind(model.vpn_flag_scan_enabled)
        .bind(model.vpn_provider_dns_telemetry_enabled)
        .bind(model.vpn_source_asn_telemetry_enabled)
        .bind(model.vpn_device_sharing_telemetry_enabled)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        crate::services::mutation_operations::complete(
            &mut transaction,
            admin.id,
            "game-create",
            "global",
            operation_id,
            &game_id.to_string(),
            None,
        )
        .await?;
        game_id
    };
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let created = load_game(&st, game_id).await?;
    Ok(RequestResponse::ok(GameInfoModel::from_game(&created)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn game_insert_and_operation_result_share_one_transaction() {
        let source = include_str!("creation.rs");
        assert!(source.contains("INSERT INTO \"Games\""));
        assert!(source.contains("mutation_operations::complete"));
        assert!(source.contains("transaction.commit()"));
        assert!(
            source.find("INSERT INTO \"Games\"").unwrap()
                < source.find("transaction.commit()").unwrap()
        );
    }
}
