use crate::utils::error::{AppError, AppResult};

pub(super) const ACCOUNT_ROLE_ELIGIBILITY_FENCE_LOCK_ID: i64 = 5_932_159_163_412_923_205;

/// Hold the shared account-role eligibility fence through the final rebase and
/// score commit. Account role update statements take the matching exclusive
/// lock through the migration-installed trigger. The role
/// transaction commits before any game/roster revocation locks are acquired,
/// so this global fence cannot invert the checker game-lock order.
pub(super) async fn lock_eligibility_fence(connection: &mut sqlx::PgConnection) -> AppResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(ACCOUNT_ROLE_ELIGIBILITY_FENCE_LOCK_ID)
        .execute(&mut *connection)
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    Ok(())
}
