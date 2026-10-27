//! Durable team-profile projection effects.

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use uuid::Uuid;
const INVALIDATION_GAME_PAGE: i64 = 32;
const INVALIDATION_TEAM_BATCH: i64 = 4;

#[derive(sqlx::FromRow)]
struct PendingInvalidation {
    team_id: i32,
    profile_revision: i64,
    after_game_id: i32,
}

/// Apply a bounded page of durable profile invalidations.
pub(crate) async fn process_profile_invalidations(state: &SharedState) -> AppResult<u64> {
    let claim_id = Uuid::new_v4();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let effects = sqlx::query_as::<_, PendingInvalidation>(
        r#"WITH candidates AS (
               SELECT team_id FROM "TeamProfileInvalidations"
                WHERE claim_expires_at_utc IS NULL
                   OR claim_expires_at_utc <= CURRENT_TIMESTAMP
                ORDER BY updated_at_utc, team_id
                FOR UPDATE SKIP LOCKED LIMIT $2
           )
           UPDATE "TeamProfileInvalidations" effect
              SET claim_id = $1,
                  claim_expires_at_utc = CURRENT_TIMESTAMP + INTERVAL '2 minutes'
             FROM candidates
            WHERE effect.team_id = candidates.team_id
        RETURNING effect.team_id, effect.profile_revision, effect.after_game_id"#,
    )
    .bind(claim_id)
    .bind(INVALIDATION_TEAM_BATCH)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut processed = 0;
    for effect in effects {
        let game_ids = sqlx::query_scalar::<_, i32>(
            r#"SELECT DISTINCT game_id FROM "Participations"
                WHERE team_id = $1 AND game_id > $2
                ORDER BY game_id LIMIT $3"#,
        )
        .bind(effect.team_id)
        .bind(effect.after_game_id)
        .bind(INVALIDATION_GAME_PAGE)
        .fetch_all(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        crate::services::scoreboard_invalidation::flush_scoreboards_for_games(state, &game_ids)
            .await;

        if game_ids.len() < INVALIDATION_GAME_PAGE as usize {
            let deleted = sqlx::query(
                r#"DELETE FROM "TeamProfileInvalidations"
                    WHERE team_id = $1 AND profile_revision = $2 AND claim_id = $3"#,
            )
            .bind(effect.team_id)
            .bind(effect.profile_revision)
            .bind(claim_id)
            .execute(state.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
            processed += deleted.rows_affected();
        } else if let Some(last_game_id) = game_ids.last() {
            sqlx::query(
                r#"UPDATE "TeamProfileInvalidations"
                      SET after_game_id = $3, claim_id = NULL, claim_expires_at_utc = NULL
                    WHERE team_id = $1 AND profile_revision = $2 AND claim_id = $4"#,
            )
            .bind(effect.team_id)
            .bind(effect.profile_revision)
            .bind(last_game_id)
            .bind(claim_id)
            .execute(state.pg())
            .await
            .map_err(|error| AppError::internal(error.to_string()))?;
        }
    }
    sqlx::query(
        r#"DELETE FROM "TeamProfileOperations" WHERE operation_id IN (
               SELECT operation_id FROM "TeamProfileOperations"
                WHERE created_at_utc < CURRENT_TIMESTAMP - INTERVAL '7 days'
                ORDER BY created_at_utc LIMIT 256
           )"#,
    )
    .execute(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(processed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalidation_pass_is_bounded() {
        const { assert!(super::INVALIDATION_TEAM_BATCH * super::INVALIDATION_GAME_PAGE <= 128) };
    }
}
