//! Durable KotH crown-cycle lifecycle and qualified capture state.

mod claims;
mod config;
mod cooldown;
mod lifecycle;
mod rollover;
mod state;

pub(crate) use claims::{apply_observation, ClaimObservation, ObservedToken};
pub(crate) use config::{
    snapshot_official_config, valid_crown_shape, validate_crown_shape, CrownShapeError,
};
pub(crate) use lifecycle::{
    drive_cycle_transitions, recover_cycle, recover_ended_cycle_transitions,
};
pub(crate) use state::{cycle_position, select_cycle_champions, CrownPhase};

pub(crate) fn require_recovery_owner(
    role: crate::models::internal::configs::RuntimeRole,
) -> crate::utils::error::AppResult<()> {
    if role.capabilities().round_engine {
        Ok(())
    } else {
        Err(crate::utils::error::AppError::unavailable(
            "KotH recovery must run on the round-engine owner",
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::models::internal::configs::RuntimeRole;

    #[test]
    fn unprivileged_http_roles_cannot_execute_koth_recovery() {
        for role in [RuntimeRole::Web, RuntimeRole::Migrate] {
            let error = super::require_recovery_owner(role).unwrap_err();
            assert_eq!(error.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        }

        for role in [
            RuntimeRole::All,
            RuntimeRole::Control,
            RuntimeRole::Engine,
            RuntimeRole::Network,
        ] {
            assert!(super::require_recovery_owner(role).is_ok());
        }
    }
}
