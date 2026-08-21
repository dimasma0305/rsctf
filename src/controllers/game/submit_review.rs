use super::*;
use sea_orm::ActiveEnum;

/// `POST /api/game/{id}/challenges/{challengeId}/review` — rate a solved challenge.
///
/// Mirrors RSCTF `ReviewChallenge` + `ChallengeReviewRepository.AddOrUpdateReviewAsync`:
/// the caller must be an accepted participant who has solved the challenge, then a
/// `ChallengeReviews` row (keyed on user+challenge) is inserted or updated in place.
const UPSERT_CHALLENGE_REVIEW_SQL: &str = r#"
INSERT INTO "ChallengeReviews"
            (challenge_id, user_id, game_id, rating, comment, submit_time_utc)
     VALUES ($1, $2, $3, $4, $5, $6)
ON CONFLICT (user_id, challenge_id)
DO UPDATE SET game_id = EXCLUDED.game_id,
              rating = EXCLUDED.rating,
              comment = EXCLUDED.comment,
              submit_time_utc = EXCLUDED.submit_time_utc
"#;

fn review_rating_from_wire(value: Option<i32>) -> ReviewRating {
    value
        .and_then(|value| i16::try_from(value).ok())
        .and_then(|value| ReviewRating::try_from_value(&value).ok())
        .unwrap_or(ReviewRating::None)
}

pub async fn review_challenge(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id)): Path<(i32, i32)>,
    axum::Json(model): axum::Json<ChallengeReviewModel>,
) -> AppResult<MessageResponse> {
    let ctx = context_info(&st, &user, id, false).await?;

    let rating = review_rating_from_wire(model.rating);
    if model
        .comment
        .as_ref()
        .is_some_and(|comment| comment.chars().count() > 1_000)
    {
        return Err(AppError::bad_request(
            "Review comment cannot exceed 1000 characters",
        ));
    }
    let comment = model.comment.filter(|comment| !comment.is_empty());

    let mut transaction = crate::utils::database::begin_sqlx_transaction(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    crate::utils::single_flight::acquire_transaction_advisory_lock_shared(
        &mut transaction,
        &crate::services::live_roster::lock_key(ctx.participation.team_id),
    )
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !crate::services::live_roster::participation_caller_is_live_on(
        &mut *transaction,
        user.id,
        &user.security_stamp,
        id,
        ctx.participation.team_id,
        ctx.participation.id,
        true,
    )
    .await?
    {
        return Err(AppError::Forbidden);
    }

    // Re-check challenge ownership and the accepted solve in the same roster-
    // fenced transaction as the review write. A kick that started after the
    // cached play-context lookup therefore wins before any stale review upsert.
    let challenge_exists: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "GameChallenges" WHERE id = $1 AND game_id = $2
           )"#,
    )
    .bind(challenge_id)
    .bind(id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !challenge_exists {
        return Err(AppError::not_found("Challenge not found"));
    }
    let solved: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
               SELECT 1 FROM "Submissions"
                WHERE participation_id = $1
                  AND challenge_id = $2
                  AND status = $3
           )"#,
    )
    .bind(ctx.participation.id)
    .bind(challenge_id)
    .bind(AnswerResult::Accepted as i16)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !solved {
        return Err(AppError::bad_request("You must solve the challenge first."));
    }

    sqlx::query(UPSERT_CHALLENGE_REVIEW_SQL)
        .bind(challenge_id)
        .bind(user.id)
        .bind(id)
        .bind(rating.into_value())
        .bind(comment)
        .bind(Utc::now())
        .execute(&mut *transaction)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;

    Ok(MessageResponse::ok(""))
}

/// `GET /api/game/{id}/challenges/{challengeId}/status/{submitId}`
pub async fn status(
    State(st): State<SharedState>,
    user: CurrentUser,
    Path((id, challenge_id, submit_id)): Path<(i32, i32, i32)>,
) -> AppResult<RequestResponse<AnswerResult>> {
    let sub = submission::Entity::find_by_id(submit_id)
        .one(&st.db)
        .await?
        .filter(|s| s.game_id == id && s.challenge_id == challenge_id && s.user_id == Some(user.id))
        .ok_or_else(|| AppError::not_found("Submission not found"))?;

    // Never reveal cheat detection to the player.
    let visible = match sub.status {
        AnswerResult::CheatDetected => AnswerResult::WrongAnswer,
        other => other,
    };
    Ok(RequestResponse::ok(visible))
}

#[cfg(test)]
mod tests {
    use super::{review_rating_from_wire, ReviewRating, UPSERT_CHALLENGE_REVIEW_SQL};

    #[test]
    fn challenge_review_write_is_an_atomic_upsert() {
        assert!(UPSERT_CHALLENGE_REVIEW_SQL.contains("ON CONFLICT (user_id, challenge_id)"));
        assert!(UPSERT_CHALLENGE_REVIEW_SQL.contains("submit_time_utc = EXCLUDED.submit_time_utc"));
    }

    #[test]
    fn challenge_review_rating_cannot_wrap_into_a_valid_value() {
        assert_eq!(review_rating_from_wire(Some(3)), ReviewRating::Good);
        assert_eq!(review_rating_from_wire(Some(65_537)), ReviewRating::None);
        assert_eq!(review_rating_from_wire(Some(-65_535)), ReviewRating::None);
    }
}
