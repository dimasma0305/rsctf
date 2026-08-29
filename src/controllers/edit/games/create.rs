use super::*;

const INSERT_GAME_SQL: &str = r#"INSERT INTO "Games"
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
           $34, $35, $36, $37, $38, $39)
   RETURNING id"#;

/// `POST /api/edit/games` — create with a fresh key pair + defaults.
pub async fn add_game(
    State(st): State<SharedState>,
    AdminUser(user): AdminUser,
    Json(model): Json<GameInfoModel>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    let operation_id =
        crate::services::create_operations::require_operation_id(model.operation_id)?;
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

    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let game_id = sqlx::query_scalar::<_, i32>(INSERT_GAME_SQL)
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
    crate::services::create_operations::complete(
        &mut transaction,
        user.id,
        "game",
        0,
        operation_id,
        &game_id.to_string(),
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let created = load_game(&st, game_id).await?;
    Ok(RequestResponse::ok(GameInfoModel::from_game(&created)))
}

#[cfg(test)]
mod tests {
    use sea_orm::SqlxPostgresConnector;
    use sea_orm_migration::MigratorTrait;
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    #[tokio::test]
    #[ignore = "requires PostgreSQL via RSCTF_TEST_DATABASE_URL"]
    async fn game_insert_contract_executes_with_every_bound_value() {
        let database_url = std::env::var("RSCTF_TEST_DATABASE_URL")
            .expect("RSCTF_TEST_DATABASE_URL must point to disposable PostgreSQL");
        let admin_options = crate::migrations::test_pg_connect_options(&database_url);
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(admin_options)
            .await
            .unwrap();
        let schema = format!("game_create_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(r#"CREATE SCHEMA "{schema}""#))
            .execute(&admin)
            .await
            .unwrap();
        let options = crate::migrations::test_pg_connect_options(&database_url)
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        let database = SqlxPostgresConnector::from_sqlx_postgres_pool(pool.clone());
        crate::migrations::Migrator::up(&database, None)
            .await
            .unwrap();

        let now = Utc::now();
        let game_id = sqlx::query_scalar::<_, i32>(INSERT_GAME_SQL)
            .bind("create contract")
            .bind("public-key")
            .bind("private-key")
            .bind(false)
            .bind(false)
            .bind("")
            .bind("")
            .bind(true)
            .bind(false)
            .bind(false)
            .bind(None::<String>)
            .bind(0_i32)
            .bind(None::<String>)
            .bind(3_i32)
            .bind(now)
            .bind(now + chrono::Duration::hours(2))
            .bind(now + chrono::Duration::hours(3))
            .bind(None::<DateTime<Utc>>)
            .bind("")
            .bind(0_i64)
            .bind(12_i32)
            .bind(3_i32)
            .bind(1_i32)
            .bind(2_i32)
            .bind(false)
            .bind(false)
            .bind(false)
            .bind(false)
            .bind(false)
            .bind(false)
            .bind(None::<i32>)
            .bind(None::<i32>)
            .bind(None::<i32>)
            .bind(None::<i32>)
            .bind(None::<i32>)
            .bind(true)
            .bind(None::<f64>)
            .bind(None::<i32>)
            .bind(8_i32)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(game_id > 0);

        pool.close().await;
        sqlx::query(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .execute(&admin)
            .await
            .unwrap();
    }
}
