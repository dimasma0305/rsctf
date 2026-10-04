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

#[cfg(test)]
mod tests {
    #[test]
    fn challenge_insert_sql_has_one_connection_owned_boundary() {
        let source = include_str!("mutation_sql.rs");
        assert!(!source.contains(concat!("&st", ".db")));
        assert!(!source.contains(concat!("st", ".pg()")));
        assert!(source.contains("INSERT INTO \"GameChallenges\""));
        assert!(!source.contains("UPDATE \"GameChallenges\""));
    }
}
