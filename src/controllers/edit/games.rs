//! edit: game CRUD/clone/writeups (see edit/mod.rs for the router + shared DTOs/helpers).
use super::*;

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

    fn validate(&self) -> AppResult<()> {
        self.configuration().validate()
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

fn apply_ad_creation_settings(model: &GameInfoModel, active: &mut game::ActiveModel) {
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

/// `POST /api/edit/games` — create with a fresh key pair + defaults.
pub async fn add_game(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Json(model): Json<GameInfoModel>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    model.validate()?;
    let koth_epoch_ticks = model.koth_epoch_ticks.unwrap_or(12);
    let koth_cycle_ticks = model.koth_cycle_ticks.unwrap_or(3);
    let koth_champion_cooldown_ticks = model.koth_champion_cooldown_ticks.unwrap_or(1);
    let koth_claim_confirmation_ticks = model.koth_claim_confirmation_ticks.unwrap_or(2);

    // NOTE: RSCTF generates an Ed25519 key pair here (Game.GenerateKeyPair).
    // The Ed25519 crate is not in this port's dependency set, so this is a
    // random placeholder — it is NOT a real signing key.
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();

    let mut am = game::ActiveModel {
        title: Set(model.title.clone()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        hidden: Set(model.hidden),
        practice_mode: Set(model.practice_mode),
        summary: Set(model.summary.clone()),
        content: Set(model.content.clone()),
        accept_without_review: Set(model.accept_without_review),
        allow_user_submissions: Set(model.allow_user_submissions),
        writeup_required: Set(model.writeup_required),
        invite_code: Set(model.invite_code.clone()),
        team_member_count_limit: Set(model.team_member_count_limit),
        discord_webhook: Set(model.discord_webhook.clone()),
        container_count_limit: Set(model.container_count_limit),
        start_time_utc: Set(model.start_time_utc),
        end_time_utc: Set(model.end_time_utc),
        writeup_deadline: Set(model.writeup_deadline),
        freeze_time_utc: Set(model.freeze_time_utc),
        writeup_note: Set(model.writeup_note.clone()),
        blood_bonus_value: Set(super::blood_bonus_from_value(model.blood_bonus_value)),
        koth_epoch_ticks: Set(koth_epoch_ticks),
        koth_cycle_ticks: Set(koth_cycle_ticks),
        koth_champion_cooldown_ticks: Set(koth_champion_cooldown_ticks),
        koth_claim_confirmation_ticks: Set(koth_claim_confirmation_ticks),
        // A newly-created game is still a template: challenge configuration remains
        // mutable until the first round with a real A&D roster declares the boundary.
        ad_scoring_start_round: Set(None),
        ad_scoring_paused: Set(false),
        ..Default::default()
    };
    apply_ad_creation_settings(&model, &mut am);
    let created = am.insert(&st.db).await?;
    Ok(RequestResponse::ok(GameInfoModel::from_game(&created)))
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

fn validate_start_time_transition(
    current: DateTime<Utc>,
    requested: DateTime<Utc>,
    scoring_started: bool,
) -> AppResult<()> {
    if scoring_started && requested != current {
        return Err(AppError::bad_request(
            "The event start is locked after A&D or KotH scoring starts.",
        ));
    }
    Ok(())
}

/// `PUT /api/edit/games/{id}`
pub async fn update_game(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<GameInfoModel>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    manager_or_admin(&st, &user, id).await?;
    model.validate()?;
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
        ),
    >(
        r#"SELECT ad_epoch_ticks, ad_scoring_start_round,
                      ad_flag_lifetime_ticks, ad_tick_seconds,
                      ad_getflag_window_fraction, ad_min_grace_period_seconds,
                      koth_scoring_start_round,
                      koth_epoch_ticks, koth_cycle_ticks,
                      koth_champion_cooldown_ticks,
                      koth_claim_confirmation_ticks,
                      start_time_utc, end_time_utc, deletion_pending
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
    validate_koth_crown_shape(
        requested_koth_epoch_ticks,
        requested_koth_cycle_ticks,
        requested_koth_champion_cooldown_ticks,
        requested_koth_claim_confirmation_ticks,
    )?;
    let schedule_changed =
        model.start_time_utc != current_start_time || model.end_time_utc != current_end_time;
    let config_snapshotted = if schedule_changed {
        let config_snapshotted: bool = sqlx::query_scalar(
            r#"SELECT EXISTS(SELECT 1 FROM "KothOfficialConfigs" WHERE game_id = $1)"#,
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        if model.end_time_utc != current_end_time && config_snapshotted {
            return Err(AppError::bad_request(
                "The event deadline is locked after KotH crown scoring starts.",
            ));
        }
        config_snapshotted
    } else {
        false
    };
    validate_start_time_transition(
        current_start_time,
        model.start_time_utc,
        current_start_round.is_some() || current_koth_start_round.is_some() || config_snapshotted,
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
               koth_claim_confirmation_ticks = $33
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
    .bind(&model.discord_webhook)
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
    .execute(&mut **tx)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    crate::controllers::game::invalidate_game_row_cache(id);
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
    let model = GameInfoModel::from_game(&g);
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
    Ok(RequestResponse::ok(model))
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

fn apply_clone_challenge_defaults(clone: &mut game_challenge::ActiveModel) {
    clone.enable_shared_container = Set(false);
    clone.score_curve = Set(ScoreCurve::Standard);
    clone.network_mode = Set(Some(NetworkMode::Open));
    clone.ad_allow_egress = Set(false);
    clone.ad_allow_self_reset = Set(false);
    clone.ad_ssh_requires_flag = Set(false);
    clone.ad_self_hosted = Set(false);
}

/// `POST /api/edit/games/{id}/Clone` — deep-copy a game (settings + challenges,
/// static flags included) into a new **hidden** template. Contract: raw new
/// game id (`number`). Mirrors `EditController.CloneGame`.
pub async fn clone_game(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
    Json(model): Json<GameCloneModel>,
) -> AppResult<RequestResponse<i32>> {
    let source_control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    let source = load_game(&st, id).await?;
    let sources = if model.include_challenges {
        game_challenge::Entity::find()
            .filter(game_challenge::Column::GameId.eq(id))
            .all(&st.db)
            .await?
    } else {
        Vec::new()
    };
    let mut clone_configuration = GameInfoModel::from_game(&source).configuration();
    clone_configuration.start_time_utc = model.start_time_utc;
    clone_configuration.end_time_utc = model.end_time_utc;
    clone_configuration.freeze_time_utc = None;
    clone_configuration.validate()?;
    source_control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    // Fresh key pair — copying the source's private key would collide the
    // TeamHashSalt across the two games. (Placeholder, as in add_game.)
    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    // A clone is one aggregate. If any challenge or flag cannot be copied, do
    // not leave a hidden, half-populated game behind.
    let transaction = st.db.begin().await?;

    let new_game = game::ActiveModel {
        title: Set(model.title.trim().to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        summary: Set(source.summary.clone()),
        content: Set(source.content.clone()),
        practice_mode: Set(source.practice_mode),
        accept_without_review: Set(source.accept_without_review),
        // RSCTF `CloneGame` copies neither of these — the clone starts from the
        // Game entity defaults (AllowUserSubmissions = false, WriteupDeadline =
        // Unix epoch), not the source's values.
        allow_user_submissions: Set(false),
        writeup_required: Set(source.writeup_required),
        writeup_note: Set(source.writeup_note.clone()),
        team_member_count_limit: Set(source.team_member_count_limit),
        container_count_limit: Set(source.container_count_limit),
        blood_bonus_value: Set(source.blood_bonus_value),
        start_time_utc: Set(model.start_time_utc),
        end_time_utc: Set(model.end_time_utc),
        writeup_deadline: Set(super::epoch()),
        hidden: Set(true),
        // RSCTF CloneGame does not copy AdAllowSnapshotDownload — the new Game
        // starts from the entity default (true).
        ad_allow_snapshot_download: Set(true),
        // Official score shape is template configuration, not historical state.
        ad_epoch_ticks: Set(source.ad_epoch_ticks),
        koth_epoch_ticks: Set(source.koth_epoch_ticks),
        koth_cycle_ticks: Set(source.koth_cycle_ticks),
        koth_champion_cooldown_ticks: Set(source.koth_champion_cooldown_ticks),
        koth_claim_confirmation_ticks: Set(source.koth_claim_confirmation_ticks),
        ad_warmup_seconds: Set(source.ad_warmup_seconds),
        ad_snapshot_retention_days: Set(source.ad_snapshot_retention_days),
        ad_tick_seconds: Set(source.ad_tick_seconds),
        ad_flag_lifetime_ticks: Set(source
            .ad_flag_lifetime_ticks
            .map(|ticks| ticks.clamp(1, 50))),
        ad_getflag_window_fraction: Set(source.ad_getflag_window_fraction),
        ad_min_grace_period_seconds: Set(source.ad_min_grace_period_seconds),
        ad_reset_cooldown_minutes: Set(source.ad_reset_cooldown_minutes),
        ad_scoring_start_round: Set(None),
        koth_scoring_start_round: Set(None),
        ad_scoring_paused: Set(false),
        ..Default::default()
    };
    let new_game = new_game.insert(&transaction).await?;

    for src in sources {
        let mut clone = game_challenge::ActiveModel {
            game_id: Set(new_game.id),
            title: Set(src.title.clone()),
            content: Set(src.content.clone()),
            category: Set(src.category),
            challenge_type: Set(src.challenge_type),
            hints: Set(src.hints.clone()),
            flag_template: Set(src.flag_template.clone()),
            file_name: Set(src.file_name.clone()),
            container_image: Set(src.container_image.clone()),
            network_mode: Set(src.network_mode),
            memory_limit: Set(src.memory_limit),
            storage_limit: Set(src.storage_limit),
            cpu_count: Set(src.cpu_count),
            expose_port: Set(src.expose_port),
            workload_spec: Set(src.workload_spec.clone()),
            enable_traffic_capture: Set(src.enable_traffic_capture),
            disable_blood_bonus: Set(src.disable_blood_bonus),
            original_score: Set(src.original_score),
            min_score_rate: Set(src.min_score_rate),
            difficulty: Set(src.difficulty),
            ad_scoring_weight: Set(src.ad_scoring_weight),
            submission_limit: Set(src.submission_limit),
            is_enabled: Set(false),
            accepted_count: Set(0),
            submission_count: Set(0),
            review_status: Set(ChallengeReviewStatus::Active),
            build_status: Set(ChallengeBuildStatus::None),
            // RSCTF CloneGame's GameChallenge whitelist copies neither
            // EnableSharedContainer, ScoreCurve, nor the operational AD knobs
            // (AdCheckerImage/AdAllowEgress/AdAllowSelfReset/
            // AdSshRequiresFlag/AdSelfHosted) — they stay at entity defaults.
            // The official scoring weight is intentionally preserved above.
            ..Default::default()
        };
        apply_clone_challenge_defaults(&mut clone);
        let clone = clone.insert(&transaction).await?;

        // Copy static flags (the flag text, not the attachment blob).
        let flags = flag_context::Entity::find()
            .filter(flag_context::Column::ChallengeId.eq(src.id))
            .all(&transaction)
            .await?;
        for f in flags {
            let am = flag_context::ActiveModel {
                flag: Set(f.flag),
                is_occupied: Set(false),
                challenge_id: Set(Some(clone.id)),
                ..Default::default()
            };
            am.insert(&transaction).await?;
        }
    }

    transaction.commit().await?;
    Ok(RequestResponse::ok(new_game.id))
}

/// `DELETE /api/edit/games/{id}/writeups` — clear submitted writeups; returns
/// the game (contract: `GameInfoModel`).
pub async fn delete_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    let g = load_game(&st, id).await?;

    let deleted_hashes = crate::services::blob_refs::clear_game_writeups(st.pg(), id).await?;
    for hash in deleted_hashes {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "deleted game writeup purge failed");
        }
    }
    Ok(RequestResponse::ok(GameInfoModel::from_game(&g)))
}
