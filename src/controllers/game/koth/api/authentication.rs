//! Public token exchange used by a Leaderboard challenge coordinator.

use axum::extract::{Json, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::app_state::SharedState;
use crate::utils::error::{AppError, AppResult};
use crate::utils::shared::RequestResponse;

/// Keep invalid capability floods from occupying the shared PostgreSQL pool.
/// The serving role derives up to sixteen lookup slots strictly from
/// connections above its deadlock-safe floor. A bounded request queue absorbs
/// short scheduler and query-latency bursts in the maintained
/// 100-authentication/second arena profile without allowing an attacker to
/// accumulate unbounded waiting tasks.
const MAX_DATABASE_LOOKUP_REQUESTS: usize = 128;
const DATABASE_LOOKUP_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
static DATABASE_LOOKUP_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| {
    Semaphore::new(crate::extensions::database::configured_koth_capability_lookup_concurrency())
});
static DATABASE_LOOKUP_REQUEST_SLOTS: LazyLock<Semaphore> =
    LazyLock::new(|| Semaphore::new(MAX_DATABASE_LOOKUP_REQUESTS));

async fn acquire_bounded_database_lookup<'a>(
    request_slots: &'a Semaphore,
    lookup_slots: &'a Semaphore,
    wait_timeout: Duration,
) -> Option<(SemaphorePermit<'a>, SemaphorePermit<'a>)> {
    let request_slot = request_slots.try_acquire().ok()?;
    let lookup_slot = tokio::time::timeout(wait_timeout, lookup_slots.acquire())
        .await
        .ok()?
        .ok()?;
    Some((request_slot, lookup_slot))
}

async fn acquire_database_lookup_slot(
) -> Option<(SemaphorePermit<'static>, SemaphorePermit<'static>)> {
    acquire_bounded_database_lookup(
        &DATABASE_LOOKUP_REQUEST_SLOTS,
        &DATABASE_LOOKUP_SLOTS,
        DATABASE_LOOKUP_WAIT_TIMEOUT,
    )
    .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KothCapabilityAuthenticationRequest {
    token: String,
    game_id: i32,
    challenge_id: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KothCapabilityIdentityModel {
    /// Challenge-local pseudonym. It is deliberately the capability digest so
    /// the managed reporter can map arena evidence without learning RSCTF IDs.
    team_id: String,
    team_name: String,
}

fn validated_token(raw: &str) -> Option<&str> {
    let token = raw.trim();
    crate::services::ad::koth_api_capability::is_well_formed(token).then_some(token)
}

pub async fn authenticate_capability(
    State(st): State<SharedState>,
    Json(request): Json<KothCapabilityAuthenticationRequest>,
) -> AppResult<Response> {
    // Reject ambiguous and attacker-sized values before acquiring PostgreSQL.
    // The service repeats this check as defense in depth for non-HTTP callers.
    let token = validated_token(&request.token).ok_or(AppError::Unauthorized)?;
    let Some((database_request_slot, database_slot)) = acquire_database_lookup_slot().await else {
        return Ok(crate::middlewares::rate_limiter::too_many_requests(1));
    };
    let mut connection = st
        .pg()
        .acquire()
        .await
        .map_err(|error| AppError::internal(error.to_string()))?;
    let identity = crate::services::ad::koth_api_capability::authenticate(
        &mut connection,
        token,
        request.game_id,
        request.challenge_id,
    )
    .await?
    .ok_or(AppError::Unauthorized)?;
    drop(connection);
    drop(database_slot);
    drop(database_request_slot);

    if let Some(response) = crate::middlewares::rate_limiter::admit_koth_capability_auth(
        identity.game_id,
        identity.challenge_id,
        identity.participation_id,
    )
    .await
    {
        return Ok(response);
    }

    Ok(RequestResponse::ok(KothCapabilityIdentityModel {
        team_id: crate::services::ad::koth_api_capability::token_hash_hex(token),
        team_name: identity.team_name,
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::{header, StatusCode};
    use std::time::Duration;
    use tokio::sync::Semaphore;

    use super::{validated_token, KothCapabilityAuthenticationRequest};

    #[test]
    fn scope_and_token_are_all_required_and_unknown_fields_fail() {
        let valid = serde_json::from_str::<KothCapabilityAuthenticationRequest>(
            r#"{"token":"koth_12345678","gameId":7,"challengeId":9}"#,
        );
        assert!(valid.is_ok());
        assert!(serde_json::from_str::<KothCapabilityAuthenticationRequest>(
            r#"{"token":"koth_12345678","gameId":7}"#
        )
        .is_err());
        assert!(serde_json::from_str::<KothCapabilityAuthenticationRequest>(
            r#"{"token":"koth_12345678","gameId":7,"challengeId":9,"teamId":1}"#
        )
        .is_err());
    }

    #[test]
    fn token_shape_is_normalized_before_database_authentication() {
        assert_eq!(
            validated_token("  koth_exampleToken-123456  "),
            Some("koth_exampleToken-123456")
        );
        assert_eq!(validated_token("koth_short"), None);
        assert_eq!(validated_token(&format!("koth_{}", "a".repeat(129))), None);
    }

    #[test]
    fn database_lookup_rejection_is_retryable() {
        let response = crate::middlewares::rate_limiter::too_many_requests(1);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "1");
    }

    #[tokio::test]
    async fn database_lookup_admission_waits_for_a_short_burst() {
        let request_slots = Semaphore::new(2);
        let lookup_slots = Semaphore::new(1);
        let held_lookup = lookup_slots.acquire().await.unwrap();
        let waiting = super::acquire_bounded_database_lookup(
            &request_slots,
            &lookup_slots,
            Duration::from_secs(1),
        );
        tokio::pin!(waiting);

        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        assert_eq!(request_slots.available_permits(), 1);

        drop(held_lookup);
        let permits = waiting.await.expect("short burst should acquire lookup");
        drop(permits);
        assert_eq!(request_slots.available_permits(), 2);
        assert_eq!(lookup_slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn database_lookup_waiters_are_bounded_before_the_lookup_queue() {
        let request_slots = Semaphore::new(1);
        let lookup_slots = Semaphore::new(0);
        let waiting = super::acquire_bounded_database_lookup(
            &request_slots,
            &lookup_slots,
            Duration::from_secs(1),
        );
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );

        assert!(super::acquire_bounded_database_lookup(
            &request_slots,
            &lookup_slots,
            Duration::from_secs(1),
        )
        .await
        .is_none());

        lookup_slots.add_permits(1);
        drop(
            waiting
                .await
                .expect("admitted waiter should acquire lookup"),
        );
    }

    #[tokio::test]
    async fn database_lookup_timeout_releases_request_admission() {
        let request_slots = Semaphore::new(1);
        let lookup_slots = Semaphore::new(0);
        assert!(super::acquire_bounded_database_lookup(
            &request_slots,
            &lookup_slots,
            Duration::from_millis(1),
        )
        .await
        .is_none());

        assert_eq!(request_slots.available_permits(), 1);
    }
}
