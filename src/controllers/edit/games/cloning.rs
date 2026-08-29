//! Aggregate-safe game cloning and writeup cleanup.

use super::*;
use sha2::{Digest, Sha256};

const MAX_CLONE_CHALLENGES: i64 = 500;
const MAX_CLONE_FLAGS: i64 = 5_000;
const MAX_CLONE_TITLE_BYTES: usize = 128;
const MAX_CLONE_DURATION_DAYS: i64 = 366;

const CLONE_CHALLENGES_SQL: &str = r#"
WITH source AS MATERIALIZED (
    SELECT challenge.*,
           nextval(pg_get_serial_sequence('"GameChallenges"', 'id'))::integer AS clone_id
      FROM "GameChallenges" challenge
     WHERE challenge.game_id = $1
     ORDER BY challenge.id
), inserted AS (
    INSERT INTO "GameChallenges" (
        id, game_id, title, content, category, "Type", hints, is_enabled,
        deadline_utc, submission_limit, accepted_count, submission_count,
        container_image, memory_limit, storage_limit, cpu_count, expose_port,
        workload_spec, file_name, flag_template, review_status, review_note,
        submitted_by_user_id, submitted_at_utc, reviewed_at_utc,
        original_archive_blob_path, build_context_subdir, build_status,
        build_image_digest, last_build_log, source_yaml_path, attachment_id,
        test_container_id, enable_traffic_capture, enable_shared_container,
        disable_blood_bonus, original_score, min_score_rate, difficulty,
        score_curve, shared_container_id, network_mode, variant_mode,
        variant_generator_image, variant_generator_digest,
        variant_generator_build_context_subdir, variant_generator_build_status,
        variant_generator_last_build_log, solve_receipt_mode,
        receipt_verifier_identity, ad_checker_image, ad_allow_egress,
        ad_allow_self_reset, ad_ssh_requires_flag, ad_self_hosted,
        ad_scoring_weight
    )
    SELECT source.clone_id, $2, source.title, source.content, source.category,
           source."Type", source.hints, FALSE, NULL, source.submission_limit,
           0, 0, source.container_image, source.memory_limit,
           source.storage_limit, source.cpu_count, source.expose_port,
           source.workload_spec, source.file_name, source.flag_template, $3,
           NULL, NULL, NULL, NULL, NULL, NULL, $4, NULL, NULL, NULL, NULL,
           NULL, source.enable_traffic_capture, FALSE,
           source.disable_blood_bonus, source.original_score,
           source.min_score_rate, source.difficulty, $5, NULL, $6, $7,
           NULL, NULL, NULL, $4, NULL, source.solve_receipt_mode,
           source.receipt_verifier_identity, NULL, FALSE, FALSE, FALSE,
           FALSE, source.ad_scoring_weight
      FROM source
    RETURNING id
), copied_flags AS (
    INSERT INTO "FlagContexts" (
        flag, is_occupied, attachment_id, challenge_id, exercise_id
    )
    SELECT flag.flag, FALSE, NULL, source.clone_id, NULL
      FROM source
      JOIN "FlagContexts" flag ON flag.challenge_id = source.id
     WHERE EXISTS (SELECT 1 FROM inserted WHERE inserted.id = source.clone_id)
    RETURNING id
)
SELECT (SELECT COUNT(*) FROM inserted)::bigint,
       (SELECT COUNT(*) FROM copied_flags)::bigint
"#;

#[derive(sqlx::FromRow)]
struct CloneOperationRow {
    source_game_id: i32,
    requested_by: Uuid,
    request_digest: String,
    destination_game_id: Option<i32>,
    status: i16,
}

fn clone_request_digest(source_id: i32, model: &GameCloneModel, title: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(source_id.to_be_bytes());
    digest.update(model.expected_source_revision.to_be_bytes());
    digest.update(model.expected_challenge_revision.to_be_bytes());
    digest.update(title.as_bytes());
    digest.update(model.start_time_utc.timestamp_millis().to_be_bytes());
    digest.update(model.end_time_utc.timestamp_millis().to_be_bytes());
    digest.update([u8::from(model.include_challenges)]);
    hex::encode(digest.finalize())
}

fn validate_source_revisions(
    model: &GameCloneModel,
    source_revision: i64,
    challenge_revision: i64,
) -> AppResult<()> {
    if model.expected_source_revision != source_revision
        || model.expected_challenge_revision != challenge_revision
    {
        return Err(AppError::conflict(format!(
            "Source game changed before cloning (current revisions: {source_revision}/{challenge_revision})"
        )));
    }
    Ok(())
}

