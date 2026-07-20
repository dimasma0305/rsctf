//! Jeopardy grading/scoring invariants for repository synchronization.

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseTransaction, Statement};

use crate::models::data::game_challenge;
use crate::utils::enums::ScoreCurve;
use crate::utils::error::{AppError, AppResult};

pub(super) struct GradingIntent<'a> {
    pub submission_limit: i32,
    pub disable_blood_bonus: bool,
    pub original_score: i32,
    pub min_score_rate: f64,
    pub difficulty: f64,
    pub score_curve: ScoreCurve,
    pub flag_template: Option<&'a str>,
    pub static_flags: &'a [String],
}

pub(super) struct GradingFence {
    pub protected: bool,
    pub update_deferred: bool,
}

async fn stored_static_flag_policy_locked(
    transaction: &DatabaseTransaction,
    challenge_id: i32,
) -> AppResult<Vec<String>> {
    let rows = transaction
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT flag.flag
                 FROM "FlagContexts" flag
                WHERE flag.challenge_id = $1
                  AND flag.is_occupied = FALSE
                  AND NOT EXISTS (
                        SELECT 1 FROM "GameInstances" instance
                         WHERE instance.flag_id = flag.id
                  )
                ORDER BY flag.flag"#,
            [challenge_id.into()],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut flags = rows
        .into_iter()
        .map(|row| {
            row.try_get::<String>("", "flag")
                .map_err(|error| AppError::internal(error.to_string()))
        })
        .collect::<AppResult<Vec<_>>>()?;
    flags.sort();
    flags.dedup();
    Ok(flags)
}

pub(super) async fn grading_fence_locked(
    transaction: &DatabaseTransaction,
    game_id: i32,
    challenge: Option<&game_challenge::Model>,
    intent: &GradingIntent<'_>,
) -> AppResult<GradingFence> {
    let Some(challenge) = challenge.filter(|challenge| !challenge.challenge_type.uses_ad_engine())
    else {
        return Ok(GradingFence {
            protected: false,
            update_deferred: false,
        });
    };
    let row = transaction
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"SELECT (
                       game.start_time_utc <= clock_timestamp()
                       OR challenge.accepted_count > 0
                       OR challenge.submission_count > 0
                       OR EXISTS (
                             SELECT 1 FROM "Submissions" submission
                              WHERE submission.challenge_id = challenge.id
                       )
                       OR EXISTS (
                             SELECT 1 FROM "FirstSolves" solve
                              WHERE solve.challenge_id = challenge.id
                       )
                   ) AS protected
                 FROM "Games" game
                 JOIN "GameChallenges" challenge
                   ON challenge.game_id = game.id
                WHERE game.id = $1 AND challenge.id = $2"#,
            [game_id.into(), challenge.id.into()],
        ))
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .ok_or_else(|| AppError::not_found("Challenge not found"))?;
    let protected = row
        .try_get::<bool>("", "protected")
        .map_err(|error| AppError::internal(error.to_string()))?;
    if !protected {
        return Ok(GradingFence {
            protected: false,
            update_deferred: false,
        });
    }
    let stored_flags = stored_static_flag_policy_locked(transaction, challenge.id).await?;
    let update_deferred = challenge.submission_limit != intent.submission_limit
        || challenge.disable_blood_bonus != intent.disable_blood_bonus
        || challenge.original_score != intent.original_score
        || challenge.min_score_rate != intent.min_score_rate
        || challenge.difficulty != intent.difficulty
        || challenge.score_curve != intent.score_curve
        || challenge.flag_template.as_deref() != intent.flag_template
        || stored_flags != intent.static_flags;
    Ok(GradingFence {
        protected,
        update_deferred,
    })
}
