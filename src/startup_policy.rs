//! Runtime-role ownership rules for startup and background reconciliation.

use rsctf::models::internal::configs::RuntimeRole;

pub(super) fn owns_suspicion_reconciliation(role: RuntimeRole) -> bool {
    matches!(
        role,
        RuntimeRole::All | RuntimeRole::Development | RuntimeRole::Control | RuntimeRole::Engine
    )
}

pub(super) fn owns_feed_reconciliation(role: RuntimeRole) -> bool {
    matches!(
        role,
        RuntimeRole::All | RuntimeRole::Development | RuntimeRole::Control | RuntimeRole::Engine
    )
}

pub(super) fn owns_mail_reconciliation(role: RuntimeRole) -> bool {
    matches!(
        role,
        RuntimeRole::All | RuntimeRole::Development | RuntimeRole::Control
    )
}

pub(super) fn should_run_migrations(role: RuntimeRole, combined_migrations_disabled: bool) -> bool {
    role == RuntimeRole::Migrate
        || role == RuntimeRole::Development
        || (role == RuntimeRole::All && !combined_migrations_disabled)
}