fn validate_clone_request(model: &GameCloneModel) -> AppResult<String> {
    if model.operation_id.is_nil() {
        return Err(AppError::bad_request(
            "A valid clone operation ID is required",
        ));
    }
    let title = model.title.trim();
    if !(3..=MAX_CLONE_TITLE_BYTES).contains(&title.len()) {
        return Err(AppError::bad_request(
            "Clone title must be between 3 and 128 bytes",
        ));
    }
    let duration = model.end_time_utc - model.start_time_utc;
    if duration <= chrono::Duration::zero()
        || duration > chrono::Duration::days(MAX_CLONE_DURATION_DAYS)
    {
        return Err(AppError::bad_request(
            "Clone end time must follow start time by no more than 366 days",
        ));
    }
    Ok(title.to_string())
}

async fn existing_clone_operation(
    pool: &sqlx::PgPool,
    operation_id: Uuid,
) -> AppResult<Option<CloneOperationRow>> {
    sqlx::query_as::<_, CloneOperationRow>(
        r#"SELECT source_game_id, requested_by, request_digest,
                  destination_game_id, status
             FROM "GameCloneOperations"
            WHERE operation_id = $1"#,
    )
    .bind(operation_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| AppError::internal(error.to_string()))
}

fn replay_clone_operation(
    operation: CloneOperationRow,
    source_id: i32,
    requested_by: Uuid,
    request_digest: &str,
) -> AppResult<Option<i32>> {
    if operation.source_game_id != source_id
        || operation.requested_by != requested_by
        || operation.request_digest != request_digest
    {
        return Err(AppError::conflict(
            "Clone operation ID is already bound to different input",
        ));
    }
    match (operation.status, operation.destination_game_id) {
        (1, Some(destination)) => Ok(Some(destination)),
        (0, _) => Err(AppError::conflict("Clone operation is already running")),
        _ => Err(AppError::conflict("Clone operation did not complete")),
    }
}

#[cfg(test)]
pub(super) fn apply_clone_challenge_defaults(clone: &mut game_challenge::ActiveModel) {
    clone.enable_shared_container = Set(false);
    clone.score_curve = Set(ScoreCurve::Standard);
    clone.network_mode = Set(Some(NetworkMode::Open));
    clone.ad_allow_egress = Set(false);
    clone.ad_allow_self_reset = Set(false);
    clone.ad_ssh_requires_flag = Set(false);
    clone.ad_self_hosted = Set(false);
    if matches!(&clone.variant_generator_build_context_subdir, Set(Some(_))) {
        clone.variant_mode = Set(ChallengeVariantMode::Disabled);
        clone.variant_generator_image = Set(None);
        clone.variant_generator_digest = Set(None);
        clone.variant_generator_build_context_subdir = Set(None);
        clone.variant_generator_build_status = Set(ChallengeBuildStatus::None);
        clone.variant_generator_last_build_log = Set(None);
    }
}

