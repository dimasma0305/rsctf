//! Runtime backstops for malformed legacy normal flags, and per-participation
//! variant grading. Each check remains in PostgreSQL so an invalid large value
//! is never copied into the grader.

use crate::utils::crypto_utils::ct_eq;
use crate::utils::enums::AnswerResult;
use crate::utils::error::{AppError, AppResult};

pub(super) async fn ensure_flag_contexts(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
) -> AppResult<()> {
    let has_invalid: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1 FROM "FlagContexts"
                WHERE challenge_id = $1
                  AND NOT (
                      OCTET_LENGTH(flag) BETWEEN 1 AND $2
                      AND NOT rsctf_flag_has_boundary_whitespace(flag)
                  )
           )"#,
    )
    .bind(challenge_id)
    .bind(i32::try_from(crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES).unwrap_or(127))
    .fetch_one(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if has_invalid {
        tracing::warn!(challenge_id, "invalid legacy flag blocked during grading");
        return Err(AppError::unavailable(
            "Challenge has an invalid flag definition; ask an administrator to repair it",
        ));
    }
    Ok(())
}

pub(super) async fn ensure_variants(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let has_invalid: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1 FROM "ChallengeVariants"
                WHERE game_id = $1 AND challenge_id = $2
                  AND frozen_at_utc IS NOT NULL
                  AND (
                      jsonb_typeof(manifest->'flag') IS DISTINCT FROM 'string'
                      OR NOT (
                          OCTET_LENGTH(manifest->>'flag') BETWEEN 1 AND $3
                          AND NOT rsctf_flag_has_boundary_whitespace(manifest->>'flag')
                      )
                  )
           )"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .bind(i32::try_from(crate::utils::flag_policy::NORMAL_FLAG_MAX_BYTES).unwrap_or(127))
    .fetch_one(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if has_invalid {
        tracing::warn!(
            game_id,
            challenge_id,
            "invalid legacy variant flag blocked during grading"
        );
        return Err(AppError::unavailable(
            "Challenge variant has an invalid flag; ask an administrator to repair it",
        ));
    }
    Ok(())
}

/// A team's own variant flag always wins, so a generator that repeated a flag
/// can never turn a team's own answer into `CheatDetected`.
pub(super) fn variant_verdict(
    variants: &[(i32, String)],
    participation_id: i32,
    answer: &str,
) -> (AnswerResult, Option<i32>) {
    if variants
        .iter()
        .any(|(owner, flag)| *owner == participation_id && ct_eq(flag, answer))
    {
        return (AnswerResult::Accepted, None);
    }
    variants
        .iter()
        .find(|(_, flag)| ct_eq(flag, answer))
        .map_or((AnswerResult::WrongAnswer, None), |(owner, _)| {
            (AnswerResult::CheatDetected, Some(*owner))
        })
}

pub(super) async fn grade_variant_answer(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    game_id: i32,
    challenge_id: i32,
    participation_id: i32,
    answer: &str,
) -> AppResult<(AnswerResult, Option<i32>)> {
    ensure_variants(transaction, game_id, challenge_id).await?;
    let variants = sqlx::query_as::<_, (i32, String)>(
        r#"SELECT participation_id, manifest->>'flag'
             FROM "ChallengeVariants"
            WHERE game_id = $1 AND challenge_id = $2
              AND frozen_at_utc IS NOT NULL
            ORDER BY participation_id"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !variants
        .iter()
        .any(|(candidate, _)| *candidate == participation_id)
    {
        return Err(AppError::unavailable(
            "This participation's deterministic challenge variant is not ready",
        ));
    }
    Ok(variant_verdict(&variants, participation_id, answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_variant_flag_never_flags_its_own_team() {
        let variants = vec![
            (1, "flag{same}".to_string()),
            (2, "flag{same}".to_string()),
            (3, "flag{three}".to_string()),
        ];
        // Team 2 comes second in the ledger order but submits its own flag.
        assert!(matches!(
            variant_verdict(&variants, 2, "flag{same}"),
            (AnswerResult::Accepted, None)
        ));
        assert!(matches!(
            variant_verdict(&variants, 3, "flag{same}"),
            (AnswerResult::CheatDetected, Some(1))
        ));
        assert!(matches!(
            variant_verdict(&variants, 3, "flag{three}"),
            (AnswerResult::Accepted, None)
        ));
        assert!(matches!(
            variant_verdict(&variants, 3, "flag{none}"),
            (AnswerResult::WrongAnswer, None)
        ));
    }
}
