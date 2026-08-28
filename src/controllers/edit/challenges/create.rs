use super::mutation_recovery::{
    claim_challenge_create_operation, complete_challenge_create_operation, INSERTABLE_GAME_SQL,
};
use super::*;

/// `POST /api/edit/games/{id}/challenges`
pub async fn add_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path(id): Path<i32>,
    Json(model): Json<ChallengeInfoModel>,
) -> AppResult<RequestResponse<ChallengeEditDetailModel>> {
    manager_or_admin(&st, &user, id).await?;
    let operation_id =
        crate::services::create_operations::require_operation_id(model.operation_id)?;
    load_game(&st, id).await?;

    // Every challenge kind shares the game deletion/control domain. A game
    // whose hard-delete fence committed must not gain a new child while its
    // external teardown is running.
    let mut engine_control =
        Some(crate::services::ad_engine::acquire_ad_game_lock(&st.db, id).await?);
    let control = engine_control
        .as_mut()
        .expect("new challenge holds the game control lock");
    let game_accepts_children = sqlx::query_scalar::<_, bool>(INSERTABLE_GAME_SQL)
        .bind(id)
        .fetch_optional(&mut **control.transaction_mut())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Game not found"))?;
    if !game_accepts_children {
        return Err(AppError::conflict("Game is being deleted"));
    }
    if model.challenge_type == ChallengeType::KingOfTheHill {
        super::super::games::validate_koth_game_shape_locked(control.transaction_mut(), id).await?;
    }

    let request_digest = crate::utils::codec::sha256_str(
        &serde_json::to_string(&(model.title.as_str(), model.category, model.challenge_type))
            .map_err(|error| AppError::internal(error.to_string()))?,
    );
    let replay_id = claim_challenge_create_operation(
        control.transaction_mut(),
        user.id,
        id,
        operation_id,
        &request_digest,
    )
    .await?;
    let challenge_id = match replay_id {
        Some(challenge_id) => challenge_id,
        None => {
            let challenge_id = sqlx::query_scalar::<_, i32>(
                r#"INSERT INTO "GameChallenges"
                          (game_id, title, content, category, "Type", is_enabled,
                           submission_limit, accepted_count, submission_count,
                           review_status, build_status, original_score, min_score_rate,
                           difficulty, score_curve, network_mode, enable_traffic_capture,
                           enable_shared_container, disable_blood_bonus, ad_allow_egress,
                           ad_allow_self_reset, ad_ssh_requires_flag, ad_self_hosted)
                   VALUES ($1, $2, '', $3, $4, FALSE, $5, 0, 0, $6, $7, $8,
                           $9, $10, $11, $12, FALSE, FALSE, TRUE, FALSE, FALSE,
                           FALSE, FALSE)
                   RETURNING id"#,
            )
            .bind(id)
            .bind(&model.title)
            .bind(model.category as i16)
            .bind(model.challenge_type as i16)
            .bind(crate::utils::scoring::DEFAULT_CHALLENGE_SUBMISSION_LIMIT)
            .bind(ChallengeReviewStatus::Active as i16)
            .bind(ChallengeBuildStatus::None as i16)
            .bind(crate::utils::scoring::DEFAULT_JEOPARDY_ORIGINAL_SCORE)
            .bind(crate::utils::scoring::DEFAULT_JEOPARDY_MIN_SCORE_RATE)
            .bind(crate::utils::scoring::DEFAULT_JEOPARDY_DIFFICULTY)
            .bind(ScoreCurve::Standard as i16)
            .bind(NetworkMode::Open as i16)
            .fetch_one(&mut **control.transaction_mut())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            seed_division_configs(control.transaction_mut(), id, challenge_id).await?;
            complete_challenge_create_operation(
                control.transaction_mut(),
                user.id,
                id,
                operation_id,
                challenge_id,
            )
            .await?;
            challenge_id
        }
    };
    if let Some(control) = engine_control {
        control
            .release()
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
    }
    let created = load_challenge(&st, id, challenge_id).await?;
    flush_game_scoreboards(&st, id).await;
    Ok(RequestResponse::ok(
        ChallengeEditDetailModel::from_challenge(&st, &created, Vec::new()).await?,
    ))
}
