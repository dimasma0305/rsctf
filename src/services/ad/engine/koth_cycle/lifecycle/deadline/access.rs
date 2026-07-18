//! Deadline-only revocation of hill capabilities and network cooldowns.

use crate::utils::error::{AppError, AppResult};

/// Make every live capability and cooldown for this hill ineligible before its
/// routable target is changed. The cycle phase and runtime identities are not
/// touched, so a crash still has an exact durable recovery source.
pub(super) async fn persist_deadline_access_revocation(
    connection: &mut sqlx::PgConnection,
    game_id: i32,
    challenge_id: i32,
) -> AppResult<()> {
    let protected_phase = sqlx::query_scalar::<_, String>(
        r#"SELECT owner.phase
             FROM "KothCycleCooldowns" cooldown
             JOIN "KothCrownCycles" owner ON owner.id = cooldown.cycle_id
            WHERE owner.game_id = $1 AND owner.challenge_id = $2
              AND owner.phase IN ('Active','CooldownReleasePending','Completed','Ended')
              AND cooldown.network_enforced = FALSE
              AND cooldown.network_enforced_at IS NULL
              AND cooldown.network_released_at IS NULL
            LIMIT 1"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    if let Some(phase) = protected_phase {
        return Err(AppError::conflict(format!(
            "KotH {phase} cycle has a cooldown without network enforcement evidence"
        )));
    }

    sqlx::query(
        r#"UPDATE "KothTokens" token
              SET revoked_at = COALESCE(token.revoked_at, clock_timestamp())
             FROM "KothCrownCycles" owner
            WHERE owner.id = token.cycle_id
              AND owner.game_id = $1 AND owner.challenge_id = $2
              AND token.revoked_at IS NULL"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(&mut *connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;

    // A missing enforcement receipt means no network policy was installed.
    // Terminal cleanup removes only pre-activation intent instead of inventing
    // evidence for a cycle that reached a scoring phase.
    sqlx::query(
        r#"WITH abandoned AS (
             DELETE FROM "KothCycleCooldowns" cooldown
              USING "KothCrownCycles" owner
              WHERE owner.id = cooldown.cycle_id
                AND owner.game_id = $1 AND owner.challenge_id = $2
                AND owner.phase IN (
                  'FinalizePending','SnapshotPending','DestroyPending','CreatePending',
                  'PublishPending','CapabilityPending','ReadinessPending','FirewallPending','Failed'
                )
                AND cooldown.network_enforced = FALSE
                AND cooldown.network_enforced_at IS NULL
                AND cooldown.network_released_at IS NULL
              RETURNING cooldown.cycle_id
           )
           UPDATE "KothCycleCooldowns" cooldown
              SET network_released_at = COALESCE(cooldown.network_released_at, clock_timestamp())
             FROM "KothCrownCycles" owner
            WHERE owner.id = cooldown.cycle_id
              AND owner.game_id = $1 AND owner.challenge_id = $2
              AND cooldown.network_released_at IS NULL
              AND cooldown.network_enforced_at IS NOT NULL"#,
    )
    .bind(game_id)
    .bind(challenge_id)
    .execute(connection)
    .await
    .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
