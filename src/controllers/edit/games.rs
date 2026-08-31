//! edit: game CRUD/clone/writeups (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

mod creation;
pub use creation::add_game;
pub(crate) use creation::apply_ad_creation_settings;

/// RSCTF `Models/Request/Edit/GameInfoModel` — used for both create/update
/// (inbound) and the get/delete responses (outbound). The `start`/`end`/
/// `freeze`/`poster`/`bloodBonus` JSON names are load-bearing overrides of the
/// default camelCase mapping and must match the original API contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameInfoModel {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub accept_without_review: bool,
    #[serde(default)]
    pub allow_user_submissions: bool,
    #[serde(default)]
    pub writeup_required: bool,
    #[serde(default)]
    pub invite_code: Option<String>,
    #[serde(default)]
    pub team_member_count_limit: i32,
    #[serde(default = "default_container_limit")]
    pub container_count_limit: i32,
    #[serde(default)]
    pub discord_webhook: Option<String>,
    #[serde(default, rename = "poster")]
    pub poster_url: Option<String>,
    #[serde(default)]
    pub public_key: String,
    #[serde(default = "default_true")]
    pub practice_mode: bool,
    #[serde(
        default = "epoch",
        rename = "start",
        with = "crate::utils::datetime::millis"
    )]
    pub start_time_utc: DateTime<Utc>,
    #[serde(
        default = "epoch",
        rename = "end",
        with = "crate::utils::datetime::millis"
    )]
    pub end_time_utc: DateTime<Utc>,
    #[serde(
        default,
        rename = "freeze",
        with = "crate::utils::datetime::millis_opt"
    )]
    pub freeze_time_utc: Option<DateTime<Utc>>,
    #[serde(default = "epoch", with = "crate::utils::datetime::millis")]
    pub writeup_deadline: DateTime<Utc>,
    #[serde(default)]
    pub writeup_note: String,
    #[serde(default = "default_blood_bonus", rename = "bloodBonus")]
    pub blood_bonus_value: i64,
    // --- A&D / KotH knobs (only overwrite when provided) ---
    #[serde(default)]
    pub ad_warmup_seconds: Option<i32>,
    #[serde(default)]
    pub ad_snapshot_retention_days: Option<i32>,
    #[serde(default)]
    pub ad_tick_seconds: Option<i32>,
    #[serde(default)]
    pub ad_flag_lifetime_ticks: Option<i32>,
    #[serde(default)]
    pub ad_reset_cooldown_minutes: Option<i32>,
    #[serde(default)]
    pub ad_allow_snapshot_download: Option<bool>,
    #[serde(default)]
    pub ad_getflag_window_fraction: Option<f64>,
    #[serde(default)]
    pub ad_min_grace_period_seconds: Option<i32>,
    #[serde(default)]
    pub ad_epoch_ticks: Option<i32>,
    #[serde(default)]
    pub koth_epoch_ticks: Option<i32>,
    #[serde(default)]
    pub koth_cycle_ticks: Option<i32>,
    #[serde(default)]
    pub koth_champion_cooldown_ticks: Option<i32>,
    #[serde(default)]
    pub koth_claim_confirmation_ticks: Option<i32>,
    #[serde(default, skip_deserializing)]
    pub ad_scoring_start_round: Option<i32>,
    #[serde(default, skip_deserializing)]
    pub koth_scoring_start_round: Option<i32>,
    #[serde(default)]
    pub vpn_access_required: bool,
    #[serde(default)]
    pub vpn_behavior_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_flag_scan_enabled: bool,
    #[serde(default)]
    pub vpn_provider_dns_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_source_asn_telemetry_enabled: bool,
    #[serde(default)]
    pub vpn_device_sharing_telemetry_enabled: bool,
    #[serde(skip_deserializing, with = "crate::utils::datetime::millis_opt")]
    pub server_time: Option<DateTime<Utc>>,
    /// Required only when an existing event's VPN/telemetry policy changes.
    #[serde(default, skip_serializing)]
    pub vpn_policy_change_reason: Option<String>,
}

