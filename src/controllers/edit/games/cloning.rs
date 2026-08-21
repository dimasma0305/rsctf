//! Aggregate-safe game cloning and writeup cleanup.

use super::*;

pub(super) fn apply_clone_challenge_defaults(clone: &mut game_challenge::ActiveModel) {
    clone.enable_shared_container = Set(false);
    clone.score_curve = Set(ScoreCurve::Standard);
    clone.network_mode = Set(Some(NetworkMode::Open));
    clone.ad_allow_egress = Set(false);
    clone.ad_allow_self_reset = Set(false);
    clone.ad_ssh_requires_flag = Set(false);
    clone.ad_self_hosted = Set(false);
}

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

    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let transaction = st.db.begin().await?;
    let new_game = game::ActiveModel {
        title: Set(model.title.trim().to_string()),
        public_key: Set(public_key),
        private_key: Set(private_key),
        summary: Set(source.summary.clone()),
        content: Set(source.content.clone()),
        practice_mode: Set(source.practice_mode),
        accept_without_review: Set(source.accept_without_review),
        allow_user_submissions: Set(false),
        writeup_required: Set(source.writeup_required),
        writeup_note: Set(source.writeup_note.clone()),
        team_member_count_limit: Set(source.team_member_count_limit),
        container_count_limit: Set(source.container_count_limit),
        blood_bonus_value: Set(source.blood_bonus_value),
        start_time_utc: Set(model.start_time_utc),
        end_time_utc: Set(model.end_time_utc),
        writeup_deadline: Set(super::super::epoch()),
        hidden: Set(true),
        ad_allow_snapshot_download: Set(true),
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
        vpn_access_required: Set(source.vpn_access_required),
        vpn_behavior_telemetry_enabled: Set(source.vpn_behavior_telemetry_enabled),
        vpn_flag_scan_enabled: Set(source.vpn_flag_scan_enabled),
        vpn_provider_dns_telemetry_enabled: Set(source.vpn_provider_dns_telemetry_enabled),
        vpn_source_asn_telemetry_enabled: Set(source.vpn_source_asn_telemetry_enabled),
        vpn_device_sharing_telemetry_enabled: Set(source.vpn_device_sharing_telemetry_enabled),
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
            variant_mode: Set(src.variant_mode),
            variant_generator_image: Set(src.variant_generator_image.clone()),
            variant_generator_digest: Set(src.variant_generator_digest.clone()),
            variant_generator_build_context_subdir: Set(src
                .variant_generator_build_context_subdir
                .clone()),
            variant_generator_build_status: Set(src.variant_generator_build_status),
            variant_generator_last_build_log: Set(src.variant_generator_last_build_log.clone()),
            solve_receipt_mode: Set(src.solve_receipt_mode),
            receipt_verifier_identity: Set(src.receipt_verifier_identity.clone()),
            ..Default::default()
        };
        apply_clone_challenge_defaults(&mut clone);
        let clone = clone.insert(&transaction).await?;
        let flags = flag_context::Entity::find()
            .filter(flag_context::Column::ChallengeId.eq(src.id))
            .all(&transaction)
            .await?;
        for flag in flags {
            flag_context::ActiveModel {
                flag: Set(flag.flag),
                is_occupied: Set(false),
                challenge_id: Set(Some(clone.id)),
                ..Default::default()
            }
            .insert(&transaction)
            .await?;
        }
    }
    transaction.commit().await?;
    Ok(RequestResponse::ok(new_game.id))
}

pub async fn delete_writeups(
    State(st): State<SharedState>,
    _admin: AdminUser,
    Path(id): Path<i32>,
) -> AppResult<RequestResponse<GameInfoModel>> {
    let game = load_game(&st, id).await?;
    let deleted_hashes = crate::services::blob_refs::clear_game_writeups(st.pg(), id).await?;
    for hash in deleted_hashes {
        if let Err(error) =
            crate::services::blob_refs::purge_if_unreferenced(st.pg(), st.storage.as_ref(), &hash)
                .await
        {
            tracing::warn!(%error, %hash, "deleted game writeup purge failed");
        }
    }
    Ok(RequestResponse::ok(GameInfoModel::from_game(&game)))
}