pub async fn clone_game(
    State(st): State<SharedState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<i32>,
    Json(model): Json<GameCloneModel>,
) -> AppResult<RequestResponse<i32>> {
    let title = validate_clone_request(&model)?;
    let request_digest = clone_request_digest(id, &model, &title);
    if let Some(existing) = existing_clone_operation(st.pg(), model.operation_id).await? {
        if let Some(destination) = replay_clone_operation(existing, id, admin.id, &request_digest)?
        {
            return Ok(RequestResponse::ok(destination));
        }
    }

    let mut source_control = crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?;
    sqlx::query("SET LOCAL statement_timeout = '20s'")
        .execute(&mut **source_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let (source_fingerprint, source_revision, challenge_revision, challenge_count, flag_count): (
        String,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        r#"SELECT md5(
                      row_to_json(source)::text || COALESCE((
                          SELECT string_agg(row_to_json(challenge)::text, '' ORDER BY challenge.id)
                            FROM "GameChallenges" challenge
                           WHERE challenge.game_id = source.id
                      ), '')
                  ), source.configuration_revision,
                  source.challenge_configuration_revision,
                  (SELECT COUNT(*) FROM "GameChallenges" WHERE game_id = source.id),
                  (SELECT COUNT(*)
                     FROM "FlagContexts" flag
                     JOIN "GameChallenges" challenge ON challenge.id = flag.challenge_id
                    WHERE challenge.game_id = source.id)
             FROM "Games" source
            WHERE source.id = $1"#,
    )
    .bind(id)
    .fetch_optional(&mut **source_control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .ok_or_else(|| AppError::not_found("Game not found"))?;
    validate_source_revisions(&model, source_revision, challenge_revision)?;
    if model.include_challenges
        && (challenge_count > MAX_CLONE_CHALLENGES || flag_count > MAX_CLONE_FLAGS)
    {
        return Err(AppError::bad_request(
            "Source exceeds the clone limit of 500 challenges or 5000 flags",
        ));
    }
    if model.include_challenges {
        let definitions = sqlx::query_as::<_, (i32, i16, Option<String>)>(
            r#"SELECT id, "Type", flag_template
                 FROM "GameChallenges"
                WHERE game_id = $1
                ORDER BY id"#,
        )
        .bind(id)
        .fetch_all(&mut **source_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        for (challenge_id, challenge_type, template) in definitions {
            if challenge_type == ChallengeType::DynamicContainer as i16 {
                if let Some(template) = template.as_deref() {
                    crate::utils::flag_policy::validate_dynamic_template(template).map_err(
                        |error| {
                            AppError::conflict(format!(
                                "Source challenge {challenge_id} has an invalid flag template: {error}"
                            ))
                        },
                    )?;
                }
            }
        }
        let flags = sqlx::query_as::<_, (i32, String)>(
            r#"SELECT challenge.id, flag.flag
                 FROM "GameChallenges" challenge
                 JOIN "FlagContexts" flag ON flag.challenge_id = challenge.id
                WHERE challenge.game_id = $1
                ORDER BY challenge.id, flag.id"#,
        )
        .bind(id)
        .fetch_all(&mut **source_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        for (challenge_id, flag) in flags {
            crate::utils::flag_policy::validate_normal(&flag).map_err(|error| {
                AppError::conflict(format!(
                    "Source challenge {challenge_id} has an invalid static flag: {error}"
                ))
            })?;
        }
    }
    let inserted = sqlx::query(
        r#"INSERT INTO "GameCloneOperations" (
               operation_id, source_game_id, requested_by, request_digest,
               source_revision, status
           ) VALUES ($1, $2, $3, $4, $5, 0)
           ON CONFLICT (operation_id) DO NOTHING"#,
    )
    .bind(model.operation_id)
    .bind(id)
    .bind(admin.id)
    .bind(&request_digest)
    .bind(&source_fingerprint)
    .execute(&mut **source_control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    if inserted == 0 {
        let existing = sqlx::query_as::<_, CloneOperationRow>(
            r#"SELECT source_game_id, requested_by, request_digest,
                      destination_game_id, status
                 FROM "GameCloneOperations" WHERE operation_id = $1"#,
        )
        .bind(model.operation_id)
        .fetch_one(&mut **source_control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        let destination = replay_clone_operation(existing, id, admin.id, &request_digest)?
            .ok_or_else(|| AppError::conflict("Clone operation is already running"))?;
        source_control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        return Ok(RequestResponse::ok(destination));
    }

    let (public_key, private_key) = crate::utils::crypto_utils::generate_game_keypair();
    let new_game_id: i32 = sqlx::query_scalar(
        r#"INSERT INTO "Games" (
               title, public_key, private_key, summary, content, practice_mode,
               accept_without_review, allow_user_submissions, writeup_required,
               writeup_note, team_member_count_limit, container_count_limit,
               blood_bonus_value, start_time_utc, end_time_utc, writeup_deadline,
               hidden, ad_allow_snapshot_download, ad_epoch_ticks,
               koth_epoch_ticks, koth_cycle_ticks, koth_champion_cooldown_ticks,
               koth_claim_confirmation_ticks, ad_warmup_seconds,
               ad_snapshot_retention_days, ad_tick_seconds,
               ad_flag_lifetime_ticks, ad_getflag_window_fraction,
               ad_min_grace_period_seconds, ad_reset_cooldown_minutes,
               ad_scoring_start_round, koth_scoring_start_round,
               ad_scoring_paused, vpn_access_required,
               vpn_behavior_telemetry_enabled, vpn_flag_scan_enabled,
               vpn_provider_dns_telemetry_enabled, vpn_source_asn_telemetry_enabled,
               vpn_device_sharing_telemetry_enabled
           )
           SELECT $2, $3, $4, source.summary, source.content,
                  source.practice_mode, source.accept_without_review, FALSE,
                  source.writeup_required, source.writeup_note,
                  source.team_member_count_limit, source.container_count_limit,
                  source.blood_bonus_value, $5, $6, $7, TRUE, TRUE,
                  source.ad_epoch_ticks, source.koth_epoch_ticks,
                  source.koth_cycle_ticks, source.koth_champion_cooldown_ticks,
                  source.koth_claim_confirmation_ticks, source.ad_warmup_seconds,
                  source.ad_snapshot_retention_days, source.ad_tick_seconds,
                  CASE WHEN source.ad_flag_lifetime_ticks IS NULL THEN NULL
                       ELSE LEAST(50, GREATEST(1, source.ad_flag_lifetime_ticks)) END,
                  source.ad_getflag_window_fraction,
                  source.ad_min_grace_period_seconds,
                  source.ad_reset_cooldown_minutes, NULL, NULL, FALSE,
                  source.vpn_access_required,
                  source.vpn_behavior_telemetry_enabled,
                  source.vpn_flag_scan_enabled,
                  source.vpn_provider_dns_telemetry_enabled,
                  source.vpn_source_asn_telemetry_enabled,
                  source.vpn_device_sharing_telemetry_enabled
             FROM "Games" source WHERE source.id = $1
         RETURNING id"#,
    )
    .bind(id)
    .bind(&title)
    .bind(public_key)
    .bind(private_key)
    .bind(model.start_time_utc)
    .bind(model.end_time_utc)
    .bind(super::super::epoch())
    .fetch_one(&mut **source_control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    if model.include_challenges {
        let copied: (i64, i64) = sqlx::query_as(CLONE_CHALLENGES_SQL)
            .bind(id)
            .bind(new_game_id)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(ChallengeBuildStatus::None as i16)
            .bind(ScoreCurve::Standard as i16)
            .bind(NetworkMode::Open as i16)
            .bind(ChallengeVariantMode::Disabled as i16)
            .fetch_one(&mut **source_control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        if copied != (challenge_count, flag_count) {
            return Err(AppError::internal("Clone row-count integrity check failed"));
        }
    }
    sqlx::query(
        r#"UPDATE "GameCloneOperations"
              SET destination_game_id = $2, status = 1,
                  completed_at_utc = clock_timestamp()
            WHERE operation_id = $1 AND status = 0"#,
    )
    .bind(model.operation_id)
    .bind(new_game_id)
    .execute(&mut **source_control.transaction_mut())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    source_control
        .release()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(RequestResponse::ok(new_game_id))
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

#[cfg(test)]
mod clone_contract_tests {
    use super::*;

    fn clone_model() -> GameCloneModel {
        GameCloneModel {
            operation_id: Uuid::new_v4(),
            expected_source_revision: 7,
            expected_challenge_revision: 11,
            title: "Clone target".to_string(),
            start_time_utc: Utc::now(),
            end_time_utc: Utc::now() + chrono::Duration::days(1),
            include_challenges: true,
        }
    }

    #[test]
    fn clone_copy_is_set_based_and_maps_flags_through_preallocated_ids() {
        assert!(CLONE_CHALLENGES_SQL.contains("WITH source AS MATERIALIZED"));
        assert!(CLONE_CHALLENGES_SQL.contains("nextval(pg_get_serial_sequence"));
        assert!(CLONE_CHALLENGES_SQL.contains("JOIN \"FlagContexts\""));
        assert!(!CLONE_CHALLENGES_SQL.contains("SELECT *"));
    }

    #[test]
    fn uppercase_compatibility_and_canonical_lowercase_routes_are_registered() {
        let router = include_str!("../mod.rs");
        assert!(router.contains("/api/edit/games/{id}/Clone"));
        assert!(router.contains("/api/edit/games/{id}/clone"));
    }

    #[test]
    fn clone_intent_is_fenced_by_both_observed_source_revisions() {
        let model = clone_model();
        assert!(validate_source_revisions(&model, 7, 11).is_ok());
        assert!(validate_source_revisions(&model, 8, 11).is_err());
        assert!(validate_source_revisions(&model, 7, 12).is_err());
    }

    #[test]
    fn nil_clone_operation_is_rejected_before_database_work() {
        let mut model = clone_model();
        model.operation_id = Uuid::nil();
        assert!(matches!(
            validate_clone_request(&model),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn source_revision_is_part_of_exact_replay_identity() {
        let mut model = clone_model();
        let first = clone_request_digest(1, &model, "Clone target");
        model.expected_challenge_revision += 1;
        assert_ne!(first, clone_request_digest(1, &model, "Clone target"));
    }
}