impl GameInfoModel {
    fn from_game(g: &game::Model) -> Self {
        Self {
            id: g.id,
            title: g.title.clone(),
            hidden: g.hidden,
            summary: g.summary.clone(),
            content: g.content.clone(),
            accept_without_review: g.accept_without_review,
            allow_user_submissions: g.allow_user_submissions,
            writeup_required: g.writeup_required,
            invite_code: g.invite_code.clone(),
            team_member_count_limit: g.team_member_count_limit,
            container_count_limit: g.container_count_limit,
            discord_webhook: g.discord_webhook.clone(),
            poster_url: g.poster_url(),
            public_key: g.public_key.clone(),
            practice_mode: g.practice_mode,
            start_time_utc: g.start_time_utc,
            end_time_utc: g.end_time_utc,
            freeze_time_utc: g.freeze_time_utc,
            writeup_deadline: g.writeup_deadline,
            writeup_note: g.writeup_note.clone(),
            blood_bonus_value: g.blood_bonus_value,
            ad_warmup_seconds: g.ad_warmup_seconds,
            ad_snapshot_retention_days: g.ad_snapshot_retention_days,
            ad_tick_seconds: g.ad_tick_seconds,
            ad_flag_lifetime_ticks: g.ad_flag_lifetime_ticks,
            ad_reset_cooldown_minutes: g.ad_reset_cooldown_minutes,
            ad_allow_snapshot_download: Some(g.ad_allow_snapshot_download),
            ad_getflag_window_fraction: g.ad_getflag_window_fraction,
            ad_min_grace_period_seconds: g.ad_min_grace_period_seconds,
            ad_epoch_ticks: Some(g.ad_epoch_ticks),
            koth_epoch_ticks: Some(g.koth_epoch_ticks),
            koth_cycle_ticks: Some(g.koth_cycle_ticks),
            koth_champion_cooldown_ticks: Some(g.koth_champion_cooldown_ticks),
            koth_claim_confirmation_ticks: Some(g.koth_claim_confirmation_ticks),
            ad_scoring_start_round: g.ad_scoring_start_round,
            koth_scoring_start_round: g.koth_scoring_start_round,
            vpn_access_required: g.vpn_access_required,
            vpn_behavior_telemetry_enabled: g.vpn_behavior_telemetry_enabled,
            vpn_flag_scan_enabled: g.vpn_flag_scan_enabled,
            vpn_provider_dns_telemetry_enabled: g.vpn_provider_dns_telemetry_enabled,
            vpn_source_asn_telemetry_enabled: g.vpn_source_asn_telemetry_enabled,
            vpn_device_sharing_telemetry_enabled: g.vpn_device_sharing_telemetry_enabled,
            server_time: Some(Utc::now()),
            vpn_policy_change_reason: None,
        }
    }

    fn configuration(&self) -> crate::services::game_config::GameConfiguration {
        crate::services::game_config::GameConfiguration {
            start_time_utc: self.start_time_utc,
            end_time_utc: self.end_time_utc,
            freeze_time_utc: self.freeze_time_utc,
            team_member_count_limit: self.team_member_count_limit,
            container_count_limit: self.container_count_limit,
            ad_warmup_seconds: self.ad_warmup_seconds,
            ad_snapshot_retention_days: self.ad_snapshot_retention_days,
            ad_tick_seconds: self.ad_tick_seconds,
            ad_flag_lifetime_ticks: self.ad_flag_lifetime_ticks,
            ad_reset_cooldown_minutes: self.ad_reset_cooldown_minutes,
            ad_getflag_window_fraction: self.ad_getflag_window_fraction,
            ad_min_grace_period_seconds: self.ad_min_grace_period_seconds,
            ad_epoch_ticks: self.ad_epoch_ticks.unwrap_or(8),
            koth_epoch_ticks: self.koth_epoch_ticks.unwrap_or(12),
            koth_cycle_ticks: self.koth_cycle_ticks.unwrap_or(3),
            koth_champion_cooldown_ticks: self.koth_champion_cooldown_ticks.unwrap_or(1),
            koth_claim_confirmation_ticks: self.koth_claim_confirmation_ticks.unwrap_or(2),
        }
    }

    fn validate(&self) -> AppResult<Option<String>> {
        self.configuration().validate()?;
        crate::services::discord_webhook::normalize_discord_webhook(self.discord_webhook.as_deref())
    }

