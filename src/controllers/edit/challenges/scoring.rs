use super::*;

pub(super) fn summary_score(
    challenge_type: ChallengeType,
    original_score: i32,
    min_score_rate: f64,
    difficulty: f64,
    score_curve: ScoreCurve,
    eligible_solve_count: i32,
) -> i32 {
    if challenge_type.uses_ad_engine() {
        0
    } else {
        crate::controllers::game::calculate_challenge_score(
            original_score,
            min_score_rate,
            difficulty,
            eligible_solve_count,
            score_curve,
        )
    }
}

/// Distinct-team solve counts under the same eligibility gates as the official
/// scoreboard's dynamic-score fold.
pub(super) async fn eligible_dynamic_solve_counts(
    st: &SharedState,
    game_id: i32,
) -> AppResult<std::collections::HashMap<i32, i32>> {
    let rows: Vec<(i32, i64)> = sqlx::query_as(
        r#"SELECT first_solve.challenge_id, COUNT(*)::bigint
             FROM "FirstSolves" first_solve
             JOIN "Submissions" submission
               ON submission.id = first_solve.submission_id
              AND submission.participation_id = first_solve.participation_id
              AND submission.challenge_id = first_solve.challenge_id
              AND submission.game_id = $1
              AND submission.status = $5
             JOIN "Participations" participation
               ON participation.id = first_solve.participation_id
              AND participation.game_id = $1
              AND participation.status = $3
             JOIN "GameChallenges" challenge
               ON challenge.id = first_solve.challenge_id
              AND challenge.game_id = $1
              AND challenge.is_enabled
              AND challenge.review_status = $2
             JOIN "Games" game ON game.id = challenge.game_id
             LEFT JOIN "Divisions" division
               ON division.id = participation.division_id
              AND division.game_id = participation.game_id
             LEFT JOIN "DivisionChallengeConfigs" permission
               ON permission.division_id = participation.division_id
              AND permission.challenge_id = challenge.id
            WHERE (game.practice_mode OR (
                    submission.submit_time_utc >= game.start_time_utc
                AND submission.submit_time_utc < game.end_time_utc
                  ))
              AND (challenge.deadline_utc IS NULL
                   OR submission.submit_time_utc <= challenge.deadline_utc)
              AND (
                    participation.division_id IS NULL
                    OR (division.id IS NOT NULL
                        AND (COALESCE(permission.permissions,
                                      division.default_permissions, 0) & $4) = $4)
                  )
            GROUP BY first_solve.challenge_id"#,
    )
    .bind(game_id)
    .bind(ChallengeReviewStatus::Active as i16)
    .bind(ParticipationStatus::Accepted as i16)
    .bind(GamePermission::AFFECT_DYNAMIC_SCORE)
    .bind(crate::utils::enums::AnswerResult::Accepted as i16)
    .fetch_all(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|(challenge_id, count)| (challenge_id, i32::try_from(count).unwrap_or(i32::MAX)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::summary_score;
    use crate::utils::enums::{ChallengeType, ScoreCurve};

    #[test]
    fn admin_summary_decays_only_from_the_supplied_eligible_count() {
        let score = |challenge_type, count| {
            summary_score(challenge_type, 1000, 0.25, 5.0, ScoreCurve::Standard, count)
        };
        assert_eq!(score(ChallengeType::StaticAttachment, 0), 1000);
        assert!(score(ChallengeType::StaticAttachment, 10) < 1000);
        assert_eq!(score(ChallengeType::KingOfTheHill, 10), 0);
    }
}
