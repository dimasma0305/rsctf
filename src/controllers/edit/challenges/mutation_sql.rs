//! Challenge writes executed on the caller-owned game-control connection.

use super::*;

pub(super) async fn insert_challenge_locked(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    model: &ChallengeInfoModel,
) -> AppResult<game_challenge::Model> {
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
    .bind(game_id)
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
    .fetch_one(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    load_challenge_locked(connection, game_id, challenge_id).await
}

/// Persist the projected mutable definition without leaving the transaction
/// that owns the game advisory lock. The caller seeds division policy before
/// committing this same connection.
pub(super) async fn update_challenge_locked(
    connection: &mut sqlx::PgConnection,
    projected: &game_challenge::Model,
) -> AppResult<game_challenge::Model> {
    let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(r#"UPDATE "GameChallenges" SET "#);
    {
        let mut set = query.separated(", ");
        set.push("title = ").push_bind_unseparated(&projected.title);
        set.push("content = ")
            .push_bind_unseparated(&projected.content);
        set.push("flag_template = ")
            .push_bind_unseparated(&projected.flag_template);
        set.push("category = ")
            .push_bind_unseparated(projected.category as i16);
        set.push("hints = ")
            .push_bind_unseparated(projected.hints.as_ref().map(sqlx::types::Json));
        set.push("is_enabled = ")
            .push_bind_unseparated(projected.is_enabled);
        set.push("file_name = ")
            .push_bind_unseparated(&projected.file_name);
        set.push("deadline_utc = ")
            .push_bind_unseparated(projected.deadline_utc);
        set.push("submission_limit = ")
            .push_bind_unseparated(projected.submission_limit);
        set.push("container_image = ")
            .push_bind_unseparated(&projected.container_image);
        set.push("build_status = ")
            .push_bind_unseparated(projected.build_status as i16);
        set.push("build_image_digest = ")
            .push_bind_unseparated(&projected.build_image_digest);
        set.push("last_build_log = ")
            .push_bind_unseparated(&projected.last_build_log);
        set.push("memory_limit = ")
            .push_bind_unseparated(projected.memory_limit);
        set.push("cpu_count = ")
            .push_bind_unseparated(projected.cpu_count);
        set.push("storage_limit = ")
            .push_bind_unseparated(projected.storage_limit);
        set.push("expose_port = ")
            .push_bind_unseparated(projected.expose_port);
        set.push("workload_spec = ")
            .push_bind_unseparated(projected.workload_spec.as_ref().map(sqlx::types::Json));
        set.push("original_score = ")
            .push_bind_unseparated(projected.original_score);
        set.push("min_score_rate = ")
            .push_bind_unseparated(projected.min_score_rate);
        set.push("difficulty = ")
            .push_bind_unseparated(projected.difficulty);
        set.push("score_curve = ")
            .push_bind_unseparated(projected.score_curve as i16);
        set.push("enable_traffic_capture = ")
            .push_bind_unseparated(projected.enable_traffic_capture);
        set.push("disable_blood_bonus = ")
            .push_bind_unseparated(projected.disable_blood_bonus);
        set.push("network_mode = ")
            .push_bind_unseparated(projected.network_mode.map(|value| value as i16));
        set.push("enable_shared_container = ")
            .push_bind_unseparated(projected.enable_shared_container);
        set.push("ad_checker_image = ")
            .push_bind_unseparated(&projected.ad_checker_image);
        set.push("ad_allow_egress = ")
            .push_bind_unseparated(projected.ad_allow_egress);
        set.push("ad_allow_self_reset = ")
            .push_bind_unseparated(projected.ad_allow_self_reset);
        set.push("ad_ssh_requires_flag = ")
            .push_bind_unseparated(projected.ad_ssh_requires_flag);
        set.push("ad_self_hosted = ")
            .push_bind_unseparated(projected.ad_self_hosted);
        set.push("ad_scoring_weight = ")
            .push_bind_unseparated(projected.ad_scoring_weight);
        set.push("variant_mode = ")
            .push_bind_unseparated(projected.variant_mode as i16);
        set.push("variant_generator_image = ")
            .push_bind_unseparated(&projected.variant_generator_image);
        set.push("variant_generator_digest = ")
            .push_bind_unseparated(&projected.variant_generator_digest);
        set.push("variant_generator_build_context_subdir = ")
            .push_bind_unseparated(&projected.variant_generator_build_context_subdir);
        set.push("variant_generator_build_status = ")
            .push_bind_unseparated(projected.variant_generator_build_status as i16);
        set.push("variant_generator_last_build_log = ")
            .push_bind_unseparated(&projected.variant_generator_last_build_log);
        set.push("solve_receipt_mode = ")
            .push_bind_unseparated(projected.solve_receipt_mode as i16);
        set.push("receipt_verifier_identity = ")
            .push_bind_unseparated(&projected.receipt_verifier_identity);
    }
    query
        .push(" WHERE id = ")
        .push_bind(projected.id)
        .push(" AND game_id = ")
        .push_bind(projected.game_id)
        .push(" AND deletion_pending = FALSE");
    let updated = query
        .build()
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::conflict("Challenge is being deleted"));
    }
    Ok(projected.clone())
}

#[cfg(test)]
mod tests {
    #[test]
    fn challenge_mutation_sql_has_one_connection_owned_boundary() {
        let source = include_str!("mutation_sql.rs");
        assert!(!source.contains(concat!("&st", ".db")));
        assert!(!source.contains(concat!("st", ".pg()")));
        assert!(source.contains("deletion_pending = FALSE"));
    }
}