    fn validate_event_security(&self, st: &SharedState) -> AppResult<()> {
        let telemetry = self.vpn_behavior_telemetry_enabled
            || self.vpn_flag_scan_enabled
            || self.vpn_provider_dns_telemetry_enabled
            || self.vpn_source_asn_telemetry_enabled
            || self.vpn_device_sharing_telemetry_enabled;
        if telemetry && !self.vpn_access_required {
            return Err(AppError::bad_request(
                "Event VPN telemetry requires the per-event VPN access policy",
            ));
        }
        if self.vpn_access_required {
            if !crate::services::ad_vpn::enabled() {
                return Err(AppError::bad_request(
                    "Event VPN access requires RSCTF_AD_VPN_ENABLED=true",
                ));
            }
            crate::services::event_security::validate_credential_key(
                &st.config.event_vpn_credential_key,
            )?;
            crate::services::event_security::proof_url(self.id.max(1))?;
        }
        if telemetry
            && (st.config.event_sensor_token.len() < 32
                || st
                    .config
                    .event_sensor_token
                    .chars()
                    .any(char::is_whitespace))
        {
            return Err(AppError::bad_request(
                "Event VPN telemetry requires RSCTF_EVENT_SENSOR_TOKEN",
            ));
        }
        Ok(())
    }
}

