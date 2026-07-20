//! Shared validation for jeopardy-style challenge scoring parameters.

use crate::utils::error::{AppError, AppResult};
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

// Keep jeopardy flag changes linearizable with an in-flight submission. Existing
// `FlagContexts` rows can be protected with row locks, but an INSERT has no row to
// lock yet; this challenge-scoped advisory lock closes that phantom-row gap.
// The namespace is the ASCII-ish tag `JFLG` and is intentionally distinct from
// the `(0, challenge_id)` lock used to order blood claims.
//
// Interactive flag CRUD is the only path that adds static flags to an existing
// playable challenge. Import/clone paths populate fresh disabled challenge IDs;
// dynamic container rotation is instead fenced by submit's row locks on the
// exact `GameInstances` + `FlagContexts` pair.
const JEOPARDY_FLAG_LOCK_NAMESPACE: i32 = 0x4a46_4c47;

pub async fn lock_jeopardy_flags_shared(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1, $2)")
        .bind(JEOPARDY_FLAG_LOCK_NAMESPACE)
        .bind(challenge_id)
        .execute(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

pub async fn lock_jeopardy_flags_exclusive(
    connection: &mut sqlx::PgConnection,
    challenge_id: i32,
) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(JEOPARDY_FLAG_LOCK_NAMESPACE)
        .bind(challenge_id)
        .execute(connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// SeaORM-transaction counterpart used by repository upserts, whose enum-rich
/// loaded-model merge cannot safely be duplicated as a second raw-SQL write.
pub async fn lock_jeopardy_flags_exclusive_orm<C>(
    connection: &C,
    challenge_id: i32,
) -> AppResult<()>
where
    C: ConnectionTrait,
{
    connection
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT pg_advisory_xact_lock($1, $2)",
            [JEOPARDY_FLAG_LOCK_NAMESPACE.into(), challenge_id.into()],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}

/// Validate the persisted inputs consumed by the dynamic-score formula.
///
/// Keep this at every ingestion boundary (REST, archive import, repository sync)
/// and back it with database CHECK constraints. The formula assumes a non-negative
/// base score, a floor in `[0, 1]`, a finite positive difficulty, and a
/// non-negative attempt limit (`0` means unlimited).
pub fn validate_challenge_scoring(
    original_score: i32,
    min_score_rate: f64,
    difficulty: f64,
    submission_limit: i32,
) -> AppResult<()> {
    if original_score < 0 {
        return Err(AppError::bad_request(
            "Challenge score must be non-negative.",
        ));
    }
    if !min_score_rate.is_finite() || !(0.0..=1.0).contains(&min_score_rate) {
        return Err(AppError::bad_request(
            "Minimum score rate must be between 0 and 1.",
        ));
    }
    if !difficulty.is_finite() || difficulty <= 0.0 {
        return Err(AppError::bad_request(
            "Challenge difficulty must be a finite number greater than zero.",
        ));
    }
    if submission_limit < 0 {
        return Err(AppError::bad_request(
            "Submission limit must be non-negative.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_challenge_scoring;

    #[test]
    fn valid_scoring_boundaries_are_accepted() {
        assert!(validate_challenge_scoring(0, 0.0, f64::MIN_POSITIVE, 0).is_ok());
        assert!(validate_challenge_scoring(i32::MAX, 1.0, f64::MAX, i32::MAX).is_ok());
    }

    #[test]
    fn invalid_scoring_inputs_are_rejected() {
        assert!(validate_challenge_scoring(-1, 0.25, 5.0, 0).is_err());
        assert!(validate_challenge_scoring(100, -0.01, 5.0, 0).is_err());
        assert!(validate_challenge_scoring(100, 1.01, 5.0, 0).is_err());
        assert!(validate_challenge_scoring(100, f64::NAN, 5.0, 0).is_err());
        assert!(validate_challenge_scoring(100, 0.25, 0.0, 0).is_err());
        assert!(validate_challenge_scoring(100, 0.25, f64::INFINITY, 0).is_err());
        assert!(validate_challenge_scoring(100, 0.25, 5.0, -1).is_err());
    }
}
