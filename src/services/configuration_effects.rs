//! Durable event-configuration projection effects.

use crate::app_state::SharedState;
use crate::services::scoreboard_invalidation::flush_game_scoreboards;
use crate::utils::error::{AppError, AppResult};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct PendingEffect {
    game_id: i32,
    configuration_revision: i64,
    invalidate_game: bool,
    invalidate_scoreboards: bool,
    invalidate_policy: bool,
}

pub(crate) async fn process_configuration_effects(state: &SharedState) -> AppResult<u64> {
    let claim_id = Uuid::new_v4();
    let mut transaction = crate::utils::database::begin_sqlx_transaction(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let effects = sqlx::query_as::<_, PendingEffect>(
        r#"WITH candidates AS (
               SELECT game_id FROM "GameConfigurationEffects"
                WHERE claim_expires_at_utc IS NULL
                   OR claim_expires_at_utc <= CURRENT_TIMESTAMP
                ORDER BY updated_at_utc, game_id
                FOR UPDATE SKIP LOCKED LIMIT 32
           )
           UPDATE "GameConfigurationEffects" effect
              SET claim_id = $1,
                  claim_expires_at_utc = CURRENT_TIMESTAMP + INTERVAL '2 minutes'
             FROM candidates
            WHERE effect.game_id = candidates.game_id
        RETURNING effect.game_id, effect.configuration_revision,
                  effect.invalidate_game, effect.invalidate_scoreboards,
                  effect.invalidate_policy"#,
    )
    .bind(claim_id)
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    transaction
        .commit()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let mut processed = 0;
    for effect in effects {
        if effect.invalidate_game {
            crate::services::game_cache::invalidate_game_row_cache(effect.game_id);
        }
        if effect.invalidate_policy {
            crate::services::event_security::invalidate_policy(state, effect.game_id).await;
        }
        if effect.invalidate_scoreboards {
            flush_game_scoreboards(state, effect.game_id).await;
            crate::services::anti_cheat::report_cache::invalidate_report(state, effect.game_id)
                .await;
        }
        let deleted = sqlx::query(
            r#"DELETE FROM "GameConfigurationEffects"
                WHERE game_id = $1 AND configuration_revision = $2 AND claim_id = $3"#,
        )
        .bind(effect.game_id)
        .bind(effect.configuration_revision)
        .bind(claim_id)
        .execute(state.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
        processed += deleted.rows_affected();
    }
    sqlx::query(
        r#"DELETE FROM "GameConfigurationOperations"
            WHERE operation_id IN (
                SELECT operation_id FROM "GameConfigurationOperations"
                 WHERE created_at_utc < CURRENT_TIMESTAMP - INTERVAL '7 days'
                 ORDER BY created_at_utc LIMIT 256
            )"#,
    )
    .execute(state.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(processed)
}