pub(super) async fn validate_koth_game_shape_locked(
    conn: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<()> {
    let (
        koth_epoch_ticks,
        koth_cycle_ticks,
        koth_champion_cooldown_ticks,
        koth_claim_confirmation_ticks,
    ): (i32, i32, i32, i32) = sqlx::query_as(
        r#"SELECT koth_epoch_ticks, koth_cycle_ticks,
                  koth_champion_cooldown_ticks,
                  koth_claim_confirmation_ticks
             FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(game_id)
    .fetch_one(conn)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    validate_koth_crown_shape(
        koth_epoch_ticks,
        koth_cycle_ticks,
        koth_champion_cooldown_ticks,
        koth_claim_confirmation_ticks,
    )
}

/// `GET /api/edit/games` — RSCTF `EditController.GetGames` (`[RequireUser]`): an
/// Admin sees ALL games; a non-admin sees ONLY the games they co-manage (a
/// `game_manager` row), and 403s when they manage none.
pub async fn get_games(
    State(st): State<SharedState>,
    user: CurrentUser,
    axum::extract::Query(page): axum::extract::Query<PageParams>,
) -> AppResult<ArrayResponse<GameInfoModel>> {
    if user.is_admin() {
        let total = game::Entity::find().count(&st.db).await? as i64;
        let games = game::Entity::find()
            .order_by_desc(game::Column::StartTimeUtc)
            .offset(page.skip)
            .limit(page.limit())
            .all(&st.db)
            .await?;
        let data = games.iter().map(GameInfoModel::from_game).collect();
        return Ok(ArrayResponse::new(data, total));
    }

    // Non-admin: restrict to the games this user manages; 403 if they manage none.
    let managed_ids: Vec<i32> = game_manager::Entity::find()
        .filter(game_manager::Column::UserId.eq(user.id))
        .all(&st.db)
        .await?
        .into_iter()
        .map(|m| m.game_id)
        .collect();
    if managed_ids.is_empty() {
        return Err(AppError::Forbidden);
    }
    let total = managed_ids.len() as i64;
    let games = game::Entity::find()
        .filter(game::Column::Id.is_in(managed_ids))
        .order_by_desc(game::Column::StartTimeUtc)
        .offset(page.skip)
        .limit(page.limit())
        .all(&st.db)
        .await?;
    let data = games.iter().map(GameInfoModel::from_game).collect();
    Ok(ArrayResponse::new(data, total))
}

/// `GET /api/edit/games/{id}`
pub async fn get_game(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    manager_or_admin(&st, &user, id).await?;
    let g = load_game(&st, id).await?;
    Ok(RequestResponse::ok(GameInfoModel::from_game(&g)))
}

#[allow(clippy::too_many_arguments)]
fn validate_scoring_transition(
    current_epoch_ticks: i32,
    current_start_round: Option<i32>,
    current_lifetime: Option<i32>,
    current_tick_seconds: Option<i32>,
    current_getflag_fraction: Option<f64>,
    current_grace_seconds: Option<i32>,
    current_koth_start_round: Option<i32>,
    requested_epoch_ticks: i32,
    requested_lifetime: Option<i32>,
    requested_tick_seconds: Option<i32>,
    requested_getflag_fraction: Option<f64>,
    requested_grace_seconds: Option<i32>,
) -> AppResult<()> {
    let ad_scoring_started = current_start_round.is_some();
    let engine_scoring_started = ad_scoring_started || current_koth_start_round.is_some();
    if ad_scoring_started && requested_epoch_ticks != current_epoch_ticks {
        return Err(AppError::bad_request(
            "A&D epoch length is locked after A&D scoring has started.",
        ));
    }
    if current_start_round.is_some() && requested_lifetime != current_lifetime {
        return Err(AppError::bad_request(
            "A&D flag lifetime is locked after epoch scoring has started.",
        ));
    }
    if engine_scoring_started && requested_tick_seconds != current_tick_seconds {
        return Err(AppError::bad_request(
            "A&D/KotH tick timing is locked after epoch scoring has started.",
        ));
    }
    if engine_scoring_started
        && (requested_getflag_fraction != current_getflag_fraction
            || requested_grace_seconds != current_grace_seconds)
    {
        return Err(AppError::bad_request(
            "A&D/KotH checker sampling timing is locked after epoch scoring has started.",
        ));
    }
    Ok(())
}

fn validate_schedule_transition(
    current_start: DateTime<Utc>,
    current_end: DateTime<Utc>,
    requested_start: DateTime<Utc>,
    requested_end: DateTime<Utc>,
    activity_started: bool,
    evidence_closed: bool,
    koth_config_snapshotted: bool,
) -> AppResult<()> {
    let start_changed = requested_start != current_start;
    let end_changed = requested_end != current_end;
    if !start_changed && !end_changed {
        return Ok(());
    }
    if evidence_closed {
        return Err(AppError::bad_request(
            "The event schedule is locked after competitive evidence has closed.",
        ));
    }
    if koth_config_snapshotted {
        return Err(AppError::bad_request(
            "The event schedule is locked after KotH crown scoring starts.",
        ));
    }
    if start_changed && activity_started {
        return Err(AppError::bad_request(
            "The event start is locked after competitive activity has been recorded.",
        ));
    }
    if end_changed && activity_started && requested_end < current_end {
        return Err(AppError::bad_request(
            "The event end cannot be shortened after competitive activity has been recorded.",
        ));
    }
    Ok(())
}

/// A wall-clock crossing is reversible when an event has remained idle: an
/// organizer may have opened the event with the wrong time zone and need to
/// move it back into the future. Once any gameplay, audit, or engine evidence
/// exists, changing the start would reinterpret its competition window.
async fn schedule_activity_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
) -> AppResult<(bool, bool)> {
    sqlx::query_as::<_, (bool, bool)>(
        r#"SELECT (
                    game.ad_scoring_start_round IS NOT NULL
                    OR game.koth_scoring_start_round IS NOT NULL
                    OR EXISTS (SELECT 1 FROM "Submissions" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "GameEvents" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "AdRounds" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "KothCrownCycles" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "KothControlResults" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "KothAcquisitions" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "IdentityObservations" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "SuspicionEvents" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "AntiCheatFindings" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "SuspicionEvaluationOutbox" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "ContainerAccessEvents" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "FlagEgressEvents" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "VpnFlowTelemetryBuckets" WHERE game_id = game.id)
                    OR EXISTS (SELECT 1 FROM "VpnFlagTransportEvents" WHERE game_id = game.id)
                ),
                EXISTS (
                    SELECT 1
                      FROM "SuspicionReconciliationState" state
                     WHERE state.game_id = game.id
                       AND (state.evidence_closed_at_utc IS NOT NULL
                            OR state.sealed_at_utc IS NOT NULL)
                )
           FROM "Games" game
          WHERE game.id = $1"#,
    )
    .bind(game_id)
    .fetch_optional(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))
}

