//! Shared validation for jeopardy-style challenge scoring parameters.

use crate::utils::error::{AppError, AppResult};
use chrono::{DateTime, Utc};
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

/// New jeopardy challenges start at 1,000 points and decay to a 10-point floor.
/// The persisted model stores that floor as a rate rather than an absolute score.
pub const DEFAULT_JEOPARDY_ORIGINAL_SCORE: i32 = 1_000;
pub const DEFAULT_JEOPARDY_MIN_SCORE_POINTS: i32 = 10;
pub const DEFAULT_JEOPARDY_MIN_SCORE_RATE: f64 =
    DEFAULT_JEOPARDY_MIN_SCORE_POINTS as f64 / DEFAULT_JEOPARDY_ORIGINAL_SCORE as f64;
pub const DEFAULT_JEOPARDY_DIFFICULTY: f64 = 5.0;
pub const DEFAULT_CHALLENGE_SUBMISSION_LIMIT: i32 = 0;

/// Whether a public scoreboard is currently hiding post-freeze evidence.
/// Event end is still an immutable evidence cutoff, but it is not a "frozen
/// view": once the event ends the final public result is revealed.
pub fn public_scoreboard_frozen(
    freeze: Option<DateTime<Utc>>,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
    is_monitor: bool,
) -> bool {
    !is_monitor && freeze.is_some_and(|freeze| now >= freeze && now < end)
}

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

/// Largest factor the field-best normalization may apply to one Attack & Defense
/// service or King of the Hill hill. A challenge whose best event-average score is
/// below `100 / MAX_FIELD_BEST_MULTIPLIER` points is not inflated to a full 100,
/// so a hill or service nobody meaningfully played cannot hand its whole budget to
/// the first team that touches it.
pub const MAX_FIELD_BEST_MULTIPLIER: f64 = 4.0;

/// Multiplier that maps the field's best event-average score on one engine
/// challenge onto the fixed 100-point ceiling, capped at
/// [`MAX_FIELD_BEST_MULTIPLIER`]. A field with no positive score keeps the
/// neutral factor `1`, and a best above 100 (float noise) is never scaled down.
pub fn field_best_multiplier(field_best: f64) -> f64 {
    if !field_best.is_finite() || field_best <= 0.0 {
        return 1.0;
    }
    (100.0 / field_best).clamp(1.0, MAX_FIELD_BEST_MULTIPLIER)
}

/// Scale one team's event-average challenge score by the field-best multiplier
/// and keep the result inside `[0, 100]`.
pub fn normalize_to_field_best(points: f64, field_best: f64) -> f64 {
    if !points.is_finite() {
        return 0.0;
    }
    (points * field_best_multiplier(field_best)).clamp(0.0, 100.0)
}

#[cfg(test)]
mod field_best_tests {
    use super::{field_best_multiplier, normalize_to_field_best, MAX_FIELD_BEST_MULTIPLIER};

    fn close(left: f64, right: f64) {
        assert!(
            (left - right).abs() < 1e-12,
            "expected {left} to equal {right}"
        );
    }

    #[test]
    fn field_best_maps_to_the_full_ceiling() {
        close(field_best_multiplier(80.0), 1.25);
        close(normalize_to_field_best(80.0, 80.0), 100.0);
        close(normalize_to_field_best(40.0, 80.0), 50.0);
    }

    #[test]
    fn low_field_best_is_capped_not_inflated_to_full_credit() {
        close(field_best_multiplier(25.0), MAX_FIELD_BEST_MULTIPLIER);
        close(field_best_multiplier(7.8), MAX_FIELD_BEST_MULTIPLIER);
        close(normalize_to_field_best(7.8, 7.8), 31.2);
        close(normalize_to_field_best(1.0, 7.8), 4.0);
    }

    #[test]
    fn empty_or_saturated_fields_stay_neutral_and_bounded() {
        close(field_best_multiplier(0.0), 1.0);
        close(field_best_multiplier(-3.0), 1.0);
        close(field_best_multiplier(f64::NAN), 1.0);
        close(field_best_multiplier(100.0), 1.0);
        close(field_best_multiplier(120.0), 1.0);
        close(normalize_to_field_best(0.0, 0.0), 0.0);
        close(normalize_to_field_best(f64::NAN, 50.0), 0.0);
        close(normalize_to_field_best(150.0, 50.0), 100.0);
        close(normalize_to_field_best(-1.0, 50.0), 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        public_scoreboard_frozen, validate_challenge_scoring, DEFAULT_JEOPARDY_MIN_SCORE_POINTS,
        DEFAULT_JEOPARDY_MIN_SCORE_RATE, DEFAULT_JEOPARDY_ORIGINAL_SCORE,
    };

    #[test]
    fn default_jeopardy_floor_is_ten_points() {
        assert_eq!(
            (DEFAULT_JEOPARDY_ORIGINAL_SCORE as f64 * DEFAULT_JEOPARDY_MIN_SCORE_RATE).floor()
                as i32,
            DEFAULT_JEOPARDY_MIN_SCORE_POINTS
        );
    }

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

    #[test]
    fn public_freeze_is_live_only_and_reveals_at_event_end() {
        let end = chrono::Utc::now();
        let freeze = end - chrono::Duration::minutes(30);
        assert!(public_scoreboard_frozen(
            Some(freeze),
            end,
            end - chrono::Duration::minutes(1),
            false,
        ));
        assert!(!public_scoreboard_frozen(Some(freeze), end, end, false));
        assert!(!public_scoreboard_frozen(
            Some(freeze),
            end,
            end - chrono::Duration::minutes(1),
            true,
        ));
    }
}
