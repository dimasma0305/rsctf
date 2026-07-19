//! First-administrator bootstrap authorization.

use sea_orm::DatabaseTransaction;
use sha2::{Digest, Sha256};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};

const TOKEN_ENV: &str = "RSCTF_BOOTSTRAP_TOKEN";
const DENIED: &str = "Bootstrap registration is unavailable";
const MIN_TOKEN_BYTES: usize = 32;

/// Cheap fail-fast check before captcha, validation, and password hashing. The
/// lock-protected check in `register` remains authoritative.
pub(super) async fn preflight(st: &SharedState, supplied: Option<&str>) -> AppResult<bool> {
    let is_first: bool = sqlx::query_scalar(r#"SELECT NOT EXISTS (SELECT 1 FROM "AspNetUsers")"#)
        .fetch_one(st.pg())
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    require(is_first, supplied)?;
    Ok(is_first)
}

/// Later registrations retain their existing behavior and never inspect the
/// optional token. All bootstrap failures intentionally share one response.
pub(super) fn require(is_first: bool, supplied: Option<&str>) -> AppResult<()> {
    if !is_first {
        return Ok(());
    }
    let configured = std::env::var(TOKEN_ENV).ok();
    if matches(configured.as_deref(), supplied) {
        Ok(())
    } else {
        Err(AppError::bad_request(DENIED))
    }
}

/// Authoritative check made while the registration advisory transaction is
/// live. Explicit rollback avoids returning a failed bootstrap transaction to
/// cleanup implicitly.
pub(super) async fn recheck(
    transaction: DatabaseTransaction,
    is_first: bool,
    supplied: Option<&str>,
) -> AppResult<DatabaseTransaction> {
    if let Err(error) = require(is_first, supplied) {
        transaction.rollback().await?;
        return Err(error);
    }
    Ok(transaction)
}

/// Compare fixed-width hashes and original lengths without a secret-dependent
/// early exit. The length fold keeps the authorization an exact string match.
fn matches(configured: Option<&str>, supplied: Option<&str>) -> bool {
    let configured = configured.unwrap_or_default();
    let supplied = supplied.unwrap_or_default();
    let configured_hash = Sha256::digest(configured.as_bytes());
    let supplied_hash = Sha256::digest(supplied.as_bytes());
    let mut difference = configured.len() ^ supplied.len();
    for (&left, &right) in configured_hash.iter().zip(supplied_hash.iter()) {
        difference |= usize::from(left ^ right);
    }

    (configured.len() >= MIN_TOKEN_BYTES) & (difference == 0)
}

#[cfg(test)]
mod tests {
    use super::{matches, require};

    #[test]
    fn bootstrap_match_requires_a_configured_exact_token() {
        const TOKEN: &str = "0123456789abcdef0123456789abcdef";
        assert!(matches(Some(TOKEN), Some(TOKEN)));
        assert!(!matches(None, Some(TOKEN)));
        assert!(!matches(Some(""), Some("")));
        assert!(!matches(Some(TOKEN), None));
        assert!(!matches(Some(TOKEN), Some("wrong")));
        assert!(!matches(
            Some(TOKEN),
            Some("0123456789abcdef0123456789abcdef ")
        ));
        assert!(!matches(
            Some(TOKEN),
            Some("0123456789ABCDEF0123456789ABCDEF")
        ));
        assert!(!matches(
            Some("0123456789abcdef0123456789abcde"),
            Some("0123456789abcdef0123456789abcde")
        ));
    }

    #[test]
    fn established_installation_ignores_the_bootstrap_field() {
        assert!(require(false, None).is_ok());
        assert!(require(false, Some("arbitrary-client-value")).is_ok());
    }
}
