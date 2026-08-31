//! Bounded pre-query admission for administrator-managed bearer tokens.

use std::sync::LazyLock;
use std::time::Duration;

use axum::response::{IntoResponse, Response};

use super::{check_async, too_many_requests, Policy};
use crate::app_state::SharedState;
use crate::services::managed_api_token::VerifiedManagedApiToken;

const QUERY_TIMEOUT: Duration = Duration::from_secs(2);
const CONCURRENCY: usize = 8;
static SLOTS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(CONCURRENCY));

pub(super) async fn authenticate(
    st: &SharedState,
    credential: Option<&str>,
    source_ip: String,
) -> Result<(bool, Option<VerifiedManagedApiToken>), Response> {
    let attempted = credential.is_some_and(crate::services::managed_api_token::looks_managed);
    if !attempted {
        return Ok((false, None));
    }
    if let Err(retry_after) = check_async(Policy::ManagedApiAuthSourceAdmission, source_ip).await {
        return Err(too_many_requests(retry_after));
    }
    let token = credential.expect("attempted managed API token is present");
    if !crate::services::managed_api_token::is_well_formed(token) {
        return Ok((true, None));
    }
    let Ok(_slot) = SLOTS.try_acquire() else {
        return Err(too_many_requests(1));
    };
    match tokio::time::timeout(
        QUERY_TIMEOUT,
        crate::services::managed_api_token::authenticate(st, token),
    )
    .await
    {
        Err(_) => Err(too_many_requests(1)),
        Ok(Ok(token)) => Ok((true, token)),
        Ok(Err(error)) => Err(error.into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_work_is_tightly_bounded() {
        assert_eq!(CONCURRENCY, 8);
        assert!(QUERY_TIMEOUT <= Duration::from_secs(2));
    }
}