/// `PUT /api/edit/games/{id}`
pub async fn update_game(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<GameInfoModel>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    manager_or_admin(&st, &user, id).await?;
    let discord_webhook = model.validate()?;
    model.validate_event_security(&st)?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    let tx = control.transaction_mut();
    // Global lock order is game-control -> A&D rollup -> KotH rollup -> table
    // rows. Both materializers hold their advisory lock while checking the game
    // FK, so update paths must acquire both before `Games FOR UPDATE`.
    crate::services::ad::scoring::lock_epoch_rollups(&mut *tx, id).await?;
    crate::controllers::game::koth::lock_epoch_rollups(&mut *tx, id).await?;
    let (
        current_epoch_ticks,
        current_start_round,
        current_lifetime,
        current_tick_seconds,
        current_getflag_fraction,
        current_grace_seconds,
        current_koth_start_round,
        current_koth_epoch_ticks,
        current_koth_cycle_ticks,
        current_koth_champion_cooldown_ticks,
        current_koth_claim_confirmation_ticks,
        current_start_time,
        current_end_time,
        current_practice_mode,
        current_blood_bonus_value,
        deletion_pending,
    ) = sqlx::query_as::<
        _,
        (
            i32,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            Option<f64>,
            Option<i32>,
            Option<i32>,
            i32,
            i32,
            i32,
            i32,
            DateTime<Utc>,
            DateTime<Utc>,
            bool,
            i64,
            bool,
        ),
    >(
        r#"SELECT ad_epoch_ticks, ad_scoring_start_round,
                      ad_flag_lifetime_ticks, ad_tick_seconds,
                      ad_getflag_window_fraction, ad_min_grace_period_seconds,
                      koth_scoring_start_round,
                      koth_epoch_ticks, koth_cycle_ticks,
                      koth_champion_cooldown_ticks,
                      koth_claim_confirmation_ticks,
                      start_time_utc, end_time_utc, practice_mode,
                      blood_bonus_value, deletion_pending
                 FROM "Games"
                WHERE id = $1
                FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    if deletion_pending {
        return Err(AppError::conflict("Game is being deleted"));
    }
    let current_freeze_time: Option<DateTime<Utc>> =
        sqlx::query_scalar(r#"SELECT freeze_time_utc FROM "Games" WHERE id = $1"#)
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;

    let current_vpn_policy: (bool, bool, bool, bool, bool, bool, i64) = sqlx::query_as(
        r#"SELECT vpn_access_required, vpn_behavior_telemetry_enabled,
                  vpn_flag_scan_enabled, vpn_provider_dns_telemetry_enabled,
                  vpn_source_asn_telemetry_enabled,
                  vpn_device_sharing_telemetry_enabled, vpn_policy_revision
             FROM "Games" WHERE id = $1"#,
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    let requested_vpn_policy = (
        model.vpn_access_required,
        model.vpn_behavior_telemetry_enabled,
        model.vpn_flag_scan_enabled,
        model.vpn_provider_dns_telemetry_enabled,
        model.vpn_source_asn_telemetry_enabled,
        model.vpn_device_sharing_telemetry_enabled,
    );
    let vpn_policy_changed = requested_vpn_policy
        != (
            current_vpn_policy.0,
            current_vpn_policy.1,
            current_vpn_policy.2,
            current_vpn_policy.3,
            current_vpn_policy.4,
            current_vpn_policy.5,
        );
    let vpn_policy_reason = if vpn_policy_changed {
        let reason = model
            .vpn_policy_change_reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| (8..=512).contains(&reason.len()))
            .ok_or_else(|| {
                AppError::bad_request(
                    "A reason of 8 to 512 characters is required for VPN policy changes.",
                )
            })?;
        Some(reason)
    } else {
        None
    };
    let requested_vpn_revision = current_vpn_policy.6 + i64::from(vpn_policy_changed);

    // Normal submissions hold the Games row FOR SHARE through commit. This
    // FOR UPDATE therefore waits for every in-flight FirstSolve, blocks new
    // ones, and makes the following immutable-boundary decision race-free.
    let competition_scoring_started = competition_scoring_started_locked(&mut *tx, id).await?;

    let requested_epoch_ticks = model.ad_epoch_ticks.unwrap_or(current_epoch_ticks);
    validate_scoring_transition(
        current_epoch_ticks,
        current_start_round,
        current_lifetime,
        current_tick_seconds,
        current_getflag_fraction,
        current_grace_seconds,
        current_koth_start_round,
        requested_epoch_ticks,
        model.ad_flag_lifetime_ticks,
        model.ad_tick_seconds,
        model.ad_getflag_window_fraction,
        model.ad_min_grace_period_seconds,
    )?;
    let requested_koth_epoch_ticks = model.koth_epoch_ticks.unwrap_or(current_koth_epoch_ticks);
    let requested_koth_cycle_ticks = model.koth_cycle_ticks.unwrap_or(current_koth_cycle_ticks);
    let requested_koth_champion_cooldown_ticks = model
        .koth_champion_cooldown_ticks
        .unwrap_or(current_koth_champion_cooldown_ticks);
    let requested_koth_claim_confirmation_ticks = model
        .koth_claim_confirmation_ticks
        .unwrap_or(current_koth_claim_confirmation_ticks);
    let constant_scoring_settings_changed = model.practice_mode != current_practice_mode
        || super::blood_bonus_from_value(model.blood_bonus_value) != current_blood_bonus_value
        || requested_epoch_ticks != current_epoch_ticks
        || model.ad_flag_lifetime_ticks != current_lifetime
        || model.ad_tick_seconds != current_tick_seconds
        || model.ad_getflag_window_fraction != current_getflag_fraction
        || model.ad_min_grace_period_seconds != current_grace_seconds
        || requested_koth_epoch_ticks != current_koth_epoch_ticks
        || requested_koth_cycle_ticks != current_koth_cycle_ticks
        || requested_koth_champion_cooldown_ticks != current_koth_champion_cooldown_ticks
        || requested_koth_claim_confirmation_ticks != current_koth_claim_confirmation_ticks;
    if competition_scoring_started && constant_scoring_settings_changed {
        return Err(AppError::bad_request(
            "Game scoring settings are locked after competition scoring has started.",
        ));
    }
    validate_koth_crown_shape(
        requested_koth_epoch_ticks,
        requested_koth_cycle_ticks,
        requested_koth_champion_cooldown_ticks,
        requested_koth_claim_confirmation_ticks,
    )?;
    let schedule_changed =
        model.start_time_utc != current_start_time || model.end_time_utc != current_end_time;
    let delivery_schedule_changed =
        schedule_changed || model.freeze_time_utc != current_freeze_time;
    let config_snapshotted = if schedule_changed {
        let config_snapshotted: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "KothOfficialConfigs" WHERE game_id = $1)"#,
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        config_snapshotted
    } else {
        false
    };
    let (schedule_activity_started, evidence_closed) = if schedule_changed {
        schedule_activity_locked(&mut *tx, id).await?
    } else {
        (false, false)
    };
    validate_schedule_transition(
        current_start_time,
        current_end_time,
        model.start_time_utc,
        model.end_time_utc,
        schedule_activity_started,
        evidence_closed,
        config_snapshotted,
    )?;
    if current_koth_start_round.is_some()
        && (requested_koth_epoch_ticks != current_koth_epoch_ticks
            || requested_koth_cycle_ticks != current_koth_cycle_ticks
            || requested_koth_champion_cooldown_ticks != current_koth_champion_cooldown_ticks
            || requested_koth_claim_confirmation_ticks != current_koth_claim_confirmation_ticks)
    {
        return Err(AppError::bad_request(
            "KotH crown-cycle settings are locked after epoch scoring has started.",
        ));
    }
    crate::services::ad::scoring::invalidate_rollups_for_end_change(
        &mut *tx,
        id,
        current_end_time,
        model.end_time_utc,
    )
    .await?;
    crate::controllers::game::koth::invalidate_rollups_for_end_change(
        &mut *tx,
        id,
        current_end_time,
        model.end_time_utc,
    )
    .await?;
    // A closeout may have sealed the latest round while its nominal tick was
    // still open. Reopen that exact round and invalidate only platform-generated
    // closeout evidence; real checker samples remain immutable.
    reopen_latest_round_for_end_extension(&mut *tx, id, current_end_time, model.end_time_utc)
        .await?;

    sqlx::query(
        r#"UPDATE "Games" SET
               title = $2, content = $3, summary = $4, hidden = $5,
               practice_mode = $6, accept_without_review = $7,
               allow_user_submissions = $8, invite_code = $9,
               start_time_utc = $10, end_time_utc = $11,
               team_member_count_limit = $12, container_count_limit = $13,
               writeup_note = $14, writeup_required = $15,
               writeup_deadline = $16, freeze_time_utc = $17,
               blood_bonus_value = $18, discord_webhook = $19,
               ad_warmup_seconds = $20,
               ad_snapshot_retention_days = $21,
               ad_tick_seconds = $22,
               ad_flag_lifetime_ticks = $23,
               ad_reset_cooldown_minutes = $24,
               ad_allow_snapshot_download = COALESCE($25, ad_allow_snapshot_download),
               ad_getflag_window_fraction = $26,
               ad_min_grace_period_seconds = $27,
               ad_epoch_ticks = $28, ad_scoring_start_round = $29,
               koth_epoch_ticks = $30, koth_cycle_ticks = $31,
               koth_champion_cooldown_ticks = $32,
               koth_claim_confirmation_ticks = $33,
               vpn_access_required = $34,
               vpn_behavior_telemetry_enabled = $35,
               vpn_flag_scan_enabled = $36,
               vpn_provider_dns_telemetry_enabled = $37,
               vpn_source_asn_telemetry_enabled = $38,
               vpn_device_sharing_telemetry_enabled = $39,
               vpn_policy_revision = $40
             WHERE id = $1"#,
    )
    .bind(id)
    .bind(&model.title)
    .bind(&model.content)
    .bind(&model.summary)
    .bind(model.hidden)
    .bind(model.practice_mode)
    .bind(model.accept_without_review)
    .bind(model.allow_user_submissions)
    .bind(&model.invite_code)
    .bind(model.start_time_utc)
    .bind(model.end_time_utc)
    .bind(model.team_member_count_limit)
    .bind(model.container_count_limit)
    .bind(&model.writeup_note)
    .bind(model.writeup_required)
    .bind(model.writeup_deadline)
    .bind(model.freeze_time_utc)
    .bind(super::blood_bonus_from_value(model.blood_bonus_value))
    .bind(&discord_webhook)
    .bind(model.ad_warmup_seconds)
    .bind(model.ad_snapshot_retention_days)
    .bind(model.ad_tick_seconds)
    .bind(model.ad_flag_lifetime_ticks)
    .bind(model.ad_reset_cooldown_minutes)
    .bind(model.ad_allow_snapshot_download)
    .bind(model.ad_getflag_window_fraction)
    .bind(model.ad_min_grace_period_seconds)
    .bind(requested_epoch_ticks)
    .bind(current_start_round)
    .bind(requested_koth_epoch_ticks)
    .bind(requested_koth_cycle_ticks)
    .bind(requested_koth_champion_cooldown_ticks)
    .bind(requested_koth_claim_confirmation_ticks)
    .bind(model.vpn_access_required)
    .bind(model.vpn_behavior_telemetry_enabled)
    .bind(model.vpn_flag_scan_enabled)
    .bind(model.vpn_provider_dns_telemetry_enabled)
    .bind(model.vpn_source_asn_telemetry_enabled)
    .bind(model.vpn_device_sharing_telemetry_enabled)
    .bind(requested_vpn_revision)
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if delivery_schedule_changed {
        crate::services::discord_webhook::reschedule_game_blood_notices(
            tx,
            id,
            current_freeze_time,
            current_end_time,
            model.freeze_time_utc,
            model.end_time_utc,
        )
        .await?;
    }
    if let Some(reason) = vpn_policy_reason {
        let old_policy = serde_json::json!({
            "accessRequired": current_vpn_policy.0,
            "behaviorTelemetry": current_vpn_policy.1,
            "flagScan": current_vpn_policy.2,
            "providerDnsTelemetry": current_vpn_policy.3,
            "sourceAsnTelemetry": current_vpn_policy.4,
            "deviceSharingTelemetry": current_vpn_policy.5,
        });
        let new_policy = serde_json::json!({
            "accessRequired": model.vpn_access_required,
            "behaviorTelemetry": model.vpn_behavior_telemetry_enabled,
            "flagScan": model.vpn_flag_scan_enabled,
            "providerDnsTelemetry": model.vpn_provider_dns_telemetry_enabled,
            "sourceAsnTelemetry": model.vpn_source_asn_telemetry_enabled,
            "deviceSharingTelemetry": model.vpn_device_sharing_telemetry_enabled,
        });
        sqlx::query(
            r#"INSERT INTO "EventVpnPolicyAudit"
                 (game_id, actor_user_id, old_revision, new_revision,
                  old_policy, new_policy, reason)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(id)
        .bind(user.id)
        .bind(current_vpn_policy.6)
        .bind(requested_vpn_revision)
        .bind(sqlx::types::Json(old_policy))
        .bind(sqlx::types::Json(new_policy))
        .bind(reason)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    }
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    crate::controllers::game::invalidate_game_row_cache(id);
    crate::services::event_security::invalidate_policy(&st, id).await;
    flush_game_scoreboards(&st, id).await;
    let updated = load_game(&st, id).await?;
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    Ok(RequestResponse::ok(GameInfoModel::from_game(&updated)))
}

#[cfg(test)]
#[path = "games_config_tests.rs"]
mod scoring_transition_tests;

mod deletion;
use deletion::{delete_ad_game_data, fence_game_for_deletion};

#[cfg(test)]
#[path = "games_deletion_tests.rs"]
mod deletion_tests;

/// `DELETE /api/edit/games/{id}` — returns the deleted game (contract:
/// `GameInfoModel`, not void).
pub async fn delete_game(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    // Admit before the first game transaction. The permit survives the slow
    // runtime sweep and moves into the final deletion lock guard, so queued
    // hard deletes never consume pool connections while waiting.
    let deletion_admission = super::deletion_locks::acquire_hard_deletion_admission().await?;
    let mut control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    let g = load_game(&st, id).await?;
    // Reject irreversible deletion before touching event state. The marker and
    // history predicate share the game transaction and all challenge submission
    // fences, so an accepted submit cannot slip between the check and commit.
    fence_game_for_deletion(control.transaction_mut(), id).await?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    // The durable fence is a point of no return even if external teardown must
    // be retried. Hide the now-partially-deleting event from every cached play
    // surface before touching Docker, VPN, or blob storage.
    crate::controllers::game::invalidate_game_row_cache(id);
    flush_game_scoreboards(&st, id).await;
    crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    // Reap every running container the game owns (per-team instances + per-
    // challenge test/shared containers) before the rows cascade away, so the
    // backend isn't left with orphans it can no longer resolve.
    destroy_game_containers(&st, id).await?;
    let mut deletion_locks =
        super::deletion_locks::acquire_game_test_deletion_locks(&st.db, id, deletion_admission)
            .await?;
    destroy_game_test_containers_locked(&st, id).await?;
    let tx = deletion_locks.game_transaction_mut();
    // A concurrent administrative/runtime writer may have committed while slow
    // backend teardown held no game lock. Re-fence before the first evidence
    // delete; a conflict leaves every durable competition row intact.
    fence_game_for_deletion(tx, id).await?;
    // Match the global writer order used by update/materialization paths before
    // deleting rollups or the Games row they reference.
    crate::services::ad::scoring::lock_epoch_rollups(&mut *tx, id).await?;
    crate::controllers::game::koth::lock_epoch_rollups(&mut *tx, id).await?;
    delete_ad_game_data(tx, id).await?;
    let deleted_challenge_artifacts =
        crate::services::blob_refs::delete_game_challenges_locked(tx, id).await?;
    let poster_hash = sqlx::query_scalar::<_, Option<String>>(
        r#"SELECT poster_hash FROM "Games" WHERE id = $1 FOR UPDATE"#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    let deleted = sqlx::query(r#"DELETE FROM "Games" WHERE id = $1"#)
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::not_found("Game not found"));
    }
    if let Some(hash) = poster_hash.as_deref() {
        crate::services::blob_refs::release_direct_hash_locked(tx, hash).await?;
    }
    deletion_locks.release().await?;
    crate::services::blob_refs::purge_deleted_challenge_artifacts(
        st.pg(),
        st.storage.as_ref(),
        &deleted_challenge_artifacts,
    )
    .await;
    for attachment_id in deleted_challenge_artifacts.attachment_ids {
        if let Err(error) = delete_attachment(&st, attachment_id).await {
            tracing::warn!(%error, attachment_id, "deleted game attachment cleanup deferred");
        }
    }
    if let Some(hash) = poster_hash {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "deleted game poster cleanup deferred");
        }
    }
    crate::controllers::game::invalidate_game_row_cache(id);
    flush_game_scoreboards(&st, id).await;
    // `serverTime` is a response-creation sample. Build the response model only
    // after every potentially slow container, VPN, and blob teardown completes.
    Ok(RequestResponse::ok(GameInfoModel::from_game(&g)))
}

/// `GET /api/edit/games/{id}/HashSalt` — the per-game team-hash salt
/// (`Game.TeamHashSalt` = `sha256("RSCTF@{PrivateKey}@PK")`). Contract: raw
/// `string`.
pub async fn get_hash_salt(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<String>> {
    manager_or_admin(&st, &user, id).await?;
    let g = load_game(&st, id).await?;
    let salt = sha256_str(&format!("RSCTF@{}@PK", g.private_key));
    Ok(RequestResponse::ok(salt))
}

mod cloning;
#[cfg(test)]
use cloning::apply_clone_challenge_defaults;
pub use cloning::{clone_game, delete_writeups};
