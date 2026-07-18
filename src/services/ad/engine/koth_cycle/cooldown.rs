//! Authoritative-round release of network-enforced champion cooldowns.

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

pub(super) async fn release_expired(
    st: &SharedState,
    game_id: i32,
    round_number: i32,
) -> AppResult<()> {
    let due: bool = sqlx::query_scalar(
        r#"SELECT EXISTS(
             SELECT 1 FROM "KothCycleCooldowns" cooldown
             JOIN "KothCrownCycles" cycle ON cycle.id = cooldown.cycle_id
               WHERE cycle.game_id = $1 AND cooldown.network_enforced = TRUE
                 AND cooldown.network_released_at IS NULL
                 AND cooldown.expires_after_round < $2
           )"#,
    )
    .bind(game_id)
    .bind(round_number)
    .fetch_one(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if !due {
        return Ok(());
    }
    let released = sqlx::query(
        r#"UPDATE "KothCycleCooldowns" cooldown
              SET network_released_at = clock_timestamp()
             FROM "KothCrownCycles" cycle
            WHERE cycle.id = cooldown.cycle_id AND cycle.game_id = $1
              AND cooldown.network_enforced = TRUE
              AND cooldown.network_released_at IS NULL
              AND cooldown.expires_after_round < $2"#,
    )
    .bind(game_id)
    .bind(round_number)
    .execute(st.pg())
    .await
    .map_err(|error| AppError::internal(error.to_string()))?
    .rows_affected();
    // The durable intent mutation must commit before its reconcile ticket. If
    // kernel activation fails, the unacknowledged generation remains pending
    // and the owner retries it fail-closed.
    if released > 0 {
        crate::services::ad_vpn::ensure_hub_and_sync(&st.db).await?;
    }
    Ok(())
}
