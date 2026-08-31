//! Runtime backstops for malformed legacy normal flags. Each check remains in
//! PostgreSQL so an invalid large value is never copied into the grader.

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
